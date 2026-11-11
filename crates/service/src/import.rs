//! Resume a complete local pack using an immutable private client journal.
use crate::{
    Error, Result,
    client::{self, Credential, Reply},
    files,
    pack::Pack,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{fs::File, path::Path, sync::Arc, time::Duration};
use treeharbor_core::{ObjectId, ObjectStore, RevisionRef};
use treeharbor_metadata::Authority;
use treeharbor_storage::LocalStore;
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
    let journal: Journal = retain_journal(path, &proposed, 4096)?;
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
    Ok(journal)
}

pub(crate) fn retain_journal<T: Serialize + DeserializeOwned>(
    path: &Path,
    proposed: &T,
    max_bytes: usize,
) -> Result<T> {
    let bytes = serde_json::to_vec(proposed).map_err(|_| Error::ImportState)?;
    if bytes.len() > max_bytes {
        return Err(Error::ImportState);
    }
    match files::write_private(path, &bytes) {
        Ok(()) => {}
        Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    let raw = files::read_private(path, max_bytes)?;
    let journal = serde_json::from_slice(&raw).map_err(|_| Error::ImportState)?;
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
    let transfer = Transfer {
        socket: request.socket,
        credential: request.credential,
        timeout: request.timeout,
        upload: journal.upload.parse().map_err(|_| Error::ImportState)?,
        begin: json!({"method":"source_begin","key":format!("import-{}-begin",journal.upload),"authority":journal.authority,"epoch":journal.epoch,"upload":journal.upload,"ttl_seconds":journal.ttl_seconds,"max_objects":journal.max_objects,"max_bytes":journal.max_bytes}),
        finish: Finish::Command(finish(&journal)),
    };
    let (ids, store) = request.pack.into_frames();
    let reply = transfer.send(ids, store).await?;
    checked_finish(reply, &journal)
}

pub(crate) enum Finish {
    Command(Value),
    ArchiveArtifact(Value),
}

pub(crate) struct Transfer<'a> {
    pub socket: &'a Path,
    pub credential: &'a Credential,
    pub timeout: Duration,
    pub upload: Uuid,
    pub begin: Value,
    pub finish: Finish,
}

impl Transfer<'_> {
    async fn finish(&self) -> Result<Reply> {
        match &self.finish {
            Finish::Command(value) => self.command(value).await,
            Finish::ArchiveArtifact(value) => {
                client::archive_retention(self.socket, self.credential, value.clone(), self.timeout)
                    .await
            }
        }
    }

    async fn command(&self, value: &Value) -> Result<Reply> {
        client::send(
            self.socket,
            &self.credential.token,
            self.credential
                .request(value.clone())
                .to_string()
                .into_bytes(),
            self.timeout,
        )
        .await
    }

    pub(crate) async fn send(self, ids: Vec<ObjectId>, store: LocalStore) -> Result<Reply> {
        // An exact completed receipt is authoritative even after upload expiry.
        let first = self.finish().await?;
        let retry = (first.status.as_u16(), first.body["error"]["code"].as_str());
        if !matches!(
            retry,
            (404, Some("not_found")) | (409, Some("upload_membership"))
        ) {
            return Ok(first);
        }
        let started = self.command(&self.begin).await?;
        if !started.status.is_success() {
            return Ok(started);
        }
        let store = Arc::new(store);
        for object in ids {
            let reader = Arc::clone(&store);
            let frame = tokio::task::spawn_blocking(move || reader.get(object)?.encode())
                .await
                .map_err(|_| Error::ImportWorker)?
                .map_err(crate::pack::Error::Object)?;
            let reply = client::send_object(
                self.socket,
                self.credential,
                self.upload,
                object,
                frame,
                self.timeout,
            )
            .await?;
            if !reply.status.is_success() {
                if reply.status.as_u16() == 409
                    && reply.body["error"]["code"] == "upload_state_changed"
                {
                    return self.finish().await;
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
        self.finish().await
    }
}
