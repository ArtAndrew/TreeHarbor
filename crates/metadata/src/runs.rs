//! Owner-authorized run requests and trusted node observations. No method in this
//! module executes code or turns a task exit into a repository Validation.
use crate::{
    Authority, Begin, Error, Event, Job, Metadata, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
    identifier,
    intake::CommandContext,
    preparations::{PreparedWorkspace, StorageSlot},
    ttl_valid,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{ObjectId, SourceProfile};
use treeharbor_runtime::{Inspection, Profile, RuntimeState, Task};
use uuid::Uuid;

pub struct RunRequest {
    pub run: Uuid,
    pub workspace: Uuid,
    pub generation: i64,
    pub profile: [u8; 32],
    pub argv: Vec<String>,
    pub agent: String,
    pub task: String,
}

pub struct RuntimeVolume {
    pub slot: StorageSlot,
    pub digest: [u8; 32],
    pub limit_bytes: u64,
}
/// Immutable managed execution identity. This is not a lease, start permission,
/// or proof that the runtime has actually stopped on its assigned node.
pub struct RunContext {
    run: Uuid,
    attempt: Uuid,
    binding: [u8; 32],
    volume_digest: [u8; 32],
    prepared: PreparedWorkspace,
    profile: Profile,
    task: Task,
    check: bool,
    engine: Option<String>,
    container: Option<String>,
}
impl RunContext {
    pub fn is_check(&self) -> bool {
        self.check
    }
    pub fn run(&self) -> Uuid {
        self.run
    }
    pub fn attempt(&self) -> Uuid {
        self.attempt
    }
    pub fn binding(&self) -> [u8; 32] {
        self.binding
    }
    pub fn volume_digest(&self) -> [u8; 32] {
        self.volume_digest
    }
    pub fn prepared(&self) -> &PreparedWorkspace {
        &self.prepared
    }
    pub fn profile(&self) -> &Profile {
        &self.profile
    }
    pub fn task(&self) -> &Task {
        &self.task
    }
    pub fn engine(&self) -> Option<&str> {
        self.engine.as_deref()
    }
    pub fn container(&self) -> Option<&str> {
        self.container.as_deref()
    }
}
pub struct RunPlan {
    job: Job,
    context: RunContext,
    stop: bool,
}
impl RunPlan {
    pub fn is_check(&self) -> bool {
        self.context.is_check()
    }
    pub fn run(&self) -> Uuid {
        self.context.run()
    }
    pub fn attempt(&self) -> Uuid {
        self.context.attempt()
    }
    pub fn binding(&self) -> [u8; 32] {
        self.context.binding()
    }
    pub fn volume_digest(&self) -> [u8; 32] {
        self.context.volume_digest()
    }
    pub fn prepared(&self) -> &PreparedWorkspace {
        self.context.prepared()
    }
    pub fn profile(&self) -> &Profile {
        self.context.profile()
    }
    pub fn task(&self) -> &Task {
        self.context.task()
    }
    pub fn stop_requested(&self) -> bool {
        self.stop
    }
}
fn digest(bytes: Vec<u8>) -> Result<[u8; 32]> {
    bytes.try_into().map_err(|_| Error::SchemaMismatch)
}
fn profile(bytes: Vec<u8>) -> Result<Profile> {
    Profile::decode(&bytes).map_err(|_| Error::SchemaMismatch)
}
fn task(bytes: Vec<u8>) -> Result<Task> {
    let argv: Vec<String> = serde_json::from_slice(&bytes).map_err(|_| Error::SchemaMismatch)?;
    if serde_json::to_vec(&argv).map_err(|_| Error::SchemaMismatch)? != bytes {
        return Err(Error::SchemaMismatch);
    }
    Task::new(argv).map_err(|_| Error::SchemaMismatch)
}
fn authority(value: &str) -> Result<Authority> {
    match value {
        "connected" => Ok(Authority::Connected),
        "standalone" => Ok(Authority::Standalone),
        _ => Err(Error::SchemaMismatch),
    }
}
pub(crate) async fn node_auth(
    tx: &Transaction<'_>,
    auth: &Authorization,
    storage: Uuid,
) -> Result<()> {
    auth.check_transaction(
        tx,
        auth.scope(),
        auth.principal(),
        Permission::RuntimeControl,
    )
    .await?;
    if tx
        .query_opt(
            "SELECT 1 FROM th_node_storage WHERE tenant=$1 AND node=$2 AND storage_instance=$3",
            &[&auth.scope().tenant(), &auth.principal(), &storage],
        )
        .await?
        .is_none()
    {
        return Err(Error::Fenced);
    }
    Ok(())
}
struct RunBinding<'a> {
    scope: &'a Scope,
    run: Uuid,
    attempt: Uuid,
    preparation: Uuid,
    owner: &'a str,
    authority: Authority,
    epoch: i64,
    generation: i64,
    profile: [u8; 32],
    argv: &'a [String],
    volume: [u8; 32],
}
fn binding(input: RunBinding<'_>) -> [u8; 32] {
    let RunBinding {
        scope,
        run,
        attempt,
        preparation,
        owner,
        authority,
        epoch,
        generation,
        profile,
        argv,
        volume,
    } = input;
    let bytes = json!([
        1,
        scope.tenant(),
        scope.repository().to_string(),
        run.to_string(),
        attempt.to_string(),
        preparation.to_string(),
        owner,
        authority.as_str(),
        epoch,
        generation,
        profile,
        argv,
        volume
    ])
    .to_string();
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-run-binding\0v1\0");
    hash.update(bytes.as_bytes());
    hash.finalize().into()
}
impl Metadata {
    /// Trusted operator bootstrap; never exposed as an owner/agent command.
    pub async fn register_runtime_profile(
        &mut self,
        scope: &Scope,
        p: &Profile,
        operator: &str,
    ) -> Result<()> {
        identifier(operator, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, true).await?;
        let id = p.digest();
        let bytes = p.encode();
        let changed=tx.execute("INSERT INTO th_runtime_profiles(tenant,repository,digest,definition) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING",
            &[&scope.tenant(),&scope.repository(),&id.as_slice(),&bytes]).await?;
        let actual:Vec<u8>=tx.query_one("SELECT definition FROM th_runtime_profiles WHERE tenant=$1 AND repository=$2 AND digest=$3",
            &[&scope.tenant(),&scope.repository(),&id.as_slice()]).await?.get(0);
        if actual != bytes {
            return Err(Error::SchemaMismatch);
        }
        if changed == 1 {
            tx.execute("INSERT INTO th_security_audit(tenant,repository,operator,principal,action) VALUES($1,$2,$3,$3,'runtime.profile.register')",&[&scope.tenant(),&scope.repository(),&operator]).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn revoke_runtime_profile(
        &mut self,
        scope: &Scope,
        id: [u8; 32],
        operator: &str,
    ) -> Result<()> {
        identifier(operator, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, true).await?;
        let changed=tx.execute("INSERT INTO th_runtime_profile_revocations(tenant,repository,digest) VALUES($1,$2,$3) ON CONFLICT DO NOTHING",&[&scope.tenant(),&scope.repository(),&id.as_slice()]).await?;
        if changed == 1 {
            tx.execute("INSERT INTO th_security_audit(tenant,repository,operator,principal,action) VALUES($1,$2,$3,$3,'runtime.profile.revoke')",&[&scope.tenant(),&scope.repository(),&operator]).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Register only after the trusted operator has verified the actual Node volume.
    pub async fn register_runtime_volume(
        &mut self,
        tenant: &str,
        node: &str,
        volume: &RuntimeVolume,
        operator: &str,
    ) -> Result<()> {
        crate::tenant_valid(tenant)?;
        identifier(node, 128)?;
        identifier(operator, 128)?;
        if volume.slot.slot.is_nil()
            || volume.slot.storage.is_nil()
            || !(67108864..=68719476736).contains(&volume.limit_bytes)
        {
            return Err(Error::Invalid("runtime volume"));
        }
        let limit = volume.limit_bytes as i64;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        if tx.query_opt("SELECT 1 FROM th_node_storage WHERE tenant=$1 AND node=$2 AND storage_instance=$3 FOR UPDATE",
            &[&tenant,&node,&volume.slot.storage]).await?.is_none() {
            return Err(Error::Fenced);
        }
        // Registration precedes every reservation of this slot. Never reinterpret
        // an already reserved legacy directory as a bounded runtime placement.
        if tx
            .query_opt(
                "SELECT 1 FROM th_node_volumes WHERE storage_instance=$1 AND slot=$2",
                &[&volume.slot.storage, &volume.slot.slot],
            )
            .await?
            .is_none()
            && tx
                .query_opt(
                    "SELECT 1 FROM th_workspace_preparations WHERE storage_instance=$1 AND slot=$2",
                    &[&volume.slot.storage, &volume.slot.slot],
                )
                .await?
                .is_some()
        {
            return Err(Error::ExecutionState);
        }
        let changed=tx.execute("INSERT INTO th_node_volumes(tenant,node,storage_instance,slot,identity_digest,limit_bytes) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
            &[&tenant,&node,&volume.slot.storage,&volume.slot.slot,&volume.digest.as_slice(),&limit]).await?;
        if tx.query_opt("SELECT 1 FROM th_node_volumes WHERE tenant=$1 AND node=$2 AND storage_instance=$3 AND slot=$4 AND identity_digest=$5 AND limit_bytes=$6",
            &[&tenant,&node,&volume.slot.storage,&volume.slot.slot,&volume.digest.as_slice(),&limit]).await?.is_none() { return Err(Error::Fenced); }
        if changed == 1 {
            tx.execute("INSERT INTO th_security_audit(tenant,operator,action,principal) VALUES($1,$2,'runtime.volume.register',$3)",&[&tenant,&operator,&node]).await?;
        }
        tx.commit().await?;
        Ok(())
    }
    pub async fn request_run(&mut self, ctx: &CommandContext, input: &RunRequest) -> Result<Value> {
        identifier(&input.agent, 128)?;
        identifier(&input.task, 128)?;
        if input.run.is_nil() || input.workspace.is_nil() || input.generation <= 0 {
            return Err(Error::Invalid("run identity"));
        }
        let request = ctx.request(
            "run.request.v1",
            json!([
                1,
                input.run.to_string(),
                input.workspace.to_string(),
                input.generation,
                input.profile,
                input.argv,
                input.agent,
                input.task
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(r) => return Ok(r),
            Begin::Fresh(tx) => tx,
        };
        let result = tx.request_run_effect(input).await?;
        tx.complete(
            result,
            vec![Event {
                kind: "run.execute".into(),
                payload: json!({"run":input.run.to_string(),"epoch":ctx.epoch}),
            }],
        )
        .await
    }
    pub async fn run_status(&mut self, auth: &Authorization, run: Uuid) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let r=tx.query_opt(r#"SELECT r.workspace,r.workspace_generation,r.epoch,r.state,m.stop_requested,m.exit_code,m.oom_killed,m.removed
            FROM th_agent_runs r JOIN th_managed_runs m USING(tenant,repository,run)
            WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 AND r.principal=$4"#,&[&s.tenant(),&s.repository(),&run,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        let result = json!({"run":run.to_string(),"workspace":r.get::<_,Uuid>(0).to_string(),"generation":r.get::<_,i64>(1),"epoch":r.get::<_,i64>(2),"state":r.get::<_,String>(3),"stop_requested":r.get::<_,bool>(4),"exit_code":r.get::<_,Option<i64>>(5),"oom_killed":r.get::<_,Option<bool>>(6),"runtime_removed":r.get::<_,bool>(7)});
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn stop_run(&mut self, ctx: &CommandContext, run: Uuid) -> Result<Value> {
        let request = ctx.request("run.stop.v1", json!([1, run.to_string()]))?;
        let tx = match self.begin(request).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let row=tx.tx.query_opt("SELECT state FROM th_agent_runs WHERE tenant=$1 AND repository=$2 AND run=$3 AND principal=$4 AND epoch=$5",&[&s.tenant(),&s.repository(),&run,&ctx.principal,&ctx.epoch]).await?.ok_or(Error::NotFound)?;
        let state: String = row.get(0);
        let active = matches!(
            state.as_str(),
            "requested" | "launching" | "running" | "stopping"
        );
        if active {
            tx.tx.execute("UPDATE th_managed_runs SET stop_requested=true WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&run]).await?;
            tx.tx.execute("UPDATE th_agent_runs SET state='stopping' WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&run]).await?;
        }
        tx.complete(
            json!({"run":run.to_string(),"state":if active {"stopping"} else {&state}}),
            vec![],
        )
        .await
    }
}

async fn live_job(tx: &Transaction<'_>, auth: &Authorization, job: &Job) -> Result<()> {
    if auth.scope().tenant() != job.scope.tenant()
        || auth.scope().repository() != job.scope.repository()
        || job.kind != "run.execute"
    {
        return Err(Error::StaleLease);
    }
    let row=tx.query_opt("SELECT kind,payload FROM th_outbox WHERE id=$1 AND tenant=$2 AND repository=$3 AND state='running' AND generation=$4 AND worker=$5 AND lease_until>clock_timestamp() FOR UPDATE",
        &[&job.id,&job.scope.tenant(),&job.scope.repository(),&job.generation,&job.worker]).await?.ok_or(Error::StaleLease)?;
    if row.get::<_, String>(0) != job.kind || row.get::<_, Value>(1) != job.payload {
        return Err(Error::StaleLease);
    }
    Ok(())
}
async fn effective_stop(tx: &Transaction<'_>, scope: &Scope, run: Uuid) -> Result<bool> {
    // Hold the accepted request owner's current policy through commit. A global
    // principal disable does not take a repository lock; an MVCC read alone
    // would allow it to return before an already-authorized start commits.
    tx.query_opt("SELECT a.principal FROM th_agent_runs r JOIN th_principals a USING(tenant,principal) WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 FOR SHARE OF a",
        &[&scope.tenant(),&scope.repository(),&run]).await?;
    tx.query_opt("SELECT g.principal FROM th_agent_runs r JOIN th_repository_access g USING(tenant,repository,principal) WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 FOR SHARE OF g",
        &[&scope.tenant(),&scope.repository(),&run]).await?;
    let native = crate::integration::execution_fence(tx, scope, run).await?;
    let connected = crate::github::execution_fence(tx, scope, run).await?;
    if native.is_some() && connected.is_some() {
        return Err(Error::SchemaMismatch);
    }
    let check = native.or(connected);
    let permission = if check.is_some() {
        "validation.report"
    } else {
        "workspace.write"
    };
    let row=tx.query_one(r#"SELECT m.stop_requested OR w.generation<>r.workspace_generation OR w.epoch<>r.epoch OR w.state IN ('draining','deleted')
        OR p.state='draining' OR repo.epoch<>r.epoch OR repo.authority<>m.authority OR repo.state<>'active'
        OR EXISTS(SELECT 1 FROM th_runtime_profile_revocations x WHERE x.tenant=m.tenant AND x.repository=m.repository AND x.digest=m.profile)
        OR NOT EXISTS(SELECT 1 FROM th_principals a JOIN th_repository_access g USING(tenant,principal)
            WHERE a.tenant=r.tenant AND a.principal=r.principal AND a.enabled AND g.repository=r.repository AND $4=ANY(g.permissions))
        FROM th_managed_runs m JOIN th_agent_runs r USING(tenant,repository,run)
        JOIN th_workspaces w USING(tenant,repository,workspace)
        JOIN th_workspace_preparations p ON p.attempt=m.preparation
        JOIN th_repositories repo ON repo.tenant=m.tenant AND repo.repository=m.repository
        WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3"#,&[&scope.tenant(),&scope.repository(),&run,&permission]).await?;
    let stop: bool = row.get::<_, bool>(0) || check == Some(true);
    if stop {
        tx.execute("UPDATE th_managed_runs SET stop_requested=true WHERE tenant=$1 AND repository=$2 AND run=$3",&[&scope.tenant(),&scope.repository(),&run]).await?;
        tx.execute("UPDATE th_agent_runs SET state='stopping' WHERE tenant=$1 AND repository=$2 AND run=$3 AND state IN ('requested','launching','running')",&[&scope.tenant(),&scope.repository(),&run]).await?;
    }
    Ok(stop)
}
async fn identity(tx: &Transaction<'_>, auth: &Authorization, plan: &RunPlan) -> Result<Row> {
    let s = auth.scope();
    if s.tenant() != plan.context.prepared.scope.tenant()
        || s.repository() != plan.context.prepared.scope.repository()
        || auth.principal() != plan.context.prepared.node
    {
        return Err(Error::Forbidden);
    }
    node_auth(tx, auth, plan.context.prepared.slot.storage).await?;
    tx.query_opt(r#"SELECT m.engine,m.container,m.exit_code,m.oom_killed,m.runtime_started_at,m.runtime_finished_at,m.removed,r.state,m.finished_generation,m.finished_worker
        FROM th_managed_runs m JOIN th_agent_runs r USING(tenant,repository,run)
        WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3 AND m.attempt=$4 AND m.binding=$5 AND m.node=$6
            AND m.storage_instance=$7 AND m.slot=$8 AND m.preparation=$9 AND m.volume_digest=$10"#,
        &[&s.tenant(),&s.repository(),&plan.context.run,&plan.context.attempt,&plan.context.binding.as_slice(),&auth.principal(),&plan.context.prepared.slot.storage,&plan.context.prepared.slot.slot,&plan.context.prepared.attempt,&plan.context.volume_digest.as_slice()]).await?.ok_or(Error::ExecutionState)
}
async fn checked(tx: &Transaction<'_>, auth: &Authorization, plan: &RunPlan) -> Result<Row> {
    let row = identity(tx, auth, plan).await?;
    live_job(tx, auth, &plan.job).await?;
    Ok(row)
}
fn finished_response(plan: &RunPlan, state: &str) -> Value {
    json!({"run":plan.context.run.to_string(),"state":state,"runtime_removed":true})
}
async fn completed(
    tx: &Transaction<'_>,
    auth: &Authorization,
    plan: &RunPlan,
) -> Result<Option<Value>> {
    let row = identity(tx, auth, plan).await?;
    if !row.get::<_, bool>(6) {
        return Ok(None);
    }
    if row.get::<_, Option<i64>>(8) != Some(plan.job.generation)
        || row.get::<_, Option<Uuid>>(9) != Some(plan.job.worker)
        || tx.query_opt("SELECT 1 FROM th_outbox WHERE id=$1 AND tenant=$2 AND repository=$3 AND generation=$4 AND kind='run.execute' AND payload=$5 AND state='done' AND worker IS NULL AND lease_until IS NULL AND completed_at IS NOT NULL",
            &[&plan.job.id,&auth.scope().tenant(),&auth.scope().repository(),&plan.job.generation,&plan.job.payload]).await?.is_none()
    {
        return Err(Error::StaleLease);
    }
    let state: String = row.get(7);
    if !matches!(
        state.as_str(),
        "succeeded" | "failed" | "cancelled" | "lost"
    ) {
        return Err(Error::SchemaMismatch);
    }
    Ok(Some(finished_response(plan, &state)))
}
async fn load_plan(
    tx: &Transaction<'_>,
    auth: &Authorization,
    job: Job,
    storage: Uuid,
) -> Result<RunPlan> {
    let run = job.payload["run"]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
        .ok_or(Error::StaleLease)?;
    let context = load_context(tx, auth, run, storage).await?;
    let stop = effective_stop(tx, auth.scope(), run).await?;
    Ok(RunPlan { job, context, stop })
}
/// Shared identity decoding for execution and stopped-file capture. The caller
/// must separately authorize a live operation; a terminal run has no fake job.
pub(crate) async fn load_context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    run: Uuid,
    storage: Uuid,
) -> Result<RunContext> {
    let s = auth.scope();
    let row=tx.query_opt(r#"SELECT m.attempt,m.preparation,m.binding,m.volume_digest,m.authority,m.profile,m.argv,f.definition AS runtime_profile,
        p.workspace,p.generation,p.epoch,p.slot,p.base_revision,p.snapshot,p.profile AS source_digest,q.definition AS source_profile,p.manifest_digest,r.principal,v.limit_bytes,m.engine,m.container
        FROM th_managed_runs m JOIN th_agent_runs r USING(tenant,repository,run)
        JOIN th_workspace_preparations p ON p.attempt=m.preparation
        JOIN th_runtime_profiles f ON f.tenant=m.tenant AND f.repository=m.repository AND f.digest=m.profile
        JOIN th_source_profiles q ON q.tenant=p.tenant AND q.repository=p.repository AND q.digest=p.profile
        JOIN th_node_volumes v ON v.storage_instance=m.storage_instance AND v.slot=m.slot AND v.identity_digest=m.volume_digest
        JOIN th_bases b ON b.tenant=p.tenant AND b.repository=p.repository AND b.revision=p.base_revision AND b.snapshot=p.snapshot
        WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3 AND m.node=$4 AND m.storage_instance=$5
          AND p.tenant=m.tenant AND p.repository=m.repository AND p.node=m.node AND p.storage_instance=m.storage_instance AND p.slot=m.slot
          AND r.workspace=p.workspace AND r.workspace_generation=p.generation AND r.epoch=p.epoch
          AND p.manifest_digest IS NOT NULL"#,&[&s.tenant(),&s.repository(),&run,&auth.principal(),&storage]).await?.ok_or(Error::ExecutionState)?;
    let runtime = profile(row.get("runtime_profile"))?;
    let command = task(row.get("argv"))?;
    let source = SourceProfile::decode(&row.get::<_, Vec<u8>>("source_profile"))?;
    let runtime_id = digest(row.get("profile"))?; // m.profile is the only column with this label.
    if runtime.digest() != runtime_id
        || source.id().as_bytes() != &digest(row.get("source_digest"))?
        || runtime.definition().workspace_bytes != row.get::<_, i64>("limit_bytes") as u64
    {
        return Err(Error::SchemaMismatch);
    }
    let mode = authority(&row.get::<_, String>("authority"))?;
    let attempt = row.get("attempt");
    let preparation = row.get("preparation");
    let volume_digest = digest(row.get("volume_digest"))?;
    let epoch = row.get("epoch");
    let generation = row.get("generation");
    let bound = digest(row.get("binding"))?;
    if bound
        != binding(RunBinding {
            scope: s,
            run,
            attempt,
            preparation,
            owner: &row.get::<_, String>("principal"),
            authority: mode,
            epoch,
            generation,
            profile: runtime_id,
            argv: command.argv(),
            volume: volume_digest,
        })
    {
        return Err(Error::SchemaMismatch);
    }
    let prepared = PreparedWorkspace {
        volume_digest: Some(volume_digest),
        scope: s.clone(),
        attempt: preparation,
        workspace: row.get("workspace"),
        generation,
        epoch,
        authority: mode,
        node: auth.principal().into(),
        slot: StorageSlot {
            storage,
            slot: row.get("slot"),
        },
        revision: row.get::<_, String>("base_revision").parse()?,
        snapshot: ObjectId::from_bytes(digest(row.get("snapshot"))?),
        profile: source,
        manifest_digest: digest(row.get("manifest_digest"))?,
    };
    Ok(RunContext {
        run,
        attempt,
        binding: bound,
        volume_digest,
        prepared,
        profile: runtime,
        task: command,
        engine: row.get("engine"),
        container: row.get("container"),
        check: tx.query_opt("SELECT 1 WHERE EXISTS(SELECT 1 FROM th_check_executions WHERE tenant=$1 AND repository=$2 AND execution_run=$3) OR EXISTS(SELECT 1 FROM th_github_managed_executions WHERE tenant=$1 AND repository=$2 AND execution_run=$3)", &[&s.tenant(),&s.repository(),&run]).await?.is_some(),
    })
}
impl Metadata {
    /// Claim execution or cleanup on the existing node only. Epoch changes stop
    /// new execution but do not make old physical writers disappear.
    pub async fn claim_run(
        &mut self,
        auth: &Authorization,
        storage: Uuid,
        worker: Uuid,
        ttl: i32,
    ) -> Result<Option<RunPlan>> {
        ttl_valid(ttl)?;
        if worker.is_nil() {
            return Err(Error::Invalid("worker"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        node_auth(&tx, auth, storage).await?;
        let row=tx.query_opt(r#"WITH selected AS(SELECT o.id FROM th_outbox o JOIN th_managed_runs m
            ON m.tenant=o.tenant AND m.repository=o.repository AND m.run::text=o.payload->>'run'
            WHERE o.tenant=$1 AND o.repository=$2 AND o.kind='run.execute' AND m.node=$3 AND m.storage_instance=$4
            AND NOT m.removed AND ((o.state='pending' AND o.available_at<=clock_timestamp()) OR (o.state='running' AND o.lease_until<=clock_timestamp()))
            ORDER BY o.id LIMIT 1 FOR UPDATE OF o SKIP LOCKED)
            UPDATE th_outbox o SET state='running',worker=$5,generation=generation+1,lease_until=clock_timestamp()+($6::integer*interval '1 second')
            FROM selected WHERE o.id=selected.id RETURNING o.id,o.generation,o.kind,o.payload"#,
            &[&s.tenant(),&s.repository(),&auth.principal(),&storage,&worker,&ttl]).await?;
        let result = if let Some(row) = row {
            let job = Job {
                id: row.get(0),
                scope: s.clone(),
                generation: row.get(1),
                worker,
                kind: row.get(2),
                payload: row.get(3),
            };
            Some(load_plan(&tx, auth, job, storage).await?)
        } else {
            None
        };
        node_auth(&tx, auth, storage).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn renew_run(
        &mut self,
        auth: &Authorization,
        plan: &RunPlan,
        ttl: i32,
    ) -> Result<bool> {
        ttl_valid(ttl)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let observed = checked(&tx, auth, plan).await?;
        if observed.get::<_, bool>(6) {
            return Err(Error::ExecutionState);
        }
        let stop = effective_stop(&tx, s, plan.context.run).await?;
        let changed=tx.execute("UPDATE th_outbox SET lease_until=clock_timestamp()+($4::integer*interval '1 second') WHERE id=$1 AND generation=$2 AND worker=$3 AND state='running' AND lease_until>clock_timestamp()",
            &[&plan.job.id,&plan.job.generation,&plan.job.worker,&ttl]).await?;
        if changed != 1 {
            return Err(Error::StaleLease);
        }
        node_auth(&tx, auth, plan.context.prepared.slot.storage).await?;
        tx.commit().await?;
        Ok(stop)
    }
    /// Linearization of launch permission. A previously admitted Engine RPC may
    /// still finish after a concurrent drain; cleanup must fence its exact ID.
    pub async fn admit_run_start(&mut self, auth: &Authorization, plan: &RunPlan) -> Result<()> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let row = checked(&tx, auth, plan).await?;
        if effective_stop(&tx, s, plan.context.run).await?
            || row.get::<_, bool>(6)
            || !matches!(row.get::<_, String>(7).as_str(), "requested" | "launching")
        {
            return Err(Error::ExecutionState);
        }
        tx.execute("UPDATE th_agent_runs SET state='launching' WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&plan.context.run]).await?;
        live_job(&tx, auth, &plan.job).await?;
        node_auth(&tx, auth, plan.context.prepared.slot.storage).await?;
        tx.commit().await?;
        Ok(())
    }
    /// Called only after exact Engine inspection by the trusted node adapter.
    pub async fn observe_run(
        &mut self,
        auth: &Authorization,
        plan: &RunPlan,
        engine: &str,
        actual: &Inspection,
    ) -> Result<bool> {
        identifier(engine, 128)?;
        if actual.container.binding() != plan.context.binding {
            return Err(Error::ReceiptMismatch);
        }
        let id = actual.container.id();
        let runtime = format!("docker:{id}");
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        let row = checked(&tx, auth, plan).await?;
        if row.get::<_, bool>(6) {
            return Err(Error::ExecutionState);
        }
        if let Some(previous) = row.get::<_, Option<String>>(1)
            && (previous != id || row.get::<_, Option<String>>(0).as_deref() != Some(engine))
        {
            return Err(Error::ReceiptMismatch);
        }
        let stop = effective_stop(&tx, s, plan.context.run).await?;
        tx.execute("UPDATE th_managed_runs SET engine=$4,container=$5 WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&plan.context.run,&engine,&id]).await?;
        tx.execute(
            "UPDATE th_agent_runs SET runtime=$4 WHERE tenant=$1 AND repository=$2 AND run=$3",
            &[&s.tenant(), &s.repository(), &plan.context.run, &runtime],
        )
        .await?;
        if tx.execute("UPDATE th_workspaces SET runtime=$4 WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND generation=$5 AND epoch=$6 AND (runtime IS NULL OR runtime=$4)",&[&s.tenant(),&s.repository(),&plan.context.prepared.workspace,&runtime,&plan.context.prepared.generation,&plan.context.prepared.epoch]).await? != 1 {
            return Err(Error::ExecutionState);
        }
        match actual.state {
            RuntimeState::Running => {
                if row.get::<_, Option<i64>>(2).is_some() {
                    return Err(Error::ReceiptMismatch);
                }
                let state = if stop { "stopping" } else { "running" };
                tx.execute("UPDATE th_agent_runs SET state=$4,started_at=COALESCE(started_at,clock_timestamp()) WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&plan.context.run,&state]).await?;
            }
            RuntimeState::Exited => {
                if let Some(code) = row.get::<_, Option<i64>>(2)
                    && (code != actual.exit_code
                        || row.get::<_, Option<bool>>(3) != Some(actual.oom_killed)
                        || row.get::<_, Option<String>>(4).as_deref() != Some(&actual.started_at)
                        || row.get::<_, Option<String>>(5).as_deref() != Some(&actual.finished_at))
                {
                    return Err(Error::ReceiptMismatch);
                }
                tx.execute("UPDATE th_managed_runs SET exit_code=$4,oom_killed=$5,runtime_started_at=$6,runtime_finished_at=$7 WHERE tenant=$1 AND repository=$2 AND run=$3",
                    &[&s.tenant(),&s.repository(),&plan.context.run,&actual.exit_code,&actual.oom_killed,&actual.started_at,&actual.finished_at]).await?;
                tx.execute("UPDATE th_agent_runs SET state='stopping',started_at=COALESCE(started_at,clock_timestamp()) WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&plan.context.run]).await?;
            }
            RuntimeState::Created => {
                if row.get::<_, Option<i64>>(2).is_some() {
                    return Err(Error::ReceiptMismatch);
                }
            }
            RuntimeState::Dead | RuntimeState::Removing => {
                tx.execute("UPDATE th_agent_runs SET state='stopping' WHERE tenant=$1 AND repository=$2 AND run=$3",&[&s.tenant(),&s.repository(),&plan.context.run]).await?;
            }
        }
        live_job(&tx, auth, &plan.job).await?;
        node_auth(&tx, auth, plan.context.prepared.slot.storage).await?;
        tx.commit().await?;
        Ok(stop)
    }
    /// Trusted caller has confirmed exact runtime removal. No files/pins are
    /// released here, and no task result is upgraded into source Validation.
    pub async fn finish_run(&mut self, auth: &Authorization, plan: &RunPlan) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, true).await?;
        if let Some(response) = completed(&tx, auth, plan).await? {
            node_auth(&tx, auth, plan.context.prepared.slot.storage).await?;
            tx.commit().await?;
            return Ok(response);
        }
        let row = checked(&tx, auth, plan).await?;
        let stop = effective_stop(&tx, s, plan.context.run).await?;
        let state = if stop {
            "cancelled"
        } else if row.get::<_, Option<bool>>(3) == Some(true) {
            "failed"
        } else {
            match row.get::<_, Option<i64>>(2) {
                Some(0) => "succeeded",
                Some(_) => "failed",
                None => "lost",
            }
        };
        tx.execute(
            "UPDATE th_managed_runs SET removed=true,finished_generation=$4,finished_worker=$5 WHERE tenant=$1 AND repository=$2 AND run=$3",
            &[&s.tenant(), &s.repository(), &plan.context.run, &plan.job.generation, &plan.job.worker],
        )
        .await?;
        tx.execute(
            "UPDATE th_agent_runs SET state=$4 WHERE tenant=$1 AND repository=$2 AND run=$3",
            &[&s.tenant(), &s.repository(), &plan.context.run, &state],
        )
        .await?;
        let changed=tx.execute("UPDATE th_outbox SET state='done',worker=NULL,lease_until=NULL,completed_at=clock_timestamp() WHERE id=$1 AND generation=$2 AND worker=$3 AND state='running' AND lease_until>clock_timestamp()",
            &[&plan.job.id,&plan.job.generation,&plan.job.worker]).await?;
        if changed != 1 {
            return Err(Error::StaleLease);
        }
        node_auth(&tx, auth, plan.context.prepared.slot.storage).await?;
        tx.commit().await?;
        Ok(finished_response(plan, state))
    }
    /// Resolve a lost finish response for the exact completing lease. A stale
    /// worker cannot claim a successor's completion, nor authorize a new start.
    pub async fn completed_run(
        &mut self,
        auth: &Authorization,
        plan: &RunPlan,
    ) -> Result<Option<Value>> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let result = completed(&tx, auth, plan).await?;
        node_auth(&tx, auth, plan.context.prepared.slot.storage).await?;
        tx.commit().await?;
        Ok(result)
    }
}

impl crate::Mutation<'_> {
    pub(crate) async fn request_run_effect(&self, input: &RunRequest) -> Result<Value> {
        let command = Task::new(input.argv.clone()).map_err(|_| Error::Invalid("run argv"))?;
        let argv = serde_json::to_vec(command.argv()).map_err(|_| Error::Invalid("run argv"))?;
        let ctx = &self.request;
        let s = &ctx.scope;
        let row=self.tx.query_opt(r#"SELECT p.attempt,p.node,p.storage_instance,p.slot,v.identity_digest,v.limit_bytes,f.definition
            FROM th_workspaces w JOIN th_workspace_preparations p ON p.tenant=w.tenant AND p.repository=w.repository AND p.workspace=w.workspace AND p.generation=w.generation
            JOIN th_node_volumes v ON v.storage_instance=p.storage_instance AND v.slot=p.slot AND v.tenant=p.tenant AND v.node=p.node
            JOIN th_runtime_profiles f ON f.tenant=w.tenant AND f.repository=w.repository AND f.digest=$7
            WHERE w.tenant=$1 AND w.repository=$2 AND w.workspace=$3 AND w.generation=$4 AND w.epoch=$5 AND w.principal=$6
            AND w.state='ready' AND w.runtime IS NULL AND w.format='native'
            AND p.state='prepared' AND p.epoch=w.epoch AND p.node=w.node
            AND p.base_revision=w.base_revision AND p.profile=w.resolved_profile
            AND NOT EXISTS(SELECT 1 FROM th_runtime_profile_revocations x WHERE x.tenant=f.tenant AND x.repository=f.repository AND x.digest=f.digest)"#,
            &[&s.tenant(),&s.repository(),&input.workspace,&input.generation,&ctx.epoch,&ctx.principal,&input.profile.as_slice()]).await?.ok_or(Error::ExecutionState)?;
        let p = profile(row.get(6))?;
        if p.digest() != input.profile
            || p.definition().workspace_bytes != row.get::<_, i64>(5) as u64
        {
            return Err(Error::Invalid("runtime profile or volume"));
        }
        let preparation: Uuid = row.get(0);
        let node: String = row.get(1);
        let storage: Uuid = row.get(2);
        let slot: Uuid = row.get(3);
        let volume = digest(row.get(4))?;
        let attempt = Uuid::new_v4();
        let bound = binding(RunBinding {
            scope: s,
            run: input.run,
            attempt,
            preparation,
            owner: &ctx.principal,
            authority: ctx.authority,
            epoch: ctx.epoch,
            generation: input.generation,
            profile: input.profile,
            argv: command.argv(),
            volume,
        });
        self.tx.execute(r#"INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state,started_at)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,NULL,'requested',NULL)"#,
            &[&s.tenant(),&s.repository(),&input.run,&input.workspace,&input.generation,&ctx.epoch,&ctx.principal,&input.agent,&input.task]).await?;
        self.tx.execute(r#"INSERT INTO th_managed_runs(tenant,repository,run,attempt,preparation,node,storage_instance,slot,volume_digest,profile,argv,binding,operation_key,authority)
            VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)"#,
            &[&s.tenant(),&s.repository(),&input.run,&attempt,&preparation,&node,&storage,&slot,&volume.as_slice(),&input.profile.as_slice(),&argv,&bound.as_slice(),&ctx.key,&ctx.authority.as_str()]).await?;
        self.tx.execute("UPDATE th_workspaces SET state='in_use' WHERE tenant=$1 AND repository=$2 AND workspace=$3",&[&s.tenant(),&s.repository(),&input.workspace]).await?;
        Ok(
            json!({"run":input.run.to_string(),"workspace":input.workspace.to_string(),"state":"requested"}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn seal_binding_matches_independent_vector() {
        let scope = Scope::new("tenant", Uuid::from_u128(1)).unwrap();
        let context = RunContext {
            run: Uuid::from_u128(2),
            attempt: Uuid::from_u128(3),
            binding: [3; 32],
            volume_digest: [4; 32],
            prepared: PreparedWorkspace {
                volume_digest: Some([4; 32]),
                scope: scope.clone(),
                attempt: Uuid::from_u128(4),
                workspace: Uuid::from_u128(5),
                generation: 2,
                epoch: 3,
                authority: Authority::Standalone,
                node: "node-1".into(),
                slot: StorageSlot {
                    storage: Uuid::from_u128(6),
                    slot: Uuid::from_u128(7),
                },
                revision: treeharbor_core::RevisionRef::Native(ObjectId::from_bytes([1; 32])),
                snapshot: ObjectId::from_bytes([2; 32]),
                profile: SourceProfile::full(),
                manifest_digest: [5; 32],
            },
            profile: Profile::decode(include_bytes!(
                "../../runtime/tests/fixtures/profile-v1.json"
            ))
            .unwrap(),
            task: Task::new(vec!["/tool".into()]).unwrap(),
            check: false,
            engine: None,
            container: None,
        };
        let path = |p: &[u8]| treeharbor_core::RepoPath::new(p.to_vec()).unwrap();
        let selection = treeharbor_core::CaptureSelection::new(
            vec![path(b"src")],
            vec![path(b"old")],
            vec![path(b"src/cache")],
        )
        .unwrap();
        let bound = crate::seals::binding(
            &scope,
            Uuid::from_u128(8),
            &context,
            &selection,
            100,
            1048576,
        );
        assert_eq!(
            ObjectId::from_bytes(bound).to_string(),
            include_str!("../tests/fixtures/seal-binding-v1.sha256").trim()
        );
    }
    #[test]
    fn run_binding_matches_independent_utf8_vector() {
        let scope = Scope::new("tenant", Uuid::from_u128(1)).unwrap();
        let bound = binding(RunBinding {
            scope: &scope,
            run: Uuid::from_u128(7),
            attempt: Uuid::from_u128(8),
            preparation: Uuid::from_u128(4),
            owner: "owner-é",
            authority: Authority::Standalone,
            epoch: 3,
            generation: 2,
            profile: [17; 32],
            argv: &["/bin/tool".into(), "literal \"quoted\" é".into()],
            volume: [71; 32],
        });
        assert_eq!(
            ObjectId::from_bytes(bound).to_string(),
            format!(
                "sha256:{}",
                include_str!("../tests/fixtures/run-binding-v1.sha256").trim()
            )
        );
    }
}
