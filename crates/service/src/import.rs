//! Resume a complete local pack using an immutable private client journal.
use crate::{
    Error, Result,
    client::{self, Credential, Reply},
    files,
    pack::Pack,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs::File, path::Path, sync::Arc, time::Duration};
use treeharbor_core::{Object, ObjectId, ObjectStore, RevisionRef};
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
        source_root: Some(request.pack.commit()),
    };
    let (ids, store) = request.pack.into_frames();
    let reply = transfer.send(ids, store).await?;
    checked_finish(reply, &journal)
}

pub(crate) enum Finish {
    Command(Value),
    ArchiveUpload { path: &'static str, fields: Value },
}

pub(crate) struct Transfer<'a> {
    pub socket: &'a Path,
    pub credential: &'a Credential,
    pub timeout: Duration,
    pub upload: Uuid,
    pub begin: Value,
    pub finish: Finish,
    pub source_root: Option<ObjectId>,
}

fn source_ancestors(
    store: &impl ObjectStore,
    root: ObjectId,
    expected_objects: usize,
) -> Result<BTreeSet<ObjectId>> {
    let mut reached = BTreeSet::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        if reached.insert(id) {
            if reached.len() > expected_objects {
                return Err(Error::Http);
            }
            pending.extend(
                store
                    .get(id)
                    .map_err(crate::pack::Error::Object)?
                    .references()
                    .into_iter()
                    .map(|(target, _)| target),
            );
        }
    }
    if reached.len() != expected_objects {
        return Err(Error::Http);
    }
    Ok(reached)
}

pub(crate) enum ParentReuse {
    Continue(BTreeSet<ObjectId>),
    Rejected(Reply),
}

pub(crate) struct ParentReuseRequest<'a> {
    pub socket: &'a Path,
    pub credential: &'a Credential,
    pub timeout: Duration,
    pub upload: Uuid,
    pub github_epoch: Option<i64>,
}

impl ParentReuseRequest<'_> {
    pub(crate) async fn enroll(
        &self,
        store: &Arc<impl ObjectStore + Send + Sync + 'static>,
        root: ObjectId,
        pack_objects: usize,
    ) -> Result<ParentReuse> {
        let reader = Arc::clone(store);
        let parent = tokio::task::spawn_blocking(move || -> Result<_> {
            let Object::NativeCommit(commit) =
                reader.get(root).map_err(crate::pack::Error::Object)?
            else {
                return Err(Error::ImportState);
            };
            Ok(commit.parents.first().copied())
        })
        .await
        .map_err(|_| Error::ImportWorker)??;
        let Some(parent) = parent else {
            return Ok(ParentReuse::Continue(BTreeSet::new()));
        };
        let base = RevisionRef::Native(parent).to_string();
        let command = if let Some(epoch) = self.github_epoch {
            json!({"method":"github","request":{"action":"import_reuse_base","import":self.upload,"epoch":epoch,"base":base}})
        } else {
            json!({"method":"source_reuse_base","upload":self.upload,"base":base})
        };
        let reply = client::send(
            self.socket,
            &self.credential.token,
            self.credential.request(command).to_string().into_bytes(),
            self.timeout,
        )
        .await?;
        if !reply.status.is_success() {
            let status = (reply.status.as_u16(), reply.body["error"]["code"].as_str());
            return Ok(
                if matches!(
                    status,
                    (400, Some("invalid_request"))
                        | (404, Some("not_found"))
                        | (409, Some("upload_membership"))
                        | (429, Some("upload_limit"))
                ) || (self.github_epoch.is_none() && status == (403, Some("forbidden")))
                {
                    ParentReuse::Continue(BTreeSet::new())
                } else {
                    ParentReuse::Rejected(reply)
                },
            );
        }
        let result = &reply.body["result"];
        if reply.body["version"] != 1
            || result["upload"] != self.upload.to_string()
            || result["base"] != base
            || self
                .github_epoch
                .is_some_and(|epoch| result["epoch"] != epoch)
        {
            return Err(Error::Http);
        }
        let objects = result["objects"]
            .as_u64()
            .filter(|&n| n > 0 && n < pack_objects as u64)
            .ok_or(Error::Http)? as usize;
        let reader = Arc::clone(store);
        let retained =
            tokio::task::spawn_blocking(move || source_ancestors(reader.as_ref(), parent, objects))
                .await
                .map_err(|_| Error::ImportWorker)??;
        Ok(ParentReuse::Continue(retained))
    }
}

impl Transfer<'_> {
    async fn finish(&self) -> Result<Reply> {
        match &self.finish {
            Finish::Command(value) => self.command(value).await,
            Finish::ArchiveUpload { path, fields } => {
                client::archive_operation(
                    self.socket,
                    self.credential,
                    path,
                    fields.clone(),
                    self.timeout,
                )
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

    pub(crate) async fn send(
        self,
        ids: Vec<ObjectId>,
        store: impl ObjectStore + Send + Sync + 'static,
    ) -> Result<Reply> {
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
        let retained = if let Some(root) = self.source_root {
            let reuse = ParentReuseRequest {
                socket: self.socket,
                credential: self.credential,
                timeout: self.timeout,
                upload: self.upload,
                github_epoch: None,
            };
            match reuse.enroll(&store, root, ids.len()).await? {
                ParentReuse::Continue(retained) => retained,
                ParentReuse::Rejected(reply) => {
                    if reply.status.as_u16() == 409
                        && reply.body["error"]["code"] == "upload_state_changed"
                    {
                        return self.finish().await;
                    }
                    return Ok(reply);
                }
            }
        } else {
            BTreeSet::new()
        };
        for object in ids {
            if retained.contains(&object) {
                continue;
            }
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
