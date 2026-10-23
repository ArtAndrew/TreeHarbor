//! Physical workspace cleanup under node/slot/volume/runtime exclusion. Durable
//! intent precedes unlink; only a durable completion reaches metadata release.
use crate::{
    Error, Node, Result, private,
    removal::{Identity, Root},
    storage::{Operation, Storage},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use treeharbor_core::ObjectId;
use treeharbor_materializer::PublicationReceipt;
use treeharbor_metadata::{
    Metadata,
    access::Authorization,
    cleanup::{CleanupPlan, DeletedFiles},
};
use treeharbor_runtime::{CleanupGuard, Engine, RemovalExpectation};
use uuid::Uuid;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    binding: String,
    root: Option<Identity>,
    initialized: bool,
}
#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Completion {
    version: u8,
    intent: String,
    state: String,
}
fn hash(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn text(digest: [u8; 32]) -> String {
    ObjectId::from_bytes(digest).to_string()
}
fn complete_bytes(intent: [u8; 32]) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Completion {
        version: 1,
        intent: text(intent),
        state: "deleted".into(),
    })?)
}
fn binding(plan: &CleanupPlan, original: Option<&[u8]>) -> Result<[u8; 32]> {
    binding_digest(&json!([
        1,
        plan.id().to_string(),
        plan.scope().tenant(),
        plan.scope().repository().to_string(),
        plan.workspace().to_string(),
        plan.generation(),
        plan.epoch(),
        plan.authority().as_str(),
        plan.node(),
        plan.storage().to_string(),
        original.map(|bytes| text(hash(bytes))),
        plan.placement().and_then(|p| p.manifest_digest).map(text),
        plan.runtime().map(|r| json!([
            r.run().to_string(),
            r.attempt().to_string(),
            text(r.binding()),
            text(r.volume_digest()),
            r.engine(),
            r.container()
        ]))
    ]))
}
fn binding_digest(payload: &Value) -> Result<[u8; 32]> {
    let bytes = serde_json::to_vec(payload)?;
    let mut digest = Sha256::new();
    digest.update(b"treeharbor-cleanup-binding\0v1\0");
    digest.update(bytes);
    Ok(digest.finalize().into())
}
struct Guard {
    storage: Arc<Storage>,
    operation: Option<Arc<Operation>>,
    runtime: Option<CleanupGuard>,
}
impl Guard {
    fn verify(&self) -> Result<()> {
        self.storage.verify()?;
        if let Some(op) = &self.operation {
            op.verify()?;
        }
        if let Some(runtime) = &self.runtime {
            runtime.verify()?;
        } else if let Some(volume) = self.operation.as_ref().and_then(|op| op.volume.as_ref())
            && exists(&volume.identity().mount.join("treeharbor-execution.json"))?
        {
            return Err(Error::Incomplete);
        }
        Ok(())
    }
}
struct Deletion {
    guard: Arc<Guard>,
    root: Option<Root>,
    directory: PathBuf,
    intent: Intent,
    bytes: Vec<u8>,
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn allowed(name: &[u8], initialized: bool) -> bool {
    name.starts_with(b"record-write-")
        || (initialized && matches!(name, b"intent.json" | b"tree" | b"staging"))
}
fn validate_names(root: &Root, initialized: bool) -> Result<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt;
    let names = root.names()?;
    for name in &names {
        if !allowed(name, initialized) {
            return Err(Error::Incomplete);
        }
        if name.starts_with(b"record-write-") {
            private::read(
                &root.identity().path.join(std::ffi::OsStr::from_bytes(name)),
                16 * 1024,
            )?;
        }
    }
    Ok(names)
}
impl Deletion {
    fn open(
        guard: Arc<Guard>,
        id: Uuid,
        bound: [u8; 32],
        original: Option<Vec<u8>>,
        manifest: Option<[u8; 32]>,
    ) -> Result<Self> {
        guard.verify()?;
        let journals = private::make_directory(&guard.storage.root.join("cleanup"))?;
        let directory = private::make_directory(&journals.join(id.to_string()))?;
        let root = match &guard.operation {
            Some(op) if exists(&op.root)? => Some(Root::open(&op.root)?),
            _ => None,
        };
        let record = directory.join("intent.json");
        let bytes = if exists(&record)? {
            private::read(&record, 32 * 1024)?
        } else {
            // An interrupted atomic write can leave only its private temporaries.
            // No deletion is started until a durable intent is admitted by the DB.
            for entry in fs::read_dir(&directory)? {
                let entry = entry?;
                if !entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b"record-write-")
                {
                    return Err(Error::Incomplete);
                }
                private::read(&entry.path(), 32 * 1024)?;
                fs::remove_file(entry.path())?;
            }
            let mut initialized = false;
            if let (Some(root), Some(original)) = (&root, &original) {
                let path = &root.identity().path;
                if exists(&path.join("intent.json"))? {
                    if private::read(&path.join("intent.json"), 16 * 1024)? != *original {
                        return Err(Error::Identity);
                    }
                    initialized = true;
                    if let Some(manifest) = manifest
                        && PublicationReceipt::load(&path.join("tree"), hash(original))?
                            .manifest_digest()?
                            != manifest
                    {
                        return Err(Error::Identity);
                    }
                }
                validate_names(root, initialized)?;
            }
            if manifest.is_some() && !initialized {
                return Err(Error::Incomplete);
            }
            let bytes = serde_json::to_vec(&Intent {
                version: 1,
                binding: text(bound),
                root: root.as_ref().map(|r| r.identity().clone()),
                initialized,
            })?;
            private::write_new(&record, &bytes)?;
            bytes
        };
        let intent: Intent = serde_json::from_slice(&bytes)?;
        if intent.version != 1
            || intent.binding != text(bound)
            || serde_json::to_vec(&intent)? != bytes
            || intent.root.as_ref() != root.as_ref().map(Root::identity)
            || (manifest.is_some() && !intent.initialized)
        {
            return Err(Error::Identity);
        }
        // Retry a possibly interrupted durability barrier without changing bytes.
        fs::File::open(&record)?.sync_all()?;
        fs::File::open(&directory)?.sync_all()?;
        fs::File::open(&journals)?.sync_all()?;
        let result = Self {
            guard,
            root,
            directory,
            intent,
            bytes,
        };
        result.verify()?;
        Ok(result)
    }
    fn verify(&self) -> Result<()> {
        self.guard.verify()?;
        if let Some(root) = &self.root {
            root.verify()?;
        } else if let Some(op) = &self.guard.operation
            && exists(&op.root)?
        {
            return Err(Error::Identity);
        }
        if private::read(&self.directory.join("intent.json"), 32 * 1024)? != self.bytes {
            return Err(Error::Identity);
        }
        Ok(())
    }
    fn run(self) -> Result<(Self, DeletedFiles)> {
        self.verify()?;
        let intent = hash(&self.bytes);
        let bytes = complete_bytes(intent)?;
        let marker = self.directory.join("complete.json");
        if exists(&marker)? {
            if private::read(&marker, 1024)? != bytes {
                return Err(Error::Identity);
            }
            if let Some(root) = &self.root {
                if !root.empty()? {
                    return Err(Error::Incomplete);
                }
                root.sync()?;
            }
        } else {
            if let Some(root) = &self.root {
                let names = validate_names(root, self.intent.initialized)?;
                root.remove(&names, || self.verify())?;
            }
            self.verify()?;
            private::write_new(&marker, &bytes)?;
        }
        fs::File::open(&marker)?.sync_all()?;
        fs::File::open(&self.directory)?.sync_all()?;
        self.verify()?;
        Ok((
            self,
            DeletedFiles {
                intent_digest: intent,
                receipt_digest: hash(&bytes),
            },
        ))
    }
}
impl Node {
    pub async fn claim_cleanup(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
        workspace: Uuid,
    ) -> Result<Option<CleanupPlan>> {
        self.check_auth(auth)?;
        self.storage.verify()?;
        Ok(db
            .claim_workspace_cleanup(
                auth,
                workspace,
                self.identity().storage(),
                Uuid::new_v4(),
                60,
            )
            .await?)
    }
    /// Drain and managed runtime removal must already be committed. This observes
    /// actual runtime absence, never fabricates a stop from a lease or metadata.
    pub async fn cleanup_workspace(
        &self,
        db: &mut Metadata,
        heartbeat: &mut Metadata,
        auth: &Authorization,
        plan: &CleanupPlan,
        socket: Option<&Path>,
    ) -> Result<Value> {
        self.check_auth(auth)?;
        self.storage.verify()?;
        if plan.node() != self.identity().node()
            || plan.storage() != self.identity().storage()
            || plan.scope().tenant() != auth.scope().tenant()
            || plan.scope().repository() != auth.scope().repository()
        {
            return Err(Error::Identity);
        }
        // No old placement is opened on completed replay: it may now be reused.
        if let Some(result) = db
            .completed_workspace_cleanup(auth, plan.workspace(), plan.storage())
            .await?
        {
            return Ok(result);
        }
        db.renew_workspace_cleanup(auth, plan, 60).await?;
        let work = async {
            let operation = match plan.placement() {
                Some(p) => Some(self.operation(p.slot.slot).await?),
                None => None,
            };
            db.renew_workspace_cleanup(auth, plan, 60).await?;
            if operation.as_ref().and_then(|op| op.volume_digest)
                != plan.placement().and_then(|p| p.volume_digest)
            {
                return Err(Error::Identity);
            }
            let runtime = if let Some(context) = plan.runtime() {
                let op = operation.as_ref().ok_or(Error::Identity)?;
                let volume = op.volume.as_ref().ok_or(Error::Identity)?.try_clone()?;
                let files = op.root.join("tree/files");
                let engine = Engine::open(socket.ok_or(Error::Configuration)?).await?;
                let journal = private::directory(&self.storage.root.join("runtime"))?
                    .join(context.attempt().to_string());
                Some(
                    CleanupGuard::open(
                        &journal,
                        &engine,
                        volume,
                        RemovalExpectation {
                            attempt: context.attempt(),
                            binding: context.binding(),
                            profile: context.profile(),
                            task: context.task(),
                            engine: context.engine(),
                            container: context.container(),
                            files: &files,
                        },
                    )
                    .await?,
                )
            } else {
                None
            };
            let guard = Arc::new(Guard {
                storage: self.storage.clone(),
                operation,
                runtime,
            });
            guard.verify()?;
            let original = plan
                .placement()
                .map(|_| crate::worker::cleanup_intent(plan))
                .transpose()?;
            let bound = binding(plan, original.as_deref())?;
            let id = plan.id();
            let manifest = plan.placement().and_then(|p| p.manifest_digest);
            let deletion = tokio::task::spawn_blocking(move || {
                Deletion::open(guard, id, bound, original, manifest)
            })
            .await??;
            let intent = hash(&deletion.bytes);
            if plan.intent().is_some_and(|expected| expected != intent) {
                return Err(Error::Identity);
            }
            db.admit_workspace_cleanup(auth, plan, intent).await?;
            // The owned Deletion carries every OS/slot guard through the actual
            // unlink and fsync work, even if this async future is abandoned.
            let (deletion, proof) = tokio::task::spawn_blocking(move || deletion.run()).await??;
            deletion.verify()?;
            let result = db.complete_workspace_cleanup(auth, plan, &proof).await?;
            drop(deletion);
            Ok(result)
        };
        let renew = async {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                if let Err(error) = heartbeat.renew_workspace_cleanup(auth, plan, 60).await {
                    return Err::<(), Error>(error.into());
                }
            }
        };
        let failure = tokio::select! {biased;result=work=>return result,failed=renew=>failed};
        // Commit may have succeeded just before the renewal observed 'deleted'.
        if let Some(result) = db
            .completed_workspace_cleanup(auth, plan.workspace(), plan.storage())
            .await?
        {
            return Ok(result);
        }
        failure?;
        Err(Error::Incomplete)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cleanup_journal_matches_independent_bytes_and_digest_vectors() {
        let payload: Value =
            serde_json::from_slice(include_bytes!("../tests/fixtures/cleanup-binding-v1.json"))
                .unwrap();
        assert_eq!(
            text(binding_digest(&payload).unwrap()),
            format!(
                "sha256:{}",
                include_str!("../tests/fixtures/cleanup-binding-v1.sha256").trim()
            )
        );
        let intent = Intent {
            version: 1,
            binding: text([42; 32]),
            root: Some(Identity {
                path: "/private/node/slots/slot".into(),
                device: 1795,
                inode: 12,
            }),
            initialized: true,
        };
        let raw = serde_json::to_vec(&intent).unwrap();
        assert_eq!(
            raw,
            include_bytes!("../tests/fixtures/cleanup-intent-v1.json")
        );
        assert_eq!(
            text(hash(&raw)),
            format!(
                "sha256:{}",
                include_str!("../tests/fixtures/cleanup-intent-v1.sha256").trim()
            )
        );
        let complete = complete_bytes(hash(&raw)).unwrap();
        assert_eq!(
            complete,
            include_bytes!("../tests/fixtures/cleanup-complete-v1.json")
        );
        assert_eq!(
            text(hash(&complete)),
            format!(
                "sha256:{}",
                include_str!("../tests/fixtures/cleanup-complete-v1.sha256").trim()
            )
        );
        let mut value: Value = serde_json::from_slice(&raw).unwrap();
        value["extra"] = json!(true);
        assert!(serde_json::from_value::<Intent>(value).is_err());
    }
}
