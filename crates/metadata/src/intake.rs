//! Trusted service commands. Authentication and runtime observation are supplied
//! by the coordinator, not by agent JSON. These records do not start a sandbox.
use crate::catalog::ObjectLease;
use crate::{
    Authority, Begin, Error, Event, Job, Metadata, Mutation, Request, Result, Scope, identifier,
};
use serde_json::{Value, json};
use treeharbor_core::{AppliedChange, Commit, Object, RevisionRef};
use uuid::Uuid;

pub struct CommandContext {
    pub scope: Scope,
    pub key: String,
    pub principal: String,
    pub authority: Authority,
    pub epoch: i64,
    authorization: Option<std::sync::Arc<crate::access::Authorization>>,
}
impl CommandContext {
    /// Administrative/internal code only. External requests use Authorization::context.
    pub fn trusted(
        scope: Scope,
        key: String,
        principal: String,
        authority: Authority,
        epoch: i64,
    ) -> Self {
        Self {
            scope,
            key,
            principal,
            authority,
            epoch,
            authorization: None,
        }
    }
    pub(crate) fn authenticated(
        auth: crate::access::Authorization,
        key: String,
        authority: Authority,
        epoch: i64,
    ) -> Self {
        Self {
            scope: auth.scope().clone(),
            principal: auth.principal().into(),
            key,
            authority,
            epoch,
            authorization: Some(std::sync::Arc::new(auth)),
        }
    }
    pub(crate) fn request(&self, kind: &str, command: Value) -> Result<Request> {
        // Only service-built, versioned arrays are hashed; no arbitrary result or
        // command digest from the client can bypass an idempotency comparison.
        let mut request = Request::new(
            self.scope.clone(),
            &self.key,
            &self.principal,
            kind,
            self.authority,
            self.epoch,
            command.to_string().as_bytes(),
        )?;
        request.authorization = self.authorization.clone();
        Ok(request)
    }
}

#[derive(Clone, Copy)]
pub enum WorkspaceFormat {
    Native,
    GitCompatible,
}
impl WorkspaceFormat {
    fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::GitCompatible => "git-compatible",
        }
    }
}
pub struct WorkspaceSpec {
    pub id: Uuid,
    pub base: RevisionRef,
    pub profile: [u8; 32],
    pub format: WorkspaceFormat,
}
pub struct WorkspaceReady {
    pub id: Uuid,
    pub generation: i64,
    pub node: String,
    pub runtime: String,
}
pub struct RunningAgent {
    pub id: Uuid,
    pub workspace: Uuid,
    pub workspace_generation: i64,
    pub agent: String,
    pub task: String,
    pub runtime: String,
}
pub struct ChangeSubmission<'a> {
    pub id: Uuid,
    pub previous_version: i64,
    pub run: Uuid,
    pub workspace_generation: i64,
    pub applied: &'a AppliedChange,
    pub operations: &'a ObjectLease,
    pub result: &'a ObjectLease,
}
fn workspace_pin(id: Uuid) -> String {
    format!("workspace:{id}")
}
fn event(kind: &str, payload: Value) -> Event {
    Event {
        kind: kind.into(),
        payload,
    }
}

impl Metadata {
    /// Register a native commit whose complete graph has already been published.
    /// This creates an immutable base binding, NOT a branch or admission decision.
    pub async fn register_native_base(
        &mut self,
        ctx: &CommandContext,
        commit: &Commit,
        lease: &ObjectLease,
    ) -> Result<Value> {
        let id = Object::NativeCommit(commit.clone()).id()?;
        if lease.key().id() != id {
            return Err(Error::ReceiptMismatch);
        }
        let revision = RevisionRef::Native(id).to_string();
        let request = ctx.request(
            "base.register.v1",
            json!([1, revision, commit.snapshot.to_string()]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        tx.register_base(id, commit.snapshot, lease).await?;
        tx.complete(
            json!({"revision":revision,"snapshot":commit.snapshot.to_string()}),
            vec![],
        )
        .await
    }

    /// Reserve logical source context. Files are not ready until the preparation
    /// worker completes its current outbox lease and reports its runtime binding.
    pub async fn create_workspace(
        &mut self,
        ctx: &CommandContext,
        spec: &WorkspaceSpec,
    ) -> Result<Value> {
        let revision = spec.base.to_string();
        let request = ctx.request(
            "workspace.create.v1",
            json!([
                1,
                spec.id.to_string(),
                revision,
                spec.profile,
                spec.format.name()
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let scope = &ctx.scope;
        let row = tx
            .tx
            .query_opt(
                "SELECT pin_holder FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",
                &[&scope.tenant, &scope.repository, &revision],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let profile_row=tx.tx.query_opt("SELECT definition FROM th_source_profiles WHERE tenant=$1 AND repository=$2 AND digest=$3", &[&scope.tenant,&scope.repository,&spec.profile.as_slice()]).await?.ok_or(Error::NotFound)?;
        let profile = treeharbor_core::SourceProfile::decode(&profile_row.get::<_, Vec<u8>>(0))?;
        if profile.id().as_bytes() != &spec.profile {
            return Err(Error::SchemaMismatch);
        }
        let holder = workspace_pin(spec.id);
        tx.copy_root_pin(&row.get::<_, String>(0), &holder).await?;
        tx.tx.execute(r#"INSERT INTO th_workspaces (tenant,repository,workspace,base_revision,principal,profile,format,state,epoch,pin_holder,resolved_profile)
            VALUES ($1,$2,$3,$4,$5,$6,$7,'requested',$8,$9,$6)"#,
            &[&scope.tenant,&scope.repository,&spec.id,&revision,&ctx.principal,&spec.profile.as_slice(),&spec.format.name(),&ctx.epoch,&holder]).await?;
        let result = json!({"workspace":spec.id.to_string(),"generation":1,"state":"requested","base":revision});
        tx.complete(
            result,
            vec![event(
                "workspace.prepare",
                json!({"workspace":spec.id.to_string(),"generation":1,"epoch":ctx.epoch}),
            )],
        )
        .await
    }

    /// The trusted coordinator calls this only after materialization/runtime
    /// observation. A stale worker or a concurrent drain cannot report READY.
    pub async fn record_workspace_ready(
        &mut self,
        ctx: &CommandContext,
        ready: &WorkspaceReady,
        job: &Job,
    ) -> Result<Value> {
        identifier(&ready.node, 128)?;
        identifier(&ready.runtime, 128)?;
        if ctx.authorization.is_some() && ready.node != ctx.principal {
            return Err(Error::Forbidden);
        }
        let request = ctx.request(
            "workspace.ready.v1",
            json!([
                1,
                ready.id.to_string(),
                ready.generation,
                ready.node,
                ready.runtime,
                job.id,
                job.generation
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        tx.check_preparation_job(job, ready).await?;
        let scope = &ctx.scope;
        // Managed preparation has a separate file-ready protocol. Legacy
        // observations cannot replace its durable node/slot/source binding.
        if tx.tx.query_opt("SELECT 1 FROM th_workspace_preparations WHERE tenant=$1 AND repository=$2 AND workspace=$3",
            &[&scope.tenant,&scope.repository,&ready.id]).await?.is_some() {
            return Err(Error::ExecutionState);
        }
        let changed=tx.tx.execute(r#"UPDATE th_workspaces SET state='ready',node=$4,runtime=$5
            WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND generation=$6 AND epoch=$7 AND state='requested' AND resolved_profile IS NOT NULL"#,
            &[&scope.tenant,&scope.repository,&ready.id,&ready.node,&ready.runtime,&ready.generation,&ctx.epoch]).await?;
        if changed != 1 {
            return Err(Error::ExecutionState);
        }
        tx.tx.execute("UPDATE th_outbox SET state='done',worker=NULL,lease_until=NULL,completed_at=clock_timestamp() WHERE id=$1",&[&job.id]).await?;
        tx.complete(
            json!({"workspace":ready.id.to_string(),"generation":ready.generation,"state":"ready"}),
            vec![],
        )
        .await
    }

    /// Record an execution observed by the node bound to this workspace. The
    /// run owner comes from the workspace; the node is only the observing actor.
    /// This method neither launches a process nor proves sandbox isolation.
    pub async fn record_running_agent(
        &mut self,
        ctx: &CommandContext,
        run: &RunningAgent,
    ) -> Result<Value> {
        identifier(&run.agent, 128)?;
        identifier(&run.task, 128)?;
        identifier(&run.runtime, 128)?;
        let request = ctx.request(
            "run.running.v1",
            json!([
                1,
                run.id.to_string(),
                run.workspace.to_string(),
                run.workspace_generation,
                run.agent,
                run.task,
                run.runtime
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let scope = &ctx.scope;
        // Authenticated controllers may observe only their own node binding.
        // Legacy trusted callers must still match the workspace owner.
        let node = ctx.authorization.as_ref().map(|_| ctx.principal.as_str());
        let legacy_owner = if node.is_none() {
            Some(ctx.principal.as_str())
        } else {
            None
        };
        let row = tx
            .tx
            .query_opt(
                r#"UPDATE th_workspaces SET state='in_use'
            WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND generation=$4 AND epoch=$5
            AND state='ready' AND runtime=$6 AND resolved_profile IS NOT NULL
            AND ($7::text IS NULL OR node=$7) AND ($8::text IS NULL OR principal=$8)
            RETURNING principal"#,
                &[
                    &scope.tenant,
                    &scope.repository,
                    &run.workspace,
                    &run.workspace_generation,
                    &ctx.epoch,
                    &run.runtime,
                    &node,
                    &legacy_owner,
                ],
            )
            .await?
            .ok_or(Error::ExecutionState)?;
        let owner: String = row.get(0);
        tx.tx.execute(r#"INSERT INTO th_agent_runs (tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'running')"#,
            &[&scope.tenant,&scope.repository,&run.id,&run.workspace,&run.workspace_generation,&ctx.epoch,&owner,&run.agent,&run.task,&run.runtime]).await?;
        tx.complete(json!({"run":run.id.to_string(),"workspace":run.workspace.to_string(),"state":"running"}),vec![]).await
    }

    /// Block new changes before requesting runtime shutdown. Keep every pin until
    /// a separate trusted lifecycle path confirms stopped writers/unmounted files.
    pub async fn drain_workspace(
        &mut self,
        ctx: &CommandContext,
        workspace: Uuid,
        generation: i64,
    ) -> Result<Value> {
        let next = generation
            .checked_add(1)
            .filter(|_| generation > 0)
            .ok_or(Error::Invalid("workspace generation"))?;
        let request = ctx.request(
            "workspace.drain.v1",
            json!([1, workspace.to_string(), generation]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let scope = &ctx.scope;
        let count = tx
            .tx
            .execute(
                r#"UPDATE th_workspaces SET state='draining',generation=$4
            WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND generation=$5 AND epoch=$6
            AND principal=$7 AND state IN ('requested','ready','in_use')"#,
                &[
                    &scope.tenant,
                    &scope.repository,
                    &workspace,
                    &next,
                    &generation,
                    &ctx.epoch,
                    &ctx.principal,
                ],
            )
            .await?;
        if count != 1 {
            return Err(Error::ExecutionState);
        }
        tx.tx.execute("UPDATE th_agent_runs SET state='stopping' WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND state='running'",&[&scope.tenant,&scope.repository,&workspace]).await?;
        tx.tx.execute("UPDATE th_workspace_preparations SET state='draining' WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND state<>'draining'",&[&scope.tenant,&scope.repository,&workspace]).await?;
        let result =
            json!({"workspace":workspace.to_string(),"generation":next,"state":"draining"});
        tx.complete(result.clone(), vec![event("workspace.drain", result)])
            .await
    }

    /// Acknowledged only after version, source/run lineage, three graph pins,
    /// replay result and outbox commit atomically. Acceptance grants no merge right.
    pub async fn accept_change(
        &mut self,
        ctx: &CommandContext,
        change: &ChangeSubmission<'_>,
    ) -> Result<Value> {
        let version = change
            .previous_version
            .checked_add(1)
            .filter(|_| change.previous_version >= 0)
            .ok_or(Error::Invalid("previous version"))?;
        let applied = change.applied;
        if change.operations.key().id() != applied.operations()
            || change.result.key().id() != applied.result_snapshot()
        {
            return Err(Error::ReceiptMismatch);
        }
        let request = ctx.request(
            "change.accept.v1",
            json!([
                1,
                change.id.to_string(),
                change.previous_version,
                change.run.to_string(),
                change.workspace_generation,
                applied.base_snapshot().to_string(),
                applied.operations().to_string(),
                applied.result_snapshot().to_string()
            ]),
        )?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let scope = &ctx.scope;
        let row=tx.tx.query_opt(r#"SELECT w.base_revision,b.snapshot,w.pin_holder FROM th_agent_runs r
            JOIN th_workspaces w USING (tenant,repository,workspace)
            JOIN th_bases b ON b.tenant=w.tenant AND b.repository=w.repository AND b.revision=w.base_revision
            WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 AND r.principal=$4 AND w.principal=$4
            AND r.state='running' AND w.state='in_use' AND r.epoch=$5 AND w.epoch=$5
            AND r.workspace_generation=$6 AND w.generation=$6 AND r.runtime=w.runtime AND w.resolved_profile IS NOT NULL"#,
            &[&scope.tenant,&scope.repository,&change.run,&ctx.principal,&ctx.epoch,&change.workspace_generation]).await?.ok_or(Error::ExecutionState)?;
        if row.get::<_, Vec<u8>>(1) != applied.base_snapshot().as_bytes() {
            return Err(Error::ReceiptMismatch);
        }
        let base_revision: String = row.get(0);
        let source_pin: String = row.get(2);
        if change.previous_version == 0 {
            let inserted=tx.tx.execute("INSERT INTO th_changesets (tenant,repository,changeset,principal,latest_version) VALUES ($1,$2,$3,$4,1) ON CONFLICT DO NOTHING",
                &[&scope.tenant,&scope.repository,&change.id,&ctx.principal]).await?;
            if inserted != 1 {
                return Err(Error::VersionConflict);
            }
        } else {
            let changed=tx.tx.execute("UPDATE th_changesets SET latest_version=$4 WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND principal=$5 AND latest_version=$6",
                &[&scope.tenant,&scope.repository,&change.id,&version,&ctx.principal,&change.previous_version]).await?;
            if changed != 1 {
                return Err(Error::VersionConflict);
            }
        }
        let prefix = format!("change:{}:{version}", change.id);
        let base_pin = format!("{prefix}:base");
        let operations_pin = format!("{prefix}:operations");
        let result_pin = format!("{prefix}:result");
        tx.lock_intake_roots(&source_pin, change.operations, change.result)
            .await?;
        tx.copy_root_pin(&source_pin, &base_pin).await?;
        tx.pin_kind(&operations_pin, change.operations, 4).await?;
        tx.pin_kind(&result_pin, change.result, 3).await?;
        tx.tx.execute(r#"INSERT INTO th_change_versions
            (tenant,repository,changeset,version,run,base_revision,base_snapshot,operations,result_snapshot,base_pin,operations_pin,result_pin,operation_key)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)"#,
            &[&scope.tenant,&scope.repository,&change.id,&version,&change.run,&base_revision,
            &applied.base_snapshot().as_bytes().as_slice(),&applied.operations().as_bytes().as_slice(),&applied.result_snapshot().as_bytes().as_slice(),
            &base_pin,&operations_pin,&result_pin,&ctx.key]).await?;
        let result = json!({"changeset":change.id.to_string(),"version":version,"state":"accepted","run":change.run.to_string(),
            "base_revision":base_revision,"base_snapshot":applied.base_snapshot().to_string(),"operations":applied.operations().to_string(),"result_snapshot":applied.result_snapshot().to_string()});
        tx.complete(result.clone(), vec![event("change.accepted", result)])
            .await
    }

    /// Caller must authenticate repository read access; a UUID is not a capability.
    pub async fn change_version(&self, scope: &Scope, id: Uuid, version: i64) -> Result<Value> {
        let row=self.client.query_opt(r#"SELECT run,base_revision,base_snapshot,operations,result_snapshot
            FROM th_change_versions WHERE tenant=$1 AND repository=$2 AND changeset=$3 AND version=$4"#,
            &[&scope.tenant,&scope.repository,&id,&version]).await?.ok_or(Error::NotFound)?;
        let object = |index| -> Result<String> {
            let raw: Vec<u8> = row.get(index);
            Ok(treeharbor_core::ObjectId::from_bytes(
                raw.try_into().map_err(|_| Error::SchemaMismatch)?,
            )
            .to_string())
        };
        Ok(
            json!({"changeset":id.to_string(),"version":version,"state":"accepted","run":row.get::<_,Uuid>(0).to_string(),
            "base_revision":row.get::<_,String>(1),"base_snapshot":object(2)?,"operations":object(3)?,"result_snapshot":object(4)?}),
        )
    }
}

impl Mutation<'_> {
    async fn check_preparation_job(&self, job: &Job, ready: &WorkspaceReady) -> Result<()> {
        let scope = &self.request.scope;
        if job.scope.tenant != scope.tenant
            || job.scope.repository != scope.repository
            || job.kind != "workspace.prepare"
            || job.payload["workspace"] != ready.id.to_string()
            || job.payload["generation"] != ready.generation
            || job.payload["epoch"] != self.request.epoch
        {
            return Err(Error::StaleLease);
        }
        let live=self.tx.query_opt(r#"SELECT kind,payload FROM th_outbox WHERE id=$1 AND tenant=$2 AND repository=$3
            AND state='running' AND worker=$4 AND generation=$5 AND lease_until>clock_timestamp() FOR UPDATE"#,
            &[&job.id,&scope.tenant,&scope.repository,&job.worker,&job.generation]).await?;
        let live = live.ok_or(Error::StaleLease)?;
        if live.get::<_, String>(0) != job.kind || live.get::<_, Value>(1) != job.payload {
            return Err(Error::StaleLease);
        }
        Ok(())
    }
    async fn lock_intake_roots(
        &self,
        source: &str,
        operations: &ObjectLease,
        result: &ObjectLease,
    ) -> Result<()> {
        let scope = &self.request.scope;
        if operations.key().tenant() != scope.tenant || result.key().tenant() != scope.tenant {
            return Err(Error::NotFound);
        }
        let row = self
            .tx
            .query_one(
                "SELECT digest FROM th_object_pins WHERE tenant=$1 AND repository=$2 AND holder=$3",
                &[&scope.tenant, &scope.repository, &source],
            )
            .await?;
        let digests = vec![
            row.get::<_, Vec<u8>>(0),
            operations.key().id().as_bytes().to_vec(),
            result.key().id().as_bytes().to_vec(),
        ];
        self.tx.query("SELECT digest FROM th_objects WHERE tenant=$1 AND digest=ANY($2::bytea[]) ORDER BY digest FOR UPDATE",&[&scope.tenant,&digests]).await?;
        Ok(())
    }
}

impl Mutation<'_> {
    pub(crate) async fn register_base(
        &self,
        id: treeharbor_core::ObjectId,
        snapshot: treeharbor_core::ObjectId,
        lease: &ObjectLease,
    ) -> Result<()> {
        let revision = RevisionRef::Native(id).to_string();
        let scope = &self.request.scope;
        let holder = format!("base:{}", id.hex());
        self.pin_kind(&holder, lease, 5).await?;
        self.tx.execute("INSERT INTO th_bases (tenant,repository,revision,snapshot,pin_holder) VALUES ($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING",
            &[&scope.tenant,&scope.repository,&revision,&snapshot.as_bytes().as_slice(),&holder]).await?;
        let row = self
            .tx
            .query_one(
                "SELECT snapshot FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3",
                &[&scope.tenant, &scope.repository, &revision],
            )
            .await?;
        if row.get::<_, Vec<u8>>(0) != snapshot.as_bytes() {
            return Err(Error::ReceiptMismatch);
        }
        Ok(())
    }
}
