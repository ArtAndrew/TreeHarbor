//! Trusted node cleanup protocol. Database removal observations are prerequisites,
//! never proof of quiescence or physical deletion. No agent-facing endpoint may
//! accept these observations. The physical node adapter must hold its OS guards.
use crate::{
    Authority, Error, Event, Metadata, Mutation, Request, Result, Scope,
    access::{Authorization, repository_lock},
    catalog::settings,
    preparations::StorageSlot,
    runs::{RunContext, load_context, node_auth},
    ttl_valid,
};
use serde_json::{Value, json};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{ObjectId, RevisionRef, SourceProfile};
use uuid::Uuid;

/// Original placement, including reservations which never completed preparation.
/// A missing manifest does not mean that its directory is empty or safe to adopt.
pub struct CleanupPlacement {
    pub attempt: Uuid,
    pub generation: i64,
    pub slot: StorageSlot,
    pub revision: RevisionRef,
    pub snapshot: ObjectId,
    pub profile: SourceProfile,
    pub manifest_digest: Option<[u8; 32]>,
    pub volume_digest: Option<[u8; 32]>,
}

/// Current cleanup lease, distinct from a preparation or execution lease.
pub struct CleanupPlan {
    id: Uuid,
    scope: Scope,
    workspace: Uuid,
    generation: i64,
    epoch: i64,
    authority: Authority,
    node: String,
    storage: Uuid,
    worker: Uuid,
    claim_generation: i64,
    placement: Option<CleanupPlacement>,
    runtime: Option<RunContext>,
    intent: Option<[u8; 32]>,
}
impl CleanupPlan {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn scope(&self) -> &Scope {
        &self.scope
    }
    pub fn workspace(&self) -> Uuid {
        self.workspace
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn epoch(&self) -> i64 {
        self.epoch
    }
    pub fn authority(&self) -> Authority {
        self.authority
    }
    pub fn node(&self) -> &str {
        &self.node
    }
    pub fn storage(&self) -> Uuid {
        self.storage
    }
    pub fn placement(&self) -> Option<&CleanupPlacement> {
        self.placement.as_ref()
    }
    pub fn runtime(&self) -> Option<&RunContext> {
        self.runtime.as_ref()
    }
    pub fn intent(&self) -> Option<[u8; 32]> {
        self.intent
    }
}

/// Trusted node observation of an fsynced deletion journal and completion record.
/// It is deliberately not an agent-provided path or an instruction to unlink.
pub struct DeletedFiles {
    pub intent_digest: [u8; 32],
    pub receipt_digest: [u8; 32],
}

fn digest(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| Error::SchemaMismatch)
}
fn mode(value: &str) -> Result<Authority> {
    match value {
        "connected" => Ok(Authority::Connected),
        "standalone" => Ok(Authority::Standalone),
        _ => Err(Error::SchemaMismatch),
    }
}
async fn record(tx: &Transaction<'_>, s: &Scope, workspace: Uuid) -> Result<Option<Row>> {
    Ok(tx.query_opt("SELECT *,lease_until>clock_timestamp() AS live,lease_until::text AS deadline FROM th_workspace_cleanups WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&s.tenant(),&s.repository(),&workspace]).await?)
}
async fn repository_current(
    tx: &Transaction<'_>,
    s: &Scope,
    authority: Authority,
    epoch: i64,
) -> Result<()> {
    if tx.query_opt("SELECT 1 FROM th_repositories WHERE tenant=$1 AND repository=$2 AND authority=$3 AND epoch=$4 AND state='active'", &[&s.tenant(),&s.repository(),&authority.as_str(),&epoch]).await?.is_none() {
        return Err(Error::Fenced);
    }
    Ok(())
}

/// Called under the repository write lock. Missing/unmanaged runtime evidence
/// quarantines resources, including when the workspace has no reserved slot.
async fn context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    workspace: Uuid,
    storage: Uuid,
) -> Result<(Row, Option<CleanupPlacement>, Option<RunContext>)> {
    let s = auth.scope();
    let w = tx.query_opt("SELECT * FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND state='draining' AND format='native'", &[&s.tenant(),&s.repository(),&workspace]).await?.ok_or(Error::ExecutionState)?;
    let epoch = w.get::<_, i64>("epoch");
    let generation = w.get::<_, i64>("generation");
    let rows = tx.query(r#"SELECT p.*,q.definition,b.snapshot AS base_snapshot,v.identity_digest
        FROM th_workspace_preparations p
        JOIN th_source_profiles q ON q.tenant=p.tenant AND q.repository=p.repository AND q.digest=p.profile
        JOIN th_bases b ON b.tenant=p.tenant AND b.repository=p.repository AND b.revision=p.base_revision
        LEFT JOIN th_node_volumes v ON v.storage_instance=p.storage_instance AND v.slot=p.slot
        WHERE p.tenant=$1 AND p.repository=$2 AND p.workspace=$3"#, &[&s.tenant(),&s.repository(),&workspace]).await?;
    if rows.len() > 1 {
        return Err(Error::ExecutionState);
    }
    let placement = if let Some(p) = rows.first() {
        let old_generation: i64 = p.get("generation");
        if p.get::<_, String>("state") != "draining"
            || old_generation.checked_add(1) != Some(generation)
            || p.get::<_, i64>("epoch") != epoch
            || p.get::<_, String>("node") != auth.principal()
            || p.get::<_, Uuid>("storage_instance") != storage
            || w.get::<_, Option<String>>("node").as_deref() != Some(auth.principal())
            || p.get::<_, String>("base_revision") != w.get::<_, String>("base_revision")
            || Some(p.get::<_, Vec<u8>>("profile"))
                != w.get::<_, Option<Vec<u8>>>("resolved_profile")
            || p.get::<_, Vec<u8>>("snapshot") != p.get::<_, Vec<u8>>("base_snapshot")
        {
            return Err(Error::ExecutionState);
        }
        let profile = SourceProfile::decode(&p.get::<_, Vec<u8>>("definition"))?;
        if profile.id().as_bytes() != &digest(p.get("profile"))? {
            return Err(Error::SchemaMismatch);
        }
        Some(CleanupPlacement {
            attempt: p.get("attempt"),
            generation: old_generation,
            slot: StorageSlot {
                storage,
                slot: p.get("slot"),
            },
            revision: p.get::<_, String>("base_revision").parse()?,
            snapshot: ObjectId::from_bytes(digest(p.get("snapshot"))?),
            profile,
            manifest_digest: p
                .get::<_, Option<Vec<u8>>>("manifest_digest")
                .map(digest)
                .transpose()?,
            volume_digest: p
                .get::<_, Option<Vec<u8>>>("identity_digest")
                .map(digest)
                .transpose()?,
        })
    } else {
        if w.get::<_, Option<String>>("node").is_some()
            || w.get::<_, Option<String>>("runtime").is_some()
        {
            return Err(Error::ExecutionState);
        }
        None
    };
    let runs = tx
        .query(
            r#"SELECT r.run,r.state,m.removed,m.preparation
        FROM th_agent_runs r LEFT JOIN th_managed_runs m USING(tenant,repository,run)
        WHERE r.tenant=$1 AND r.repository=$2 AND r.workspace=$3"#,
            &[&s.tenant(), &s.repository(), &workspace],
        )
        .await?;
    if runs.len() > 1 {
        return Err(Error::ExecutionState);
    }
    let runtime = if let Some(r) = runs.first() {
        if r.get::<_, Option<bool>>("removed") != Some(true)
            || !matches!(
                r.get::<_, String>("state").as_str(),
                "succeeded" | "failed" | "cancelled" | "lost"
            )
            || r.get::<_, Option<Uuid>>("preparation") != placement.as_ref().map(|p| p.attempt)
        {
            return Err(Error::ExecutionState);
        }
        let runtime = load_context(tx, auth, r.get("run"), storage).await?;
        if w.get::<_, Option<String>>("runtime")
            != runtime.container().map(|id| format!("docker:{id}"))
        {
            return Err(Error::ExecutionState);
        }
        Some(runtime)
    } else {
        if w.get::<_, Option<String>>("runtime").is_some() {
            return Err(Error::ExecutionState);
        }
        None
    };
    Ok((w, placement, runtime))
}

async fn lease(tx: &Transaction<'_>, auth: &Authorization, plan: &CleanupPlan) -> Result<Row> {
    if auth.scope().tenant() != plan.scope.tenant()
        || auth.scope().repository() != plan.scope.repository()
        || auth.principal() != plan.node
    {
        return Err(Error::Forbidden);
    }
    node_auth(tx, auth, plan.storage).await?;
    let r = record(tx, &plan.scope, plan.workspace)
        .await?
        .ok_or(Error::StaleLease)?;
    if r.get::<_, Uuid>("cleanup") != plan.id
        || r.get::<_, Uuid>("worker") != plan.worker
        || r.get::<_, i64>("claim_generation") != plan.claim_generation
        || !r.get::<_, bool>("live")
    {
        return Err(Error::StaleLease);
    }
    if r.get::<_, String>("node") != plan.node
        || r.get::<_, Uuid>("storage_instance") != plan.storage
        || r.get::<_, i64>("workspace_generation") != plan.generation
        || r.get::<_, i64>("epoch") != plan.epoch
        || r.get::<_, String>("authority") != plan.authority.as_str()
        || r.get::<_, Option<Uuid>>("preparation") != plan.placement.as_ref().map(|p| p.attempt)
        || r.get::<_, Option<Uuid>>("run") != plan.runtime.as_ref().map(RunContext::run)
    {
        return Err(Error::ExecutionState);
    }
    repository_current(tx, &plan.scope, plan.authority, plan.epoch).await?;
    Ok(r)
}
async fn checked(tx: &Transaction<'_>, auth: &Authorization, plan: &CleanupPlan) -> Result<Row> {
    let r = lease(tx, auth, plan).await?;
    if r.get::<_, String>("state") == "deleted" {
        return Err(Error::ExecutionState);
    }
    let (w, p, run) = context(tx, auth, plan.workspace, plan.storage).await?;
    if w.get::<_, i64>("generation") != plan.generation
        || w.get::<_, i64>("epoch") != plan.epoch
        || p.as_ref().map(|p| p.attempt) != plan.placement.as_ref().map(|p| p.attempt)
        || run.as_ref().map(RunContext::run) != plan.runtime.as_ref().map(RunContext::run)
    {
        return Err(Error::ExecutionState);
    }
    Ok(r)
}
fn response(r: &Row) -> Value {
    json!({"cleanup":r.get::<_,Uuid>("cleanup").to_string(),
        "workspace":r.get::<_,Uuid>("workspace").to_string(),
        "generation":r.get::<_,i64>("workspace_generation"),"state":"deleted"})
}

impl Metadata {
    /// Explicit workspace selection; automatic scheduling is a separate layer.
    /// None means another live claim or an already completed cleanup. A caller
    /// must query the completed receipt before opening a previously occupied slot.
    pub async fn claim_workspace_cleanup(
        &mut self,
        auth: &Authorization,
        workspace: Uuid,
        storage: Uuid,
        worker: Uuid,
        ttl: i32,
    ) -> Result<Option<CleanupPlan>> {
        ttl_valid(ttl)?;
        if workspace.is_nil() || storage.is_nil() || worker.is_nil() {
            return Err(Error::Invalid("cleanup identity"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        node_auth(&tx, auth, storage).await?;
        if let Some(r) = record(&tx, s, workspace).await? {
            if r.get::<_, String>("node") != auth.principal()
                || r.get::<_, Uuid>("storage_instance") != storage
            {
                return Err(Error::Forbidden);
            }
            if r.get::<_, String>("state") == "deleted"
                || (r.get::<_, bool>("live") && r.get::<_, Uuid>("worker") != worker)
            {
                node_auth(&tx, auth, storage).await?;
                tx.commit().await?;
                return Ok(None);
            }
        }
        let (w, placement, runtime) = context(&tx, auth, workspace, storage).await?;
        let epoch = w.get("epoch");
        let generation = w.get("generation");
        let authority = mode(
            &tx.query_one(
                "SELECT authority FROM th_repositories WHERE tenant=$1 AND repository=$2",
                &[&s.tenant(), &s.repository()],
            )
            .await?
            .get::<_, String>(0),
        )?;
        repository_current(&tx, s, authority, epoch).await?;
        if runtime
            .as_ref()
            .is_some_and(|r| r.prepared().authority != authority)
        {
            return Err(Error::Fenced);
        }
        tx.execute(r#"INSERT INTO th_workspace_cleanups(tenant,repository,workspace,cleanup,workspace_generation,epoch,authority,node,storage_instance,preparation,run,state,claim_generation,worker,lease_until)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'claimed',1,$12,clock_timestamp()+($13::integer*interval '1 second'))
            ON CONFLICT(tenant,repository,workspace) DO UPDATE SET
                worker=EXCLUDED.worker,claim_generation=th_workspace_cleanups.claim_generation+1,
                lease_until=EXCLUDED.lease_until
            WHERE th_workspace_cleanups.lease_until<=clock_timestamp() AND th_workspace_cleanups.state<>'deleted'"#,
            &[&s.tenant(),&s.repository(),&workspace,&Uuid::new_v4(),&generation,&epoch,&authority.as_str(),&auth.principal(),&storage,
                &placement.as_ref().map(|p|p.attempt),&runtime.as_ref().map(RunContext::run),&worker,&ttl]).await?;
        let r = record(&tx, s, workspace)
            .await?
            .ok_or(Error::SchemaMismatch)?;
        let plan = CleanupPlan {
            id: r.get("cleanup"),
            scope: s.clone(),
            workspace,
            generation,
            epoch,
            authority,
            node: auth.principal().into(),
            storage,
            worker,
            claim_generation: r.get("claim_generation"),
            placement,
            runtime,
            intent: r
                .get::<_, Option<Vec<u8>>>("intent_digest")
                .map(digest)
                .transpose()?,
        };
        checked(&tx, auth, &plan).await?;
        tx.commit().await?;
        Ok(Some(plan))
    }

    pub async fn renew_workspace_cleanup(
        &mut self,
        auth: &Authorization,
        plan: &CleanupPlan,
        ttl: i32,
    ) -> Result<()> {
        ttl_valid(ttl)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), true).await?;
        let old_deadline: String = checked(&tx, auth, plan).await?.get("deadline");
        // The old deadline must still hold when the renewal write executes.
        if tx.execute("UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()+($4::integer*interval '1 second') WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND lease_until>clock_timestamp()",
            &[&plan.scope.tenant(),&plan.scope.repository(),&plan.workspace,&ttl]).await?!=1 { return Err(Error::StaleLease); }
        checked(&tx, auth, plan).await?;
        if !tx
            .query_one(
                "SELECT $1::text::timestamptz>clock_timestamp()",
                &[&old_deadline],
            )
            .await?
            .get::<_, bool>(0)
        {
            return Err(Error::StaleLease);
        }
        tx.commit().await?;
        Ok(())
    }

    /// Bind the exact durable local intent before deleting any bytes. Reclaim
    /// preserves this digest and cannot admit a different deletion target.
    pub async fn admit_workspace_cleanup(
        &mut self,
        auth: &Authorization,
        plan: &CleanupPlan,
        intent: [u8; 32],
    ) -> Result<()> {
        if intent == [0; 32] {
            return Err(Error::Invalid("cleanup intent"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), true).await?;
        let r = checked(&tx, auth, plan).await?;
        if let Some(saved) = r.get::<_, Option<Vec<u8>>>("intent_digest") {
            if saved != intent {
                return Err(Error::IdempotencyConflict);
            }
        } else {
            tx.execute("UPDATE th_workspace_cleanups SET state='deleting',intent_digest=$4 WHERE tenant=$1 AND repository=$2 AND workspace=$3",
                &[&plan.scope.tenant(),&plan.scope.repository(),&plan.workspace,&intent.as_slice()]).await?;
        }
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(())
    }

    /// This method accepts only an observation from the trusted node adapter.
    /// It is not evidence that arbitrary caller-supplied filesystem IO is safe.
    pub async fn complete_workspace_cleanup(
        &mut self,
        auth: &Authorization,
        plan: &CleanupPlan,
        proof: &DeletedFiles,
    ) -> Result<Value> {
        if proof.intent_digest == [0; 32] || proof.receipt_digest == [0; 32] {
            return Err(Error::Invalid("cleanup receipt"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = &plan.scope;
        repository_lock(&tx, s, true).await?;
        if auth.scope().tenant() != s.tenant()
            || auth.scope().repository() != s.repository()
            || auth.principal() != plan.node
        {
            return Err(Error::Forbidden);
        }
        node_auth(&tx, auth, plan.storage).await?;
        let saved = record(&tx, s, plan.workspace)
            .await?
            .ok_or(Error::NotFound)?;
        if saved.get::<_, String>("state") == "deleted" {
            if saved.get::<_, Uuid>("cleanup") != plan.id
                || saved.get::<_, Uuid>("worker") != plan.worker
                || saved.get::<_, i64>("claim_generation") != plan.claim_generation
            {
                return Err(Error::StaleLease);
            }
            if saved.get::<_, Option<Vec<u8>>>("intent_digest").as_deref()
                != Some(proof.intent_digest.as_slice())
                || saved.get::<_, Option<Vec<u8>>>("receipt_digest").as_deref()
                    != Some(proof.receipt_digest.as_slice())
            {
                return Err(Error::IdempotencyConflict);
            }
            let result = response(&saved);
            node_auth(&tx, auth, plan.storage).await?;
            tx.commit().await?;
            return Ok(result);
        }
        let r = checked(&tx, auth, plan).await?;
        if r.get::<_, String>("state") != "deleting"
            || r.get::<_, Option<Vec<u8>>>("intent_digest").as_deref()
                != Some(proof.intent_digest.as_slice())
        {
            return Err(Error::ExecutionState);
        }
        let request = Request::new(
            s.clone(),
            &format!("workspace-cleanup:{}", plan.id),
            &plan.node,
            "workspace.cleanup.complete.v1",
            plan.authority,
            plan.epoch,
            &serde_json::to_vec(&json!([
                plan.id.to_string(),
                proof.intent_digest,
                proof.receipt_digest
            ]))
            .map_err(|_| Error::Invalid("cleanup request"))?,
        )?;
        let mutation = Mutation { tx, request };
        let holder:String=mutation.tx.query_one("SELECT pin_holder FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&s.tenant(),&s.repository(),&plan.workspace]).await?.get(0);
        mutation.tx.execute("UPDATE th_workspaces SET state='deleted',pin_holder=NULL WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&s.tenant(),&s.repository(),&plan.workspace]).await?;
        mutation.unpin_object(&holder).await?;
        if let Some(p) = &plan.placement
            && mutation.tx.execute("UPDATE th_workspace_preparations SET state='released' WHERE attempt=$1 AND state='draining'", &[&p.attempt]).await?!=1 {
            return Err(Error::ExecutionState);
        }
        mutation.tx.execute("UPDATE th_workspace_cleanups SET state='deleted',receipt_digest=$4,completed_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&s.tenant(),&s.repository(),&plan.workspace,&proof.receipt_digest.as_slice()]).await?;
        let result = response(&r);
        mutation
            .write_completion(
                &result,
                &[Event {
                    kind: "workspace.deleted".into(),
                    payload: result.clone(),
                }],
            )
            .await?;
        // Check time/credentials AFTER every write, including pin locks and outbox
        // triggers. Failed acknowledgment rolls all releases back together.
        lease(&mutation.tx, auth, plan).await?;
        mutation.tx.commit().await?;
        Ok(result)
    }

    /// Current assigned node may recover a completed response without touching
    /// the old slot, including after a later authority migration or slot reuse.
    pub async fn completed_workspace_cleanup(
        &mut self,
        auth: &Authorization,
        workspace: Uuid,
        storage: Uuid,
    ) -> Result<Option<Value>> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        node_auth(&tx, auth, storage).await?;
        let result = if let Some(r) = record(&tx, s, workspace).await? {
            if r.get::<_, String>("node") != auth.principal()
                || r.get::<_, Uuid>("storage_instance") != storage
            {
                return Err(Error::Forbidden);
            }
            (r.get::<_, String>("state") == "deleted").then(|| response(&r))
        } else {
            None
        };
        node_auth(&tx, auth, storage).await?;
        tx.commit().await?;
        Ok(result)
    }
}
