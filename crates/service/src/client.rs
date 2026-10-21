use crate::{Error, Result, files};
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
use tokio::net::UnixStream;
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
const MAX_RESPONSE: usize = 1024 * 1024 + 64 * 1024;
/// One request per connection; neither tokens nor bodies are logged. Transport
/// errors/timeouts cannot prove whether a submitted mutation committed.
pub async fn send(
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
    send_uploaded_object(socket, credential, upload, id, frame, timeout, false).await
}
pub async fn send_change_blob(
    socket: &Path,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
) -> Result<Reply> {
    send_uploaded_object(socket, credential, upload, id, frame, timeout, true).await
}
async fn send_uploaded_object(
    socket: &Path,
    credential: &Credential,
    upload: Uuid,
    id: treeharbor_core::ObjectId,
    frame: Vec<u8>,
    timeout: Duration,
    change: bool,
) -> Result<Reply> {
    if frame.len() > treeharbor_core::MAX_PAYLOAD + treeharbor_core::FRAME_HEADER {
        return Err(Error::Size);
    }
    let path = format!(
        "/v1/{}/{}/{}/uploads/{}/objects/{}",
        if change { "changes" } else { "source" },
        credential.scope.tenant(),
        credential.scope.repository(),
        upload,
        id
    );
    send_request(
        socket,
        &credential.token,
        "PUT",
        path,
        "application/octet-stream",
        frame,
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
    if body.len() > MAX_REQUEST_BYTES {
        return Err(Error::Size);
    }
    send_request(
        socket,
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
    let raw = exchange(
        socket,
        token,
        Exchange {
            method,
            uri,
            content_type,
            body,
            max_response: MAX_RESPONSE,
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
pub(crate) struct RawReply {
    pub status: StatusCode,
    pub headers: hyper::HeaderMap,
    pub bytes: Bytes,
}
struct Exchange {
    method: &'static str,
    uri: String,
    content_type: &'static str,
    body: Vec<u8>,
    max_response: usize,
}
async fn exchange(
    socket: &Path,
    token: &SecretToken,
    request: Exchange,
    timeout: Duration,
) -> Result<RawReply> {
    let Exchange {
        method,
        uri,
        content_type,
        body,
        max_response,
    } = request;
    files::private_directory(socket.parent().ok_or(Error::UnsafeFile)?)?;
    let meta = fs::symlink_metadata(socket)?;
    if !meta.file_type().is_socket()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::UnsafeFile);
    }
    tokio::time::timeout(timeout, async {
        let stream = UnixStream::connect(socket).await?;
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
            .header(header::HOST, "treeharbor.local")
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONNECTION, "close")
            .body(Full::new(Bytes::from(body)))
            .map_err(|_| Error::Http)?;
        let mut authorization =
            header::HeaderValue::from_str(&format!("Bearer {}", token.expose()))
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
        let bytes = Limited::new(response.into_body(), max_response)
            .collect()
            .await
            .map_err(|_| Error::Http)?
            .to_bytes();
        Ok(RawReply {
            status,
            headers,
            bytes,
        })
    })
    .await
    .map_err(|_| Error::Timeout)?
}

/// The scope always comes from the credential file, never caller-supplied fields.
pub(crate) async fn source_read(
    socket: &Path,
    credential: &Credential,
    mut fields: Value,
    file: bool,
    timeout: Duration,
) -> Result<RawReply> {
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
    exchange(
        socket,
        &credential.token,
        Exchange {
            method: "POST",
            uri: "/v1/source/read".into(),
            content_type: "application/json",
            body,
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
