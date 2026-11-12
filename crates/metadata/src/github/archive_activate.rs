//! Atomic transfer from a quiescent, externally archived GitHub repository.
mod effects;

use super::{
    archive_observation::current_archive_fence,
    archive_proof::{ArchiveStore, RetainedArchive, retained_source, verified_stage},
    archive_proposal_stage::{
        ArchiveProposalStage, StageReceipt, VerifiedArchiveProposalStage, check_retained_proposals,
        open_staged,
    },
    *,
};
use crate::{
    catalog::ObjectLease,
    identifier,
    integration::{Policy, handoff::FrozenProjection},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap, io::sink, os::unix::fs::PermissionsExt, path::Path, time::Instant,
};
use treeharbor_core::{ObjectId, ObjectStore};
use treeharbor_git_bridge::{self as git, ReattachedBranches};
use treeharbor_storage::{LocalStore, ManagedStore, PreparedObject, TenantId};

const ACTIVATE: &str = "github.archive.activate.v1";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveActivateSpec {
    pub version: u8,
    pub proposal_stage: String,
    pub observation_key: String,
    /// The old host remains read-only while this repository is Standalone.
    pub keep_host_archived: bool,
    pub policy: Policy,
}

fn operation(
    auth: &Authorization,
    command: &Command,
    spec: &ArchiveActivateSpec,
) -> Result<Request> {
    identifier(&spec.proposal_stage, 128)?;
    identifier(&spec.observation_key, 128)?;
    if spec.version != 1 || !spec.keep_host_archived || command.epoch == i64::MAX {
        return Err(Error::Invalid("archive activation specification"));
    }
    let policy = ObjectId::from_bytes(spec.policy.digest()?);
    request(
        auth,
        command,
        ACTIVATE,
        json!([
            1,
            spec.proposal_stage,
            spec.observation_key,
            policy.to_string()
        ]),
    )
}

async fn quiescent(tx: &Transaction<'_>, scope: &Scope, epoch: i64) -> Result<()> {
    let busy: bool = tx.query_one(r#"
        SELECT EXISTS(SELECT 1 FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND state<>'deleted')
          OR EXISTS(SELECT 1 FROM th_agent_runs WHERE tenant=$1 AND repository=$2
                    AND state IN ('requested','launching','running','stopping'))
          OR EXISTS(SELECT 1 FROM th_managed_runs WHERE tenant=$1 AND repository=$2 AND NOT removed)
          OR EXISTS(SELECT 1 FROM th_remote_tasks WHERE tenant=$1 AND repository=$2 AND state IN ('queued','leased'))
          OR EXISTS(SELECT 1 FROM th_outbox WHERE tenant=$1 AND repository=$2 AND state='running' AND lease_until>clock_timestamp())
          OR EXISTS(SELECT 1 FROM th_github_publications WHERE tenant=$1 AND repository=$2 AND epoch=$3
                    AND state IN ('attempting','uncertain'))
          OR EXISTS(SELECT 1 FROM th_github_pulls WHERE tenant=$1 AND repository=$2 AND epoch=$3
                    AND state IN ('attempting','uncertain'))
          OR EXISTS(SELECT 1 FROM th_github_check_publications WHERE tenant=$1 AND repository=$2 AND epoch=$3
                    AND state IN ('attempting','uncertain'))
          OR EXISTS(SELECT 1 FROM th_github_dequeues WHERE tenant=$1 AND repository=$2 AND epoch=$3
                    AND attempt IS NOT NULL AND retained_receipt IS NULL AND NOT mutation_not_sent)
          OR EXISTS(SELECT 1 FROM th_github_queue q WHERE q.tenant=$1 AND q.repository=$2 AND q.epoch=$3
                    AND q.attempt IS NOT NULL AND NOT q.dispatch_failed AND NOT q.mutation_not_sent
                    AND NOT EXISTS(SELECT 1 FROM th_github_queue_reports r
                        WHERE r.tenant=q.tenant AND r.repository=q.repository AND r.queue=q.queue
                          AND (r.report#>>'{snapshot,pull,merged}'='true' OR r.report#>>'{receipt,status}' IN ('merged','failed')))
                    AND NOT EXISTS(SELECT 1 FROM th_github_dequeues d
                        WHERE d.tenant=q.tenant AND d.repository=q.repository AND d.queue=q.queue
                          AND d.retained_receipt IS NOT NULL AND d.latest_observation#>'{after,entry}'='null'::jsonb))
        "#, &[&scope.tenant(), &scope.repository(), &epoch]).await?.get(0);
    if busy {
        return Err(Error::ArchiveActivationBusy);
    }
    Ok(())
}

async fn evidence(
    tx: &Transaction<'_>,
    auth: &Authorization,
    policy_auth: &Authorization,
    command: &Command,
    spec: &ArchiveActivateSpec,
    deadline: Instant,
) -> Result<ArchiveActivate> {
    let scope = auth.scope();
    if policy_auth.token_id() != auth.token_id() {
        return Err(Error::Forbidden);
    }
    policy_auth
        .check_transaction(tx, scope, auth.principal(), Permission::PolicyWrite)
        .await?;
    quiescent(tx, scope, command.epoch).await?;
    let (stage_receipt, proposals) =
        open_staged(tx, scope, command.epoch, &spec.proposal_stage).await?;
    let baseline = proposals.archive.observation();
    let fence = current_archive_fence(
        tx,
        scope,
        command.epoch,
        &spec.observation_key,
        baseline,
        &spec.proposal_stage,
    )
    .await?;
    let (branch_stage_key, source) =
        verified_stage(tx, scope, command.epoch, &fence.request.proof_key).await?;
    let branches = retained_source(tx, scope, &branch_stage_key, source).await?;
    let history =
        super::handoff::retained_history(tx, scope, command.epoch, &fence.observation, deadline)
            .await?;
    Ok(ArchiveActivate {
        stage_receipt,
        proposals,
        fence,
        branches,
        history,
    })
}

pub enum ArchiveActivateWork {
    Replay(Value),
    Verify(Box<ArchiveActivate>),
}

pub struct ArchiveActivate {
    stage_receipt: StageReceipt,
    proposals: Box<ArchiveProposalStage>,
    fence: ArchiveObservationReceipt,
    branches: RetainedArchive,
    history: Option<FrozenProjection>,
}

pub struct VerifiedArchiveActivation {
    stage_receipt: StageReceipt,
    proposals: VerifiedArchiveProposalStage,
    fence: ArchiveObservationReceipt,
    branches: RetainedArchive,
    reentry: Option<Reentry>,
    deadline: Instant,
}

struct Reentry {
    history: FrozenProjection,
    graph: ReattachedBranches,
    store: LocalStore,
    _scratch: tempfile::TempDir,
}

impl VerifiedArchiveActivation {
    pub fn new_commit_ids(&self) -> &[ObjectId] {
        self.reentry
            .as_ref()
            .map_or(&[], |reentry| &reentry.graph.commits)
    }

    pub fn prepare_new_commit(&self, id: ObjectId) -> Result<PreparedObject> {
        let reentry = self
            .reentry
            .as_ref()
            .ok_or(Error::ArchiveArtifactMismatch)?;
        let object = PreparedObject::new(&reentry.store.get(id)?)?;
        if object.id() != id || object.kind() != 5 {
            return Err(Error::ArchiveArtifactMismatch);
        }
        Ok(object)
    }
}

fn archive_graph_error(error: git::Error) -> Error {
    match error {
        git::Error::Limit | git::Error::Source(treeharbor_core::Error::TooLarge) => {
            Error::UploadLimit
        }
        git::Error::Source(error) => Error::Objects(error),
        git::Error::RefHistoryDiverged(_) => Error::ArchiveHistoryDiverged,
        _ => Error::ArchiveArtifactMismatch,
    }
}

impl ArchiveActivate {
    pub fn verify(
        self,
        store: &ManagedStore,
        scratch: &Path,
        deadline: Instant,
    ) -> Result<VerifiedArchiveActivation> {
        self.proposals.archive.write(store, deadline, sink())?;
        let branches = self
            .branches
            .verify(store, scratch, deadline)
            .map_err(archive_graph_error)?;
        let proposals = self.proposals.verify(store, deadline)?;
        let reentry = self
            .history
            .map(|history| {
                let source = history.source(store)?;
                let previous = history.branch_mapping(&source, scratch, deadline)?;
                let imported = ArchiveStore {
                    store,
                    generations: &self.branches.generations,
                };
                let staged = tempfile::Builder::new()
                    .prefix(".treeharbor-archive-reentry-")
                    .permissions(std::fs::Permissions::from_mode(0o700))
                    .tempdir_in(scratch)
                    .map_err(|error| treeharbor_core::Error::Storage(error.to_string()))?;
                let output = LocalStore::open(staged.path(), &TenantId::new(store.tenant())?)?;
                let graph = git::reattach_branches(
                    &source,
                    &imported,
                    &output,
                    &previous,
                    &branches.mapping,
                    deadline,
                )
                .map_err(archive_graph_error)?;
                Ok::<_, Error>(Reentry {
                    history,
                    graph,
                    store: output,
                    _scratch: staged,
                })
            })
            .transpose()?;
        Ok(VerifiedArchiveActivation {
            stage_receipt: self.stage_receipt,
            proposals,
            fence: self.fence,
            branches: self.branches,
            reentry,
            deadline,
        })
    }
}

impl Metadata {
    pub async fn prepare_archive_activation(
        &mut self,
        auth: &Authorization,
        policy_auth: &Authorization,
        command: &Command,
        spec: &ArchiveActivateSpec,
        deadline: Instant,
    ) -> Result<ArchiveActivateWork> {
        let mutation = match self.begin(operation(auth, command, spec)?).await? {
            Begin::Replay(value) => return Ok(ArchiveActivateWork::Replay(value)),
            Begin::Fresh(mutation) => mutation,
        };
        let work = evidence(
            mutation.transaction(),
            auth,
            policy_auth,
            command,
            spec,
            deadline,
        )
        .await?;
        mutation.tx.commit().await?;
        Ok(ArchiveActivateWork::Verify(Box::new(work)))
    }

    pub async fn activate_archive(
        &mut self,
        auth: &Authorization,
        policy_auth: &Authorization,
        command: &Command,
        spec: &ArchiveActivateSpec,
        proof: &VerifiedArchiveActivation,
        leases: &[ObjectLease],
    ) -> Result<Value> {
        let mutation = match self.begin(operation(auth, command, spec)?).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let current = evidence(tx, auth, policy_auth, command, spec, proof.deadline).await?;
        if current.stage_receipt != proof.stage_receipt
            || current.proposals != proof.proposals.stage
            || current.fence != proof.fence
            || current.branches != proof.branches
            || current.history.as_ref() != proof.reentry.as_ref().map(|reentry| &reentry.history)
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        check_retained_proposals(tx, scope, &proof.stage_receipt, &proof.proposals).await?;
        let mut generations = BTreeMap::new();
        if leases.len() != proof.new_commit_ids().len() {
            return Err(Error::UploadMembership);
        }
        for (&id, lease) in proof.new_commit_ids().iter().zip(leases) {
            let key = lease.key();
            if key.tenant() != scope.tenant()
                || key.backend() != proof.proposals.stage.objects.backend
                || key.id() != id
                || generations.insert(id, key.generation()).is_some()
            {
                return Err(Error::UploadMembership);
            }
        }
        let result = effects::install(tx, auth, command, spec, proof, &generations).await?;
        current_archive_fence(
            tx,
            scope,
            command.epoch,
            &spec.observation_key,
            proof.proposals.stage.archive.observation(),
            &spec.proposal_stage,
        )
        .await?;
        policy_auth
            .check_transaction(tx, scope, auth.principal(), Permission::PolicyWrite)
            .await?;
        let binding = proof.fence.request.binding;
        let revoked = tx
            .execute(
                r#"
            UPDATE th_github_bindings SET state='revoked',revoked_at=clock_timestamp()
            WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND state='active'
            "#,
                &[
                    &scope.tenant(),
                    &scope.repository(),
                    &binding,
                    &command.epoch,
                ],
            )
            .await?;
        if revoked != 1 {
            return Err(Error::HostBindingState);
        }
        // Pending notifications belong to the old epoch. Keep their immutable
        // operation receipts, but give no stale worker a job in the new mode.
        tx.execute(r#"
            UPDATE th_outbox SET state='done',worker=NULL,lease_until=NULL,completed_at=clock_timestamp()
            WHERE tenant=$1 AND repository=$2 AND state<>'done'
            "#, &[&scope.tenant(), &scope.repository()]).await?;
        let changed = tx.execute(r#"
            UPDATE th_repositories SET authority='standalone',epoch=epoch+1
            WHERE tenant=$1 AND repository=$2 AND authority='connected' AND epoch=$3 AND state='active'
            "#, &[&scope.tenant(), &scope.repository(), &command.epoch]).await?;
        if changed != 1 {
            return Err(Error::Fenced);
        }
        mutation.complete(result, vec![]).await
    }
}
