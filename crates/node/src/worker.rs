use crate::{Error, Node, Result, private, storage::Operation};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use treeharbor_core::SourceProfile;
use treeharbor_materializer::{
    BaselineExpectation, Limits, MaterializedTree, Materializer, Publication, PublicationReceipt,
    Selection,
};
use treeharbor_metadata::{
    Metadata,
    access::Authorization,
    preparations::{Preparation, PreparedFiles, PreparedWorkspace, StorageSlot},
};
use treeharbor_storage::ManagedStore;
use uuid::Uuid;

pub struct Prepare<'a> {
    pub authorization: &'a Authorization,
    pub plan: &'a Preparation,
    pub store: &'a ManagedStore,
    pub limits: Limits,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Intent {
    version: u8,
    tenant: String,
    repository: String,
    node: String,
    storage: String,
    slot: String,
    attempt: String,
    workspace: String,
    generation: i64,
    authority: String,
    epoch: i64,
    revision: String,
    snapshot: String,
    profile: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    volume: Option<String>,
}
impl Intent {
    fn cleanup(p: &treeharbor_metadata::cleanup::CleanupPlan) -> Result<Self> {
        let source = p.placement().ok_or(Error::Identity)?;
        Ok(Self {
            version: if source.volume_digest.is_some() { 2 } else { 1 },
            tenant: p.scope().tenant().into(),
            repository: p.scope().repository().to_string(),
            node: p.node().into(),
            storage: p.storage().to_string(),
            slot: source.slot.slot.to_string(),
            attempt: source.attempt.to_string(),
            workspace: p.workspace().to_string(),
            generation: source.generation,
            authority: p.authority().as_str().into(),
            epoch: p.epoch(),
            revision: source.revision.to_string(),
            snapshot: source.snapshot.to_string(),
            profile: source.profile.id().to_string(),
            volume: source
                .volume_digest
                .map(|v| treeharbor_core::ObjectId::from_bytes(v).to_string()),
        })
    }
    fn plan(p: &Preparation) -> Self {
        Self {
            version: if p.volume_digest().is_some() { 2 } else { 1 },
            tenant: p.scope().tenant().into(),
            repository: p.scope().repository().to_string(),
            node: p.node().into(),
            storage: p.slot().storage.to_string(),
            slot: p.slot().slot.to_string(),
            attempt: p.attempt().to_string(),
            workspace: p.workspace().to_string(),
            generation: p.generation(),
            authority: p.authority().as_str().into(),
            epoch: p.epoch(),
            revision: p.revision().to_string(),
            snapshot: p.snapshot().to_string(),
            profile: p.profile().id().to_string(),
            volume: p
                .volume_digest()
                .map(|v| treeharbor_core::ObjectId::from_bytes(v).to_string()),
        }
    }
    fn ready(p: &PreparedWorkspace) -> Self {
        Self {
            version: if p.volume_digest.is_some() { 2 } else { 1 },
            tenant: p.scope.tenant().into(),
            repository: p.scope.repository().to_string(),
            node: p.node.clone(),
            storage: p.slot.storage.to_string(),
            slot: p.slot.slot.to_string(),
            attempt: p.attempt.to_string(),
            workspace: p.workspace.to_string(),
            generation: p.generation,
            authority: p.authority.as_str().into(),
            epoch: p.epoch,
            revision: p.revision.to_string(),
            snapshot: p.snapshot.to_string(),
            profile: p.profile.id().to_string(),
            volume: p
                .volume_digest
                .map(|v| treeharbor_core::ObjectId::from_bytes(v).to_string()),
        }
    }
    fn binding(&self) -> Result<[u8; 32]> {
        Ok(Sha256::digest(serde_json::to_vec(self)?).into())
    }
    fn check(&self, node: &Node) -> Result<()> {
        if self.tenant != node.identity().tenant()
            || self.node != node.identity().node()
            || self.storage != node.identity().storage().to_string()
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
}

pub(crate) fn cleanup_intent(plan: &treeharbor_metadata::cleanup::CleanupPlan) -> Result<Vec<u8>> {
    Ok(serde_json::to_vec(&Intent::cleanup(plan)?)?)
}

impl Node {
    /// Scope-filtered claim/reservation; no filesystem effects yet. Reclaimed
    /// jobs reuse the existing slot and attempt. A new claim gets one new slot.
    pub async fn claim(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
    ) -> Result<Option<Preparation>> {
        self.claim_selected(db, auth, None).await
    }
    pub async fn claim_on_slot(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
        slot: Uuid,
    ) -> Result<Option<Preparation>> {
        let owned = self.operation(slot).await?;
        if owned.volume.is_none() {
            return Err(Error::Configuration);
        }
        self.claim_selected(db, auth, Some(slot)).await
    }
    async fn claim_selected(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
        selected: Option<Uuid>,
    ) -> Result<Option<Preparation>> {
        self.check_auth(auth)?;
        self.storage.verify()?;
        let Some(job) = db
            .claim_preparation(auth, self.identity().storage(), Uuid::new_v4(), 60)
            .await?
        else {
            return Ok(None);
        };
        let workspace = job.payload["workspace"]
            .as_str()
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(Error::Incomplete)?;
        let slot = match db.preparation_status(auth, workspace).await {
            Ok(status) => status["slot"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or(Error::Incomplete)?,
            Err(treeharbor_metadata::Error::NotFound) => selected.unwrap_or_else(Uuid::new_v4),
            Err(error) => return Err(error.into()),
        };
        if selected.is_some_and(|id| id != slot) {
            return Err(Error::Identity);
        }
        Ok(Some(
            db.reserve_preparation(
                auth,
                &job,
                StorageSlot {
                    storage: self.identity().storage(),
                    slot,
                },
            )
            .await?,
        ))
    }

    /// Both database connections must refer to the same deployment/schema.
    /// The second connection renews the live reservation while source IO runs.
    /// Cancellation retains guards in outstanding filesystem tasks. No sandbox
    /// or agent credentials are mounted or launched by this operation.
    pub async fn prepare(
        &self,
        db: &mut Metadata,
        heartbeat: &mut Metadata,
        request: Prepare<'_>,
    ) -> Result<MaterializedTree> {
        let Prepare {
            authorization: auth,
            plan,
            store,
            limits,
        } = request;
        self.check_auth(auth)?;
        let intent = Intent::plan(plan);
        intent.check(self)?;
        // A previous response may have been lost after DB commit. This is an
        // authorized ready-state read, not a late completion by an expired job.
        match db.prepared_workspace(auth, plan.workspace()).await {
            Ok(ready) => {
                if Intent::ready(&ready) != intent {
                    return Err(Error::Identity);
                }
                return self.open_prepared(db, auth, plan.workspace(), limits).await;
            }
            Err(treeharbor_metadata::Error::ExecutionState) => (),
            Err(error) => return Err(error.into()),
        }
        check_limits(limits)?;
        match db.renew_preparation(auth, plan, 60).await {
            Ok(()) => (),
            Err(treeharbor_metadata::Error::ExecutionState) => {
                return self.recover_plan(db, auth, plan, limits).await;
            }
            Err(error) => return Err(error.into()),
        }
        let work = async {
            let operation = self.operation(plan.slot().slot).await?;
            if operation.volume_digest != plan.volume_digest() {
                return Err(Error::Identity);
            }
            // Waiting on another local operation can outlive a database lease.
            match db.renew_preparation(auth, plan, 60).await {
                Ok(()) => (),
                Err(treeharbor_metadata::Error::ExecutionState) => {
                    let ready = db.prepared_workspace(auth, plan.workspace()).await?;
                    if Intent::ready(&ready) != intent {
                        return Err(Error::Identity);
                    }
                    drop(operation);
                    return self.open_prepared(db, auth, plan.workspace(), limits).await;
                }
                Err(error) => return Err(error.into()),
            }
            let binding = intent.binding()?;
            let owned = operation.clone();
            let root =
                tokio::task::spawn_blocking(move || initialize_attempt(&owned, &intent)).await??;
            let tree_path = root.join("tree");
            let tree = if exists(&tree_path)? {
                self.load_published(
                    operation.clone(),
                    tree_path,
                    plan.scope().clone(),
                    plan.snapshot(),
                    plan.profile().clone(),
                    binding,
                    limits,
                )
                .await?
            } else {
                let owned = operation.clone();
                let staging = root.join("staging");
                let tenant = plan.scope().tenant().to_owned();
                let materializer = tokio::task::spawn_blocking(move || {
                    clear_unpublished(&owned, &staging, &tenant)?;
                    Materializer::open(staging, 1).map_err(Error::from)
                })
                .await??;
                let publication = Publication::new(tree_path, binding, operation.clone());
                materializer
                    .prepare_published(
                        db,
                        store,
                        treeharbor_materializer::Preparation {
                            scope: plan.scope(),
                            holder: plan.source_holder(),
                            snapshot: plan.snapshot(),
                            selection: Selection::from_profile(plan.profile()),
                            limits,
                        },
                        publication,
                    )
                    .await?
            };
            operation.verify()?;
            let receipt = PublicationReceipt::load(
                tree.manifest_path().parent().ok_or(Error::Incomplete)?,
                binding,
            )?;
            db.complete_preparation(
                auth,
                plan,
                &PreparedFiles {
                    snapshot: plan.snapshot(),
                    profile: plan.profile().id(),
                    manifest_digest: receipt.manifest_digest()?,
                },
            )
            .await?;
            Ok(tree)
        };
        // Keep polling filesystem/DB work while renewal waits on database locks.
        // Awaiting renewal inside a loop body could deadlock our own transaction.
        let renew = async {
            let mut tick = tokio::time::interval(Duration::from_secs(15));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                if let Err(error) = heartbeat.renew_preparation(auth, plan, 60).await {
                    return Err::<(), Error>(error.into());
                }
            }
        };
        let renewal = tokio::select! {
            biased;
            result = work => return result,
            failed = renew => failed,
        };
        // The completion can commit before its response is polled. A renewal
        // then sees `prepared`. Resolve only a current, identical ready placement;
        // this read never turns a stale worker into a completion authority.
        match renewal {
            Err(Error::Metadata(treeharbor_metadata::Error::ExecutionState)) => {
                self.recover_plan(db, auth, plan, limits).await
            }
            Err(error) => Err(error),
            Ok(()) => Err(Error::Incomplete),
        }
    }

    async fn recover_plan(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
        plan: &Preparation,
        limits: Limits,
    ) -> Result<MaterializedTree> {
        let ready = db.prepared_workspace(auth, plan.workspace()).await?;
        if Intent::ready(&ready) != Intent::plan(plan) {
            return Err(Error::Identity);
        }
        self.open_prepared(db, auth, plan.workspace(), limits).await
    }

    /// Reopen an acknowledged file-ready placement after losing all worker state.
    /// Reads current authorization/authority twice; never restores mutable files.
    pub async fn open_prepared(
        &self,
        db: &mut Metadata,
        auth: &Authorization,
        workspace: Uuid,
        limits: Limits,
    ) -> Result<MaterializedTree> {
        self.check_auth(auth)?;
        let ready = db.prepared_workspace(auth, workspace).await?;
        let intent = Intent::ready(&ready);
        intent.check(self)?;
        let binding = intent.binding()?;
        let operation = self.operation(ready.slot.slot).await?;
        if operation.volume_digest != ready.volume_digest {
            return Err(Error::Identity);
        }
        let root = operation.root.clone();
        let expected = serde_json::to_vec(&intent)?;
        let root_for_read = root.clone();
        let owned = operation.clone();
        tokio::task::spawn_blocking(move || {
            owned.verify()?;
            check_intent(&root_for_read, &expected)
        })
        .await??;
        let receipt = PublicationReceipt::load(&root.join("tree"), binding)?;
        if receipt.manifest_digest()? != ready.manifest_digest {
            return Err(Error::Identity);
        }
        let tree = self
            .load_published(
                operation.clone(),
                root.join("tree"),
                ready.scope,
                ready.snapshot,
                ready.profile,
                binding,
                limits,
            )
            .await?;
        let current = db.prepared_workspace(auth, workspace).await?;
        if Intent::ready(&current) != intent
            || current.manifest_digest != receipt.manifest_digest()?
        {
            return Err(Error::Identity);
        }
        operation.verify()?;
        Ok(tree)
    }
    pub(crate) fn check_auth(&self, auth: &Authorization) -> Result<()> {
        if auth.scope().tenant() != self.identity().tenant()
            || auth.principal() != self.identity().node()
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
    pub(crate) async fn open_run_files(
        &self,
        operation: Arc<Operation>,
        ready: &PreparedWorkspace,
        limits: Limits,
    ) -> Result<MaterializedTree> {
        let intent = Intent::ready(ready);
        intent.check(self)?;
        if operation.volume_digest != ready.volume_digest || operation.volume.is_none() {
            return Err(Error::Identity);
        }
        let binding = intent.binding()?;
        operation.verify()?;
        check_intent(&operation.root, &serde_json::to_vec(&intent)?)?;
        let receipt = PublicationReceipt::load(&operation.root.join("tree"), binding)?;
        if receipt.manifest_digest()? != ready.manifest_digest {
            return Err(Error::Identity);
        }
        let path = operation.root.join("tree");
        self.load_published(
            operation,
            path,
            ready.scope.clone(),
            ready.snapshot,
            ready.profile.clone(),
            binding,
            limits,
        )
        .await
    }
    #[allow(clippy::too_many_arguments)]
    async fn load_published(
        &self,
        operation: Arc<Operation>,
        path: PathBuf,
        scope: treeharbor_metadata::Scope,
        snapshot: treeharbor_core::ObjectId,
        profile: SourceProfile,
        binding: [u8; 32],
        limits: Limits,
    ) -> Result<MaterializedTree> {
        tokio::task::spawn_blocking(move || {
            operation.verify()?;
            let source_parent = path
                .parent()
                .ok_or(Error::Incomplete)?
                .join("staging")
                .join(scope.tenant());
            private::directory(&source_parent)?;
            File::open(source_parent)?.sync_all()?;
            let receipt = PublicationReceipt::load(&path, binding)?;
            Ok(MaterializedTree::load_baseline(
                &path,
                BaselineExpectation {
                    scope: &scope,
                    snapshot,
                    profile: &profile,
                    manifest_digest: receipt.manifest_digest()?,
                },
                limits,
            )?)
        })
        .await?
    }
}

fn exists(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn check_intent(root: &Path, expected: &[u8]) -> Result<()> {
    private::directory(root)?;
    let raw = private::read(&root.join("intent.json"), 16 * 1024)?;
    if raw != expected {
        return Err(Error::Identity);
    }
    Ok(())
}
fn initialize_attempt(operation: &Operation, intent: &Intent) -> Result<PathBuf> {
    operation.verify()?;
    let root = operation.root.clone();
    private::make_directory(&root)?;
    let expected = serde_json::to_vec(intent)?;
    let path = root.join("intent.json");
    if exists(&path)? {
        check_intent(&root, &expected)?;
    } else {
        // Before a durable intent, only our unpublished atomic record temporaries
        // can exist. No source materialization is ever started in this state.
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            if !entry
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"record-write-")
            {
                return Err(Error::Incomplete);
            }
            private::read(&entry.path(), 16 * 1024)?;
            fs::remove_file(entry.path())?;
        }
        private::write_new(&path, &expected)?;
    }
    Ok(root)
}
fn clear_unpublished(operation: &Operation, staging: &Path, tenant: &str) -> Result<()> {
    operation.verify()?;
    private::make_directory(staging)?;
    // Staging has never been published or mounted into an agent. All old writers
    // have exited before the OS/operation guards can be acquired. Unknown data is
    // retained, and the published sibling tree is never touched by this cleanup.
    for entry in fs::read_dir(staging)? {
        let entry = entry?;
        if entry.file_name() != tenant {
            return Err(Error::Incomplete);
        }
        private::directory(&entry.path())?;
        for child in fs::read_dir(entry.path())? {
            let child = child?;
            if !child
                .file_name()
                .as_encoded_bytes()
                .starts_with(b"prepare-")
            {
                return Err(Error::Incomplete);
            }
            private::directory(&child.path())?;
            fs::remove_dir_all(child.path())?;
        }
        File::open(entry.path())?.sync_all()?;
    }
    File::open(staging)?.sync_all()?;
    Ok(())
}

fn check_limits(limits: Limits) -> Result<()> {
    if !(1..=1_000_000).contains(&limits.max_entries)
        || !(1..=64 * 1024 * 1024).contains(&limits.max_path_bytes)
        || !(1..=treeharbor_core::MAX_PAYLOAD as u64).contains(&limits.max_file_bytes)
        || !(1..=64 * 1024 * 1024 * 1024).contains(&limits.max_workspace_bytes)
        || !(1..=64 * 1024 * 1024 * 1024).contains(&limits.max_source_bytes)
    {
        return Err(Error::Configuration);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn intent_v1_has_independent_bytes_and_binding() {
        let mut intent = Intent {
            version: 1,
            tenant: "tenant".into(),
            repository: Uuid::from_u128(1).to_string(),
            node: "node-1".into(),
            storage: Uuid::from_u128(2).to_string(),
            slot: Uuid::from_u128(3).to_string(),
            attempt: Uuid::from_u128(4).to_string(),
            workspace: Uuid::from_u128(5).to_string(),
            generation: 1,
            authority: "standalone".into(),
            epoch: 1,
            revision: format!("native:sha256:{}", "11".repeat(32)),
            snapshot: format!("sha256:{}", "22".repeat(32)),
            profile: format!("sha256:{}", "33".repeat(32)),
            volume: None,
        };
        assert_eq!(
            serde_json::to_vec(&intent).unwrap(),
            include_bytes!("../tests/fixtures/intent-v1.json")
        );
        assert_eq!(
            treeharbor_core::ObjectId::from_bytes(intent.binding().unwrap()).to_string(),
            "sha256:bf3f38a95aac00da06f5185ae8a100b49fb9c0b313700b36d94547b90f02e6bd"
        );
        intent.version = 2;
        intent.volume = Some(format!(
            "sha256:{}",
            include_str!("../tests/fixtures/volume-v1.sha256").trim()
        ));
        assert_eq!(
            serde_json::to_vec(&intent).unwrap(),
            include_bytes!("../tests/fixtures/intent-v2.json")
        );
        assert_eq!(
            treeharbor_core::ObjectId::from_bytes(intent.binding().unwrap()).to_string(),
            format!(
                "sha256:{}",
                include_str!("../tests/fixtures/intent-v2.sha256").trim()
            )
        );
    }
}
