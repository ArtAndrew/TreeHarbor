//! Durable owner intent and node claims for stopped-file capture. A claim is not
//! runtime quiescence, a ready source artifact, or an accepted ChangeSet.
mod acceptance;
mod objects;
use crate::{
    Begin, Error, Event, Metadata, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
    intake::CommandContext,
    runs::{RunContext, load_context, node_auth},
    ttl_valid,
    uploads::UploadLimits,
};
pub use acceptance::{SealAcceptCommand, SealAcceptRead, SealAcceptStart};
pub use objects::{SealObjectTicket, SealPublication, SealSource};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{CaptureSelection, SourceProfile};
use uuid::Uuid;

pub struct SealRequest {
    pub seal: Uuid,
    pub run: Uuid,
    pub generation: i64,
    pub selection: CaptureSelection,
    pub limits: UploadLimits,
}
/// Exact leased capture context. The node must prove stopped writers and physical
/// placement before using this to capture anything. No live execution job exists.
pub struct SealPlan {
    id: Uuid,
    generation: i64,
    worker: Uuid,
    context: RunContext,
    selection: CaptureSelection,
    binding: [u8; 32],
    max_objects: i64,
    max_bytes: i64,
}
impl SealPlan {
    pub fn id(&self) -> Uuid {
        self.id
    }
    pub fn context(&self) -> &RunContext {
        &self.context
    }
    pub fn selection(&self) -> &CaptureSelection {
        &self.selection
    }
    pub fn binding(&self) -> [u8; 32] {
        self.binding
    }
    pub fn max_objects(&self) -> i64 {
        self.max_objects
    }
    pub fn max_bytes(&self) -> i64 {
        self.max_bytes
    }
}
fn response(row: &Row) -> Value {
    json!({"seal":row.get::<_,Uuid>("seal").to_string(),
        "run":row.get::<_,Uuid>("run").to_string(),
        "generation":row.get::<_,i64>("workspace_generation"),
        "epoch":row.get::<_,i64>("epoch"),"state":row.get::<_,String>("state"),
        "unexpired":row.get::<_,bool>("unexpired"),
        "failure":row.get::<_,Option<String>>("failure"),
        "manifest_digest":row.get::<_,Option<Vec<u8>>>("manifest_digest").map(|b| hex(&b)),
        "operations":row.get::<_,Option<Vec<u8>>>("operations").map(|b| format!("sha256:{}",hex(&b)))})
}
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                DIGITS[(b >> 4) as usize] as char,
                DIGITS[(b & 15) as usize] as char,
            ]
        })
        .collect()
}
async fn record(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired FROM th_workspace_seals WHERE tenant=$1 AND repository=$2 AND seal=$3",
        &[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
/// An accepted asynchronous request uses its owner's current principal/grant;
/// a node's runtime credential never lends the owner additional permissions.
async fn owner_policy(tx: &Transaction<'_>, s: &Scope, owner: &str) -> Result<()> {
    let actor = tx
        .query_opt(
            "SELECT enabled,kind FROM th_principals WHERE tenant=$1 AND principal=$2 FOR SHARE",
            &[&s.tenant(), &owner],
        )
        .await?
        .ok_or(Error::Forbidden)?;
    if !actor.get::<_, bool>(0) || !matches!(actor.get::<_, String>(1).as_str(), "human" | "agent")
    {
        return Err(Error::Forbidden);
    }
    let grant=tx.query_opt("SELECT permissions FROM th_repository_access WHERE tenant=$1 AND repository=$2 AND principal=$3 FOR SHARE",&[&s.tenant(),&s.repository(),&owner]).await?.ok_or(Error::Forbidden)?;
    if !grant
        .get::<_, Vec<String>>(0)
        .iter()
        .any(|p| p == "workspace.write")
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}
/// Validate mutable authority/workspace/owner fences separately from immutable
/// execution identity. Removed metadata remains only a prerequisite for the node.
async fn current(tx: &Transaction<'_>, s: &Scope, seal: &Row, removed: bool) -> Result<()> {
    let owner: String = seal.get("principal");
    owner_policy(tx, s, &owner).await?;
    let row=tx.query_opt(r#"SELECT m.removed,r.state FROM th_managed_runs m
        JOIN th_agent_runs r USING(tenant,repository,run)
        JOIN th_workspaces w USING(tenant,repository,workspace)
        JOIN th_workspace_preparations p ON p.attempt=m.preparation
        JOIN th_repositories repo ON repo.tenant=m.tenant AND repo.repository=m.repository
        WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3 AND r.principal=$4 AND w.principal=$4
        AND r.workspace_generation=$5 AND w.generation=$5 AND p.generation=$5
        AND r.epoch=$6 AND w.epoch=$6 AND p.epoch=$6 AND repo.epoch=$6
        AND m.authority=$7 AND repo.authority=$7 AND repo.state='active'
        AND w.state='in_use' AND w.format='native' AND p.state='prepared'
        AND w.node=m.node AND p.node=m.node AND p.workspace=w.workspace
        AND p.base_revision=w.base_revision AND p.profile=w.resolved_profile
        AND NOT EXISTS(SELECT 1 FROM th_check_executions c WHERE c.tenant=m.tenant AND c.repository=m.repository AND c.execution_run=m.run)"#,
        &[&s.tenant(),&s.repository(),&seal.get::<_,Uuid>("run"),&owner,&seal.get::<_,i64>("workspace_generation"),&seal.get::<_,i64>("epoch"),&seal.get::<_,String>("authority")]).await?.ok_or(Error::ExecutionState)?;
    if removed
        && (!row.get::<_, bool>(0)
            || !matches!(
                row.get::<_, String>(1).as_str(),
                "succeeded" | "failed" | "cancelled" | "lost"
            ))
    {
        return Err(Error::ExecutionState);
    }
    Ok(())
}
pub(crate) fn binding(
    s: &Scope,
    id: Uuid,
    context: &RunContext,
    selection: &CaptureSelection,
    objects: i64,
    bytes: i64,
) -> [u8; 32] {
    let p = context.prepared();
    let data = json!([
        1,
        s.tenant(),
        s.repository().to_string(),
        id.to_string(),
        context.run().to_string(),
        context.attempt().to_string(),
        hex(&context.binding()),
        p.workspace.to_string(),
        p.generation,
        p.epoch,
        p.authority.as_str(),
        p.node,
        p.slot.storage.to_string(),
        p.slot.slot.to_string(),
        hex(&context.volume_digest()),
        p.attempt.to_string(),
        hex(&p.manifest_digest),
        p.revision.to_string(),
        p.snapshot.to_string(),
        hex(p.profile.id().as_bytes()),
        selection.digest_text(),
        objects,
        bytes
    ])
    .to_string();
    let mut h = Sha256::new();
    h.update(b"treeharbor-seal-binding\0v1\0");
    h.update(data.as_bytes());
    h.finalize().into()
}
async fn checked(tx: &Transaction<'_>, auth: &Authorization, plan: &SealPlan) -> Result<Row> {
    let p = plan.context.prepared();
    let s = auth.scope();
    if p.scope.tenant() != s.tenant()
        || p.scope.repository() != s.repository()
        || p.node != auth.principal()
    {
        return Err(Error::Forbidden);
    }
    node_auth(tx, auth, p.slot.storage).await?;
    let r = record(tx, s, plan.id).await?;
    let live:bool=tx.query_one("SELECT state='capturing' AND worker=$4 AND claim_generation=$5 AND lease_until>clock_timestamp() AND expires_at>clock_timestamp() FROM th_workspace_seals WHERE tenant=$1 AND repository=$2 AND seal=$3",
        &[&s.tenant(),&s.repository(),&plan.id,&plan.worker,&plan.generation]).await?.get(0);
    if !live {
        return Err(Error::StaleLease);
    }
    current(tx, s, &r, true).await?;
    // These values came from a private plan, but recheck durable inputs as well.
    if r.get::<_, Uuid>("run") != plan.context.run()
        || r.get::<_, Vec<u8>>("selection") != plan.selection.encode()
    {
        return Err(Error::SchemaMismatch);
    }
    Ok(r)
}
impl Metadata {
    pub async fn request_workspace_seal(
        &mut self,
        ctx: &CommandContext,
        input: &SealRequest,
    ) -> Result<Value> {
        input.limits.validate()?;
        if input.seal.is_nil() || input.run.is_nil() || input.generation <= 0 {
            return Err(Error::Invalid("seal identity"));
        }
        let request = ctx.request(
            "workspace.seal.request.v1",
            json!([
                1,
                input.seal.to_string(),
                input.run.to_string(),
                input.generation,
                input.selection.digest_text(),
                input.limits.objects,
                input.limits.bytes,
                input.limits.ttl_seconds
            ]),
        )?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let tx = &mutation.tx;
        let s = &ctx.scope;
        let source=tx.query_opt(r#"SELECT w.pin_holder,q.definition,m.removed,r.state FROM th_managed_runs m
            JOIN th_agent_runs r USING(tenant,repository,run) JOIN th_workspaces w USING(tenant,repository,workspace)
            JOIN th_workspace_preparations p ON p.attempt=m.preparation
            JOIN th_source_profiles q ON q.tenant=p.tenant AND q.repository=p.repository AND q.digest=p.profile
            WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3 AND r.principal=$4 AND w.principal=$4
            AND r.workspace_generation=$5 AND w.generation=$5 AND r.epoch=$6 AND w.epoch=$6
            AND m.authority=$7 AND w.state='in_use' AND p.state='prepared' AND w.format='native'
            AND NOT EXISTS(SELECT 1 FROM th_check_executions c WHERE c.tenant=m.tenant AND c.repository=m.repository AND c.execution_run=m.run)"#,
            &[&s.tenant(),&s.repository(),&input.run,&ctx.principal,&input.generation,&ctx.epoch,&ctx.authority.as_str()]).await?.ok_or(Error::ExecutionState)?;
        let profile = SourceProfile::decode(&source.get::<_, Vec<u8>>("definition"))?;
        if let Some(paths) = profile.paths()
            && input
                .selection
                .replace()
                .iter()
                .chain(input.selection.delete())
                .any(|p| !paths.iter().any(|root| root == p || root.is_ancestor_of(p)))
        {
            return Err(Error::Invalid(
                "seal selection is outside complete source coverage",
            ));
        }
        if tx.query_opt("SELECT 1 FROM th_workspace_seals WHERE tenant=$1 AND repository=$2 AND (seal=$3 OR (run=$4 AND state IN ('waiting','capturing')))", &[&s.tenant(),&s.repository(),&input.seal,&input.run]).await?.is_some() { return Err(Error::ExecutionState); }
        let base_pin = format!("seal/{}/base", input.seal);
        mutation
            .copy_root_pin(&source.get::<_, String>("pin_holder"), &base_pin)
            .await?;
        tx.execute(r#"INSERT INTO th_workspace_seals(tenant,repository,seal,principal,run,workspace_generation,authority,epoch,selection,base_pin,max_objects,max_bytes,expires_at,state)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,clock_timestamp()+($13::integer*interval '1 second'),'waiting')"#,
            &[&s.tenant(),&s.repository(),&input.seal,&ctx.principal,&input.run,&input.generation,&ctx.authority.as_str(),&ctx.epoch,&input.selection.encode(),&base_pin,&input.limits.objects,&input.limits.bytes,&input.limits.ttl_seconds]).await?;
        let row = record(tx, s, input.seal).await?;
        current(tx, s, &row, false).await?;
        if !source.get::<_, bool>("removed") {
            tx.execute("UPDATE th_managed_runs SET stop_requested=true WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&input.run]).await?;
            tx.execute("UPDATE th_agent_runs SET state='stopping' WHERE tenant=$1 AND repository=$2 AND run=$3 AND state IN ('requested','launching','running')",&[&s.tenant(),&s.repository(),&input.run]).await?;
        }
        mutation
            .complete(
                response(&row),
                vec![Event {
                    kind: "workspace.seal.requested".into(),
                    payload: json!({"seal":input.seal.to_string(),"run":input.run.to_string()}),
                }],
            )
            .await
    }
    pub async fn workspace_seal_status(&mut self, auth: &Authorization, id: Uuid) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let r = record(&tx, s, id).await?;
        if r.get::<_, String>("principal") != auth.principal() {
            return Err(Error::NotFound);
        }
        let result = response(&r);
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn cancel_workspace_seal(&mut self, ctx: &CommandContext, id: Uuid) -> Result<Value> {
        let request = ctx.request("workspace.seal.cancel.v1", json!([1, id.to_string()]))?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let r = record(&mutation.tx, s, id).await?;
        if r.get::<_, String>("principal") != ctx.principal {
            return Err(Error::NotFound);
        }
        if r.get::<_, i64>("epoch") != ctx.epoch {
            return Err(Error::Fenced);
        }
        mutation.tx.execute("UPDATE th_workspace_seals SET state='cancelled',worker=NULL,lease_until=NULL,completed_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND seal=$3 AND state IN ('waiting','capturing')", &[&s.tenant(),&s.repository(),&id]).await?;
        let result = response(&record(&mutation.tx, s, id).await?);
        mutation.complete(result, vec![]).await
    }
    /// Claims only after managed removal has been recorded. The node must still
    /// reopen the exact journal/Engine and exclude all volume writers locally.
    pub async fn claim_workspace_seal(
        &mut self,
        auth: &Authorization,
        storage: Uuid,
        worker: Uuid,
        ttl: i32,
    ) -> Result<Option<SealPlan>> {
        ttl_valid(ttl)?;
        if worker.is_nil() {
            return Err(Error::Invalid("seal worker"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        node_auth(&tx, auth, storage).await?;
        let row=tx.query_opt(r#"SELECT c.seal FROM th_workspace_seals c JOIN th_managed_runs m USING(tenant,repository,run)
            JOIN th_agent_runs r USING(tenant,repository,run) JOIN th_workspaces w USING(tenant,repository,workspace)
            JOIN th_workspace_preparations p ON p.attempt=m.preparation
            JOIN th_repositories repo ON repo.tenant=c.tenant AND repo.repository=c.repository
            JOIN th_principals a ON a.tenant=c.tenant AND a.principal=c.principal
            JOIN th_repository_access g ON g.tenant=c.tenant AND g.repository=c.repository AND g.principal=c.principal
            WHERE c.tenant=$1 AND c.repository=$2 AND m.node=$3 AND m.storage_instance=$4 AND m.removed
            AND c.expires_at>clock_timestamp() AND (c.state='waiting' OR(c.state='capturing' AND c.lease_until<=clock_timestamp()))
            AND w.state='in_use' AND p.state='prepared' AND w.generation=c.workspace_generation AND w.epoch=c.epoch
            AND repo.state='active' AND repo.epoch=c.epoch AND repo.authority=c.authority
            AND a.enabled AND 'workspace.write'=ANY(g.permissions)
            ORDER BY c.created_at,c.seal LIMIT 1 FOR UPDATE OF c SKIP LOCKED"#,
            &[&s.tenant(),&s.repository(),&auth.principal(),&storage]).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let id: Uuid = row.get(0);
        let r = record(&tx, s, id).await?;
        current(&tx, s, &r, true).await?;
        let context = load_context(&tx, auth, r.get("run"), storage).await?;
        if context.is_check() {
            return Err(Error::Forbidden);
        }
        let selection = CaptureSelection::decode(&r.get::<_, Vec<u8>>("selection"))?;
        let max_objects = r.get("max_objects");
        let max_bytes = r.get("max_bytes");
        let binding = binding(s, id, &context, &selection, max_objects, max_bytes);
        let row=tx.query_one("UPDATE th_workspace_seals SET state='capturing',worker=$4,claim_generation=claim_generation+1,lease_until=LEAST(expires_at,clock_timestamp()+($5::integer*interval '1 second')) WHERE tenant=$1 AND repository=$2 AND seal=$3 RETURNING claim_generation",
            &[&s.tenant(),&s.repository(),&id,&worker,&ttl]).await?;
        let plan = SealPlan {
            id,
            generation: row.get(0),
            worker,
            context,
            selection,
            binding,
            max_objects,
            max_bytes,
        };
        checked(&tx, auth, &plan).await?;
        tx.commit().await?;
        Ok(Some(plan))
    }
    pub async fn renew_workspace_seal(
        &mut self,
        auth: &Authorization,
        plan: &SealPlan,
        ttl: i32,
    ) -> Result<()> {
        ttl_valid(ttl)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        checked(&tx, auth, plan).await?;
        let changed=tx.execute("UPDATE th_workspace_seals SET lease_until=LEAST(expires_at,clock_timestamp()+($6::integer*interval '1 second')) WHERE tenant=$1 AND repository=$2 AND seal=$3 AND worker=$4 AND claim_generation=$5 AND state='capturing' AND lease_until>clock_timestamp() AND expires_at>clock_timestamp()",
            &[&s.tenant(),&s.repository(),&plan.id,&plan.worker,&plan.generation,&ttl]).await?;
        if changed != 1 {
            return Err(Error::StaleLease);
        }
        checked(&tx, auth, plan).await?;
        tx.commit().await?;
        Ok(())
    }
}
