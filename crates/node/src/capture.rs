//! Managed local capture. This produces a durable private artifact; publishing
//! its objects and accepting a ChangeSet are separate server operations.
use crate::{Error, Node, Result, private, storage::Operation};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::DirBuilderExt, path::Path, sync::Arc, time::Duration};
use treeharbor_core::ObjectId;
use treeharbor_materializer::{CaptureArtifact, CaptureExpectation, Limits, Publication};
use treeharbor_metadata::{Metadata, access::Authorization, seals::SealPlan};
use treeharbor_runtime::{Controller, Engine, RemovedGuard, Specification};
use uuid::Uuid;

#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    seal: String,
    binding: String,
}
#[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
#[serde(deny_unknown_fields)]
struct Completion {
    version: u8,
    binding: String,
    manifest: String,
}
fn completion(binding: [u8; 32], manifest: [u8; 32]) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Completion {
        version: 1,
        binding: ObjectId::from_bytes(binding).to_string(),
        manifest: ObjectId::from_bytes(manifest).to_string(),
    })?)
}
struct Guard {
    operation: Arc<Operation>,
    removed: RemovedGuard,
}
impl Guard {
    fn verify(&self) -> Result<()> {
        self.operation.verify()?;
        self.removed.verify()?;
        Ok(())
    }
}
fn initialize(path: &Path, expected: &Intent) -> Result<()> {
    let fresh = match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => false,
        Err(e) => return Err(e.into()),
    };
    private::directory(path)?;
    let bytes = serde_json::to_vec(expected)?;
    let record = path.join("intent.json");
    if fresh {
        private::write_new(&record, &bytes)?;
        fs::File::open(path.parent().ok_or(Error::Configuration)?)?.sync_all()?;
    } else if private::read(&record, 1024)? != bytes {
        return Err(Error::Identity);
    }
    Ok(())
}
fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
impl Node {
    pub async fn claim_seal(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
    ) -> Result<Option<SealPlan>> {
        self.check_auth(auth)?;
        self.storage.verify()?;
        Ok(db
            .claim_workspace_seal(auth, self.identity().storage(), Uuid::new_v4(), 60)
            .await?)
    }
    /// Restrict one capture claim without changing physical or recovery fences.
    pub async fn claim_seal_for(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
        seal: Uuid,
    ) -> Result<Option<SealPlan>> {
        self.check_auth(auth)?;
        self.storage.verify()?;
        Ok(db
            .claim_workspace_seal_for(auth, self.identity().storage(), Uuid::new_v4(), 60, seal)
            .await?)
    }
    /// Capture actual stopped managed files under physical slot, volume and
    /// runtime-journal exclusion. No external claim of quiescence is accepted.
    /// A published artifact is reopened verbatim, without reading mutable files.
    /// This method alone does not mark the server seal ready or accept changes.
    #[allow(clippy::too_many_arguments)]
    pub async fn capture_seal(
        &self,
        db: &mut Metadata,
        heartbeat: &mut Metadata,
        auth: &Authorization,
        plan: &SealPlan,
        socket: &Path,
        limits: Limits,
    ) -> Result<CaptureArtifact> {
        self.check_auth(auth)?;
        let context = plan.context();
        let prepared = context.prepared();
        if prepared.node != self.identity().node()
            || prepared.slot.storage != self.identity().storage()
        {
            return Err(Error::Identity);
        }
        db.renew_workspace_seal(auth, plan, 60).await?;
        let operation = self.operation(prepared.slot.slot).await?;
        if operation.volume_digest != Some(context.volume_digest()) {
            return Err(Error::Identity);
        }
        let scope = auth.scope().clone();
        let snapshot = prepared.snapshot;
        let binding = plan.binding();
        let selection = plan.selection().clone();
        let capture_limits = Limits {
            max_source_bytes: limits.max_source_bytes.min(plan.max_bytes() as u64),
            ..limits
        };
        let work = async {
            db.renew_workspace_seal(auth, plan, 60).await?;
            let captures = private::make_directory(&self.storage.root.join("seals"))?;
            let root = captures.join(plan.id().to_string());
            initialize(
                &root,
                &Intent {
                    version: 1,
                    seal: plan.id().to_string(),
                    binding: ObjectId::from_bytes(binding).to_string(),
                },
            )?;
            let destination = root.join("artifact");
            let artifact_present = exists(&destination)?;
            let receipt = root.join("complete.json");
            let complete = exists(&receipt)?;
            if complete && !artifact_present {
                return Err(Error::Incomplete);
            }
            let artifact = if complete {
                let guard = operation.clone();
                tokio::task::spawn_blocking(move || {
                    guard.verify()?;
                    let artifact = CaptureArtifact::open(
                        &destination,
                        CaptureExpectation {
                            scope: &scope,
                            snapshot,
                            selection: &selection,
                            binding,
                        },
                        capture_limits,
                    )?;
                    if private::read(&receipt, 1024)?
                        != completion(binding, artifact.manifest_digest())?
                    {
                        return Err(Error::Identity);
                    }
                    // Retry the receipt's durability barrier after an ambiguous
                    // fsync response; this does not re-read the source files.
                    fs::File::open(&receipt)?.sync_all()?;
                    fs::File::open(&root)?.sync_all()?;
                    Ok::<_, Error>(artifact)
                })
                .await??
            } else {
                let tree = self
                    .open_run_files(operation.clone(), prepared, limits)
                    .await?;
                let volume = operation
                    .volume
                    .as_ref()
                    .ok_or(Error::Identity)?
                    .for_files(&tree.path())?;
                let engine = Engine::open(socket).await?;
                let spec = Specification::new(
                    context.attempt(),
                    context.binding(),
                    context.profile().clone(),
                    context.task().clone(),
                    volume,
                )?;
                // A seal cannot invent a new journal for an unknown execution.
                let journals = private::directory(&self.storage.root.join("runtime"))?;
                let journal = private::directory(&journals.join(context.attempt().to_string()))?;
                let controller = Controller::open(&journal, &engine, spec)?;
                let removed = controller
                    .into_removed_guard(context.engine(), context.container())
                    .await?;
                db.renew_workspace_seal(auth, plan, 60).await?;
                let guard = Arc::new(Guard {
                    operation: operation.clone(),
                    removed,
                });
                let publication = Publication::new(destination.clone(), binding, guard.clone());
                tokio::task::spawn_blocking(move || {
                    guard.verify()?;
                    let artifact = if artifact_present {
                        CaptureArtifact::open(
                            &destination,
                            CaptureExpectation {
                                scope: &scope,
                                snapshot,
                                selection: &selection,
                                binding,
                            },
                            capture_limits,
                        )?
                    } else {
                        tree.capture_selected(&selection, &publication, capture_limits)?
                    };
                    guard.verify()?;
                    // The generic artifact receipt is not a node quiescence
                    // receipt. Persist this marker only under the runtime guard.
                    private::write_new(
                        &receipt,
                        &completion(binding, artifact.manifest_digest())?,
                    )?;
                    Ok::<_, Error>(artifact)
                })
                .await??
            };
            if artifact.objects().len() as i64 > plan.max_objects() {
                return Err(treeharbor_materializer::Error::Limit.into());
            }
            // A lost lease never becomes a server publication. The already
            // fsynced private artifact is retained for a new authorized claimant.
            db.renew_workspace_seal(auth, plan, 60).await?;
            Ok(artifact)
        };
        let renew = async {
            let mut interval = tokio::time::interval(Duration::from_secs(15));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            interval.tick().await;
            loop {
                interval.tick().await;
                if let Err(error) = heartbeat.renew_workspace_seal(auth, plan, 60).await {
                    return Err::<CaptureArtifact, Error>(error.into());
                }
            }
        };
        tokio::select! { biased; result=work=>result, failure=renew=>failure }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn durable_capture_intent_is_exact_and_does_not_repair_unknown_state() {
        let parent = tempfile::tempdir().unwrap();
        fs::set_permissions(parent.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let intent = Intent {
            version: 1,
            seal: Uuid::from_u128(1).to_string(),
            binding: ObjectId::from_bytes([42; 32]).to_string(),
        };
        assert_eq!(
            serde_json::to_vec(&intent).unwrap(),
            include_bytes!("../tests/fixtures/capture-intent-v1.json")
        );
        assert_eq!(
            completion([42; 32], [43; 32]).unwrap(),
            include_bytes!("../tests/fixtures/capture-complete-v1.json")
        );
        let path = parent.path().join("capture");
        initialize(&path, &intent).unwrap();
        initialize(&path, &intent).unwrap();
        let changed = Intent {
            version: 1,
            seal: Uuid::from_u128(2).to_string(),
            binding: intent.binding.clone(),
        };
        assert!(initialize(&path, &changed).is_err());
        let missing = parent.path().join("missing");
        fs::create_dir(&missing).unwrap();
        fs::set_permissions(&missing, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(initialize(&missing, &intent).is_err());
        assert!(!missing.join("intent.json").exists());
        fs::remove_file(path.join("intent.json")).unwrap();
        std::os::unix::fs::symlink(parent.path().join("unknown"), path.join("intent.json"))
            .unwrap();
        assert!(initialize(&path, &intent).is_err());
    }
}
