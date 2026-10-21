//! Resume a complete local pack using an immutable private client journal.
use crate::{
    Error, Result,
    client::{self, Credential, Reply},
    files,
    pack::Pack,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{fs::File, path::Path, sync::Arc, time::Duration};
use treeharbor_core::RevisionRef;
use treeharbor_metadata::Authority;
use uuid::Uuid;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    tenant: String,
    repository: String,
    authority: String,
    epoch: i64,
    pack_fingerprint: String,
    commit: String,
    upload: String,
    ttl_seconds: i32,
    max_objects: i64,
    max_bytes: i64,
}
pub struct Request<'a> {
    pub socket: &'a Path,
    pub credential: &'a Credential,
    pub pack: Pack,
    pub state: &'a Path,
    pub authority: Authority,
    pub epoch: i64,
    pub ttl_seconds: i32,
    pub timeout: Duration,
}
fn load_or_create(
    path: &Path,
    pack: &Pack,
    credential: &Credential,
    authority: Authority,
    epoch: i64,
    ttl: i32,
) -> Result<Journal> {
    if epoch <= 0 || !(1..=3600).contains(&ttl) {
        return Err(Error::Config);
    }
    let proposed = Journal {
        version: 1,
        tenant: credential.scope.tenant().into(),
        repository: credential.scope.repository().to_string(),
        authority: authority.as_str().into(),
        epoch,
        pack_fingerprint: pack.fingerprint.to_string(),
        commit: pack.commit.to_string(),
        upload: Uuid::new_v4().to_string(),
        ttl_seconds: ttl,
        max_objects: pack.ids.len() as i64,
        max_bytes: pack.bytes as i64,
    };
    let bytes = serde_json::to_vec(&proposed).map_err(|_| Error::ImportState)?;
    match files::write_private(path, &bytes) {
        Ok(()) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let raw = files::read_private(path, 4096)?;
    let journal: Journal = serde_json::from_slice(&raw).map_err(|_| Error::ImportState)?;
    let id: Uuid = journal.upload.parse().map_err(|_| Error::ImportState)?;
    if journal.version != 1
        || journal.tenant != proposed.tenant
        || journal.repository != proposed.repository
        || journal.authority != proposed.authority
        || journal.epoch != epoch
        || journal.pack_fingerprint != proposed.pack_fingerprint
        || journal.commit != proposed.commit
        || journal.ttl_seconds != ttl
        || journal.max_objects != proposed.max_objects
        || journal.max_bytes != proposed.max_bytes
        || id.to_string() != journal.upload
    {
        return Err(Error::ImportState);
    }
    // A concurrent creator may not have synced the new name yet. Every reader
    // makes the journal directory durable before starting network effects.
    File::open(files::private_directory(
        path.parent().ok_or(Error::UnsafeFile)?,
    )?)?
    .sync_all()?;
    Ok(journal)
}
fn finish(j: &Journal) -> serde_json::Value {
    json!({"method":"source_finish","key":format!("import-{}-finish",j.upload),"authority":j.authority,"epoch":j.epoch,"upload":j.upload,"commit":j.commit})
}
async fn command(r: &Request<'_>, value: serde_json::Value) -> Result<Reply> {
    client::send(
        r.socket,
        &r.credential.token,
        r.credential.request(value).to_string().into_bytes(),
        r.timeout,
    )
    .await
}
fn checked_finish(reply: Reply, j: &Journal) -> Result<Reply> {
    if reply.status.is_success() {
        let revision =
            RevisionRef::Native(j.commit.parse().map_err(|_| Error::ImportState)?).to_string();
        if reply.body["version"] != 1
            || reply.body["result"]["revision"] != revision
            || reply.body["result"]["upload"] != j.upload
            || reply.body["result"]["state"] != "completed"
        {
            return Err(Error::Http);
        }
    }
    Ok(reply)
}
pub async fn send(request: Request<'_>) -> Result<Reply> {
    let journal = load_or_create(
        request.state,
        &request.pack,
        request.credential,
        request.authority,
        request.epoch,
        request.ttl_seconds,
    )?;
    // Retry the exact final command first: a historical receipt is stronger
    // evidence than a mutable session status, and survives session expiry.
    let first = command(&request, finish(&journal)).await?;
    if first.status.is_success() {
        return checked_finish(first, &journal);
    }
    if !((first.status.as_u16() == 404 && first.body["error"]["code"] == "not_found")
        || (first.status.as_u16() == 409 && first.body["error"]["code"] == "upload_membership"))
    {
        return Ok(first);
    }
    let begin=command(&request,json!({"method":"source_begin","key":format!("import-{}-begin",journal.upload),"authority":journal.authority,"epoch":journal.epoch,"upload":journal.upload,"ttl_seconds":journal.ttl_seconds,"max_objects":journal.max_objects,"max_bytes":journal.max_bytes})).await?;
    if !begin.status.is_success() {
        return Ok(begin);
    }
    let upload: Uuid = journal.upload.parse().map_err(|_| Error::ImportState)?;
    let socket = request.socket;
    let credential = request.credential;
    let timeout = request.timeout;
    let pack = Arc::new(request.pack);
    for id in &pack.ids {
        let object = *id;
        let reader = pack.clone();
        let frame = tokio::task::spawn_blocking(move || reader.frame(object))
            .await
            .map_err(|_| Error::ImportWorker)??;
        let reply = client::send_object(socket, credential, upload, object, frame, timeout).await?;
        if !reply.status.is_success() {
            // An identical concurrent client may have finished this session.
            if reply.status.as_u16() == 409 && reply.body["error"]["code"] == "upload_state_changed"
            {
                let retry = client::send(
                    socket,
                    &credential.token,
                    credential
                        .request(finish(&journal))
                        .to_string()
                        .into_bytes(),
                    timeout,
                )
                .await?;
                return checked_finish(retry, &journal);
            }
            return Ok(reply);
        }
        if reply.body["version"] != 1
            || reply.body["result"]["object"] != object.to_string()
            || reply.body["result"]["state"] != "stored"
        {
            return Err(Error::Http);
        }
    }
    let reply = client::send(
        socket,
        &credential.token,
        credential
            .request(finish(&journal))
            .to_string()
            .into_bytes(),
        timeout,
    )
    .await?;
    checked_finish(reply, &journal)
}
