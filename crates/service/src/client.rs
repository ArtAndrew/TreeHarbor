use crate::{Error, Result, files, https};
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, StatusCode, header};
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
    time::Duration,
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::UnixStream,
};
use treeharbor_metadata::{Scope, access::SecretToken, commands::MAX_REQUEST_BYTES};
use uuid::Uuid;

pub struct Credential {
    pub scope: Scope,
    pub token: SecretToken,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialFile {
    version: u8,
    tenant: String,
    repository: String,
    token_id: String,
    token: String,
}
impl Credential {
    pub fn load(path: &Path) -> Result<Self> {
        let value: CredentialFile = serde_json::from_slice(&files::read_private(path, 4096)?)
            .map_err(|_| Error::Credential)?;
        if value.version != 1 {
            return Err(Error::Credential);
        }
        let _: Uuid = value.token_id.parse().map_err(|_| Error::Credential)?;
        let repository: Uuid = value.repository.parse().map_err(|_| Error::Credential)?;
        Ok(Self {
            scope: Scope::new(value.tenant, repository).map_err(|_| Error::Credential)?,
            token: SecretToken::parse(&value.token).map_err(|_| Error::Credential)?,
        })
    }
    pub fn request(&self, command: Value) -> Value {
        json!({"version":1,"tenant":self.scope.tenant(),"repository":self.scope.repository().to_string(),"command":command})
    }
}
pub struct Reply {
    pub status: StatusCode,
    pub body: Value,
}

#[derive(Clone, Copy)]
pub(crate) enum Endpoint<'a> {
    Unix(&'a Path),
    Https(&'a https::Client),
}

impl<'a> Endpoint<'a> {
    pub fn local_socket(self) -> Result<&'a Path> {
        match self {
            Self::Unix(path) => Ok(path),
            Self::Https(_) => Err(Error::LocalOperation),
        }
    }
}

const MAX_RESPONSE: usize = 1024 * 1024 + 64 * 1024;
/// One request per connection; neither tokens nor bodies are logged. Transport
/// errors/timeouts cannot prove whether a submitted mutation committed.
pub async fn send(
    socket: &Path,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_to(Endpoint::Unix(socket), token, body, timeout).await
}

pub(crate) async fn send_to(
    endpoint: Endpoint<'_>,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    if matches!(endpoint, Endpoint::Https(_))
        && !treeharbor_metadata::commands::agent_https_command_allowed(&body)
            .map_err(|_| Error::Config)?
    {
        return Err(Error::LocalOperation);
    }
    send_request_to(
        endpoint,
        token,
        "POST",
        "/v1/commands".into(),
        "application/json",
        body,
        timeout,
    )
    .await
}
/// Full canonical frame, not an existence probe or a promise to upload later.
pub async fn send_object(
    socket: &Path,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_uploaded_object(
        Endpoint::Unix(socket),
        credential,
        upload,
        id,
        frame,
        timeout,
        "source",
    )
    .await
}
pub async fn send_change_blob(
    socket: &Path,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_change_blob_to(
        Endpoint::Unix(socket),
        credential,
        upload,
        id,
        frame,
        timeout,
    )
    .await
}

pub(crate) async fn send_change_blob_to(
    endpoint: Endpoint<'_>,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_uploaded_object(endpoint, credential, upload, id, frame, timeout, "changes").await
}
pub async fn send_github_source_object(
    socket: &Path,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_uploaded_object(
        Endpoint::Unix(socket),
        credential,
        upload,
        id,
        frame,
        timeout,
        "github",
    )
    .await
}
async fn send_uploaded_object(
    endpoint: Endpoint<'_>,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
    purpose: &str,
) -> Result<Reply> {
    if frame.len() > treeharbor_core::MAX_PAYLOAD + treeharbor_core::FRAME_HEADER {
        return Err(Error::Size);
    }
    let path = format!(
        "/v1/{}/{}/{}/uploads/{}/objects/{}",
        purpose,
        credential.scope.tenant(),
        credential.scope.repository(),
        upload,
        id
    );
    send_request_to(
        endpoint,
        &credential.token,
        "PUT",
        path,
        "application/octet-stream",
        frame,
        timeout,
    )
    .await
}
pub async fn send_native_prepare(
    socket: &Path,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    send_request(
        socket,
        token,
        "POST",
        "/v1/native/prepare".into(),
        "application/json",
        body,
        timeout,
    )
    .await
}
pub async fn send_seal_accept(
    socket: &Path,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    send_request(
        socket,
        token,
        "POST",
        "/v1/workspaces/seals/accept".into(),
        "application/json",
        body,
        timeout,
    )
    .await
}

pub async fn send_rebase_prepare(
    socket: &Path,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    send_request(
        socket,
        token,
        "POST",
        "/v1/changes/rebase".into(),
        "application/json",
        body,
        timeout,
    )
    .await
}
pub async fn send_proposal(
    socket: &Path,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_proposal_to(Endpoint::Unix(socket), token, body, timeout).await
}

pub(crate) async fn send_proposal_to(
    endpoint: Endpoint<'_>,
    token: &SecretToken,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    send_request_to(
        endpoint,
        token,
        "POST",
        "/v1/changes/submit".into(),
        "application/json",
        body,
        timeout,
    )
    .await
}
async fn send_request(
    socket: &Path,
    token: &SecretToken,
    method: &'static str,
    uri: String,
    content_type: &'static str,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_request_to(
        Endpoint::Unix(socket),
        token,
        method,
        uri,
        content_type,
        body,
        timeout,
    )
    .await
}

async fn send_request_to(
    endpoint: Endpoint<'_>,
    token: &SecretToken,
    method: &'static str,
    uri: String,
    content_type: &'static str,
    body: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    let raw = exchange(
        endpoint,
        token,
        Exchange {
            method,
            uri,
            content_type,
            body,
            max_response: MAX_RESPONSE,
            output: None,
        },
        timeout,
    )
    .await?;
    let body = serde_json::from_slice(&raw.bytes).map_err(|_| Error::Http)?;
    Ok(Reply {
        status: raw.status,
        body,
    })
}
pub async fn history(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    history_to(Endpoint::Unix(socket), credential, fields, timeout).await
}

pub(crate) async fn history_to(
    endpoint: Endpoint<'_>,
    credential: &Credential,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    send_request_to(
        endpoint,
        &credential.token,
        "POST",
        "/v1/history/read".into(),
        "application/json",
        scoped_fields(credential, fields)?,
        timeout,
    )
    .await
}
pub async fn restore_verify_source(
    socket: &Path,
    credential: &Credential,
    mut fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    fields["version"] = json!(1);
    fields["tenant"] = json!(credential.scope.tenant());
    fields["repository"] = json!(credential.scope.repository());
    let body = serde_json::to_vec(&fields).map_err(|_| Error::Config)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    send_request(
        socket,
        &credential.token,
        "POST",
        "/v1/github/restore/verify-source".into(),
        "application/json",
        body,
        timeout,
    )
    .await
}

pub(crate) async fn verify_archive(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    send_request(
        socket,
        &credential.token,
        "POST",
        crate::archive_proof::PATH.into(),
        "application/json",
        scoped_fields(credential, fields)?,
        timeout,
    )
    .await
}

pub(crate) async fn archive_operation(
    socket: &Path,
    credential: &Credential,
    path: &'static str,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    send_request(
        socket,
        &credential.token,
        "POST",
        path.into(),
        "application/json",
        scoped_fields(credential, fields)?,
        timeout,
    )
    .await
}

pub async fn review(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    review_to(Endpoint::Unix(socket), credential, fields, timeout).await
}

pub(crate) async fn review_to(
    endpoint: Endpoint<'_>,
    credential: &Credential,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    send_request_to(
        endpoint,
        &credential.token,
        "POST",
        "/v1/review/diff".into(),
        "application/json",
        scoped_fields(credential, fields)?,
        timeout,
    )
    .await
}

pub(crate) struct RawReply {
    pub status: StatusCode,
    pub headers: hyper::HeaderMap,
    pub bytes: Bytes,
    pub file: Option<std::fs::File>,
}
struct Exchange {
    method: &'static str,
    uri: String,
    content_type: &'static str,
    body: Vec<u8>,
    max_response: usize,
    output: Option<std::fs::File>,
}
async fn exchange(
    endpoint: Endpoint<'_>,
    token: &SecretToken,
    request: Exchange,
    timeout: Duration,
) -> Result<RawReply> {
    tokio::time::timeout(timeout, async {
        match endpoint {
            Endpoint::Unix(socket) => {
                files::private_directory(socket.parent().ok_or(Error::UnsafeFile)?)?;
                let meta = fs::symlink_metadata(socket)?;
                if !meta.file_type().is_socket()
                    || meta.uid() != rustix::process::geteuid().as_raw()
                    || meta.mode() & 0o077 != 0
                {
                    return Err(Error::UnsafeFile);
                }
                let stream = UnixStream::connect(socket).await?;
                exchange_on(stream, "treeharbor.local", token, request).await
            }
            Endpoint::Https(client) => {
                // Construct the HTTP request only after authenticating the TLS peer.
                let stream = client.connect().await?;
                exchange_on(stream, &client.authority, token, request).await
            }
        }
    })
    .await
    .map_err(|_| Error::Timeout)?
}

async fn exchange_on<S>(
    stream: S,
    authority: &str,
    token: &SecretToken,
    request: Exchange,
) -> Result<RawReply>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let Exchange {
        method,
        uri,
        content_type,
        body,
        max_response,
        output,
    } = request;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|_| Error::Http)?;
    // Abort on timeout/cancellation too; dropping a JoinHandle alone detaches it.
    struct Driver(tokio::task::JoinHandle<()>);
    impl Drop for Driver {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _driver = Driver(tokio::spawn(async move {
        let _ = connection.await;
    }));
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, authority)
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONNECTION, "close")
        .body(Full::new(Bytes::from(body)))
        .map_err(|_| Error::Http)?;
    let mut authorization = header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
        .map_err(|_| Error::Credential)?;
    authorization.set_sensitive(true);
    request
        .headers_mut()
        .insert(header::AUTHORIZATION, authorization);
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| Error::Http)?;
    let status = response.status();
    let headers = response.headers().clone();
    if status.is_success()
        && let Some(output) = output
    {
        if status != StatusCode::OK
            || headers
                .get(header::CONTENT_TYPE)
                .is_none_or(|v| v != crate::archive::CONTENT_TYPE)
        {
            return Err(Error::Http);
        }
        let mut file = tokio::fs::File::from_std(output);
        let mut body = response.into_body();
        let mut total = 0u64;
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Error::Http)?;
            let data = frame.into_data().map_err(|_| Error::Http)?;
            total = total.checked_add(data.len() as u64).ok_or(Error::Size)?;
            if total > crate::archive::MAX_ARCHIVE {
                return Err(Error::Size);
            }
            file.write_all(&data).await?;
        }
        file.flush().await?;
        return Ok(RawReply {
            status,
            headers,
            bytes: Bytes::new(),
            file: Some(file.into_std().await),
        });
    }
    let bytes = Limited::new(response.into_body(), max_response)
        .collect()
        .await
        .map_err(|_| Error::Http)?
        .to_bytes();
    Ok(RawReply {
        status,
        headers,
        bytes,
        file: None,
    })
}

/// The scope always comes from the credential file, never caller-supplied fields.
pub(crate) async fn source_read(
    endpoint: Endpoint<'_>,
    credential: &Credential,
    fields: Value,
    file: bool,
    timeout: Duration,
) -> Result<RawReply> {
    let body = scoped_fields(credential, fields)?;
    exchange(
        endpoint,
        &credential.token,
        Exchange {
            method: "POST",
            uri: "/v1/source/read".into(),
            content_type: "application/json",
            body,
            output: None,
            max_response: if file {
                treeharbor_core::MAX_PAYLOAD + treeharbor_core::FRAME_HEADER
            } else {
                MAX_RESPONSE
            },
        },
        timeout,
    )
    .await
}

pub(crate) async fn source_export(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    output: std::fs::File,
    timeout: Duration,
) -> Result<RawReply> {
    export_request(
        socket,
        credential,
        fields,
        output,
        timeout,
        "/v1/source/export",
    )
    .await
}

pub(crate) async fn github_projection_export(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    output: std::fs::File,
    timeout: Duration,
) -> Result<RawReply> {
    export_request(
        socket,
        credential,
        fields,
        output,
        timeout,
        "/v1/github/projections/export",
    )
    .await
}

fn scoped_fields(credential: &Credential, mut fields: Value) -> Result<Vec<u8>> {
    if !fields.is_object() {
        return Err(Error::Config);
    }
    fields["version"] = json!(1);
    fields["tenant"] = json!(credential.scope.tenant());
    fields["repository"] = json!(credential.scope.repository().to_string());
    let body = serde_json::to_vec(&fields).map_err(|_| Error::Config)?;
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    Ok(body)
}

pub(crate) async fn github_projection_prepare(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    timeout: Duration,
) -> Result<Reply> {
    send_request(
        socket,
        &credential.token,
        "POST",
        "/v1/github/projections/prepare".into(),
        "application/json",
        scoped_fields(credential, fields)?,
        timeout,
    )
    .await
}

async fn export_request(
    socket: &Path,
    credential: &Credential,
    fields: Value,
    output: std::fs::File,
    timeout: Duration,
    uri: &'static str,
) -> Result<RawReply> {
    let body = scoped_fields(credential, fields)?;
    exchange(
        Endpoint::Unix(socket),
        &credential.token,
        Exchange {
            method: "POST",
            uri: uri.into(),
            content_type: "application/json",
            body,
            max_response: MAX_RESPONSE,
            output: Some(output),
        },
        timeout,
    )
    .await
}
