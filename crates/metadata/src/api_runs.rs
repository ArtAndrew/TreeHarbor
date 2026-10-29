//! Caller-recorded task contexts. These provide immutable source/proposal
//! lineage, never observations about an external process, its isolation or tests.
use crate::{
    Begin, Error, Metadata, Mutation, Result,
    access::{Authorization, Permission, repository_lock},
    catalog::settings,
    intake::{CommandContext, WorkspaceFormat, WorkspaceSpec},
};
use serde_json::{Value, json};
use treeharbor_core::RevisionRef;
use uuid::Uuid;

pub struct ApiRunSpec {
    pub workspace: Uuid,
    pub run: Uuid,
    pub base: RevisionRef,
    pub profile: [u8; 32],
    pub agent: String,
    pub task: String,
}

impl ApiRunSpec {
    fn request(&self, ctx: &CommandContext) -> Result<crate::Request> {
        ctx.request(
            "api.run.create.v1",
            json!([
                1,
                self.workspace.to_string(),
                self.run.to_string(),
                self.base.to_string(),
                self.profile,
                self.agent,
                self.task
            ]),
        )
    }
}

impl Metadata {
    /// Atomically pins the exact source and records a caller-declared task.
    /// No preparation/execute job or runtime credential is created.
    pub async fn create_api_run(
        &mut self,
        ctx: &CommandContext,
        spec: &ApiRunSpec,
    ) -> Result<Value> {
        if spec.workspace.is_nil()
            || spec.run.is_nil()
            || !matches!(spec.base, RevisionRef::Native(_))
        {
            return Err(Error::Invalid("API run identity"));
        }
        crate::identifier(&spec.agent, 128)?;
        crate::identifier(&spec.task, 256)?;
        let request = spec.request(ctx)?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        if tx.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3) OR EXISTS(SELECT 1 FROM th_agent_runs WHERE tenant=$1 AND repository=$2 AND run=$4)", &[&s.tenant(),&s.repository(),&spec.workspace,&spec.run]).await?.get::<_,bool>(0) {
            return Err(Error::ExecutionState);
        }
        tx.create_api_workspace_effect(&WorkspaceSpec {
            id: spec.workspace,
            base: spec.base,
            profile: spec.profile,
            format: WorkspaceFormat::Native,
        })
        .await?;
        tx.tx.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,origin,state,runtime,started_at) VALUES($1,$2,$3,$4,1,$5,$6,$7,$8,'external_api','recorded',NULL,NULL)", &[&s.tenant(),&s.repository(),&spec.run,&spec.workspace,&ctx.epoch,&ctx.principal,&spec.agent,&spec.task]).await?;
        tx.complete(
            json!({"run":spec.run.to_string(),"workspace":spec.workspace.to_string(),
            "generation":1,"epoch":ctx.epoch,"base":spec.base.to_string(),
            "profile_digest":treeharbor_core::ObjectId::from_bytes(spec.profile).to_string(),
            "origin":"external_api","state":"recorded","workspace_state":"source_ready",
            "delivery":"api","execution_observed":false}),
            vec![],
        )
        .await
    }

    pub async fn api_run_status(&mut self, auth: &Authorization, run: Uuid) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let row=tx.query_opt("SELECT r.workspace,r.workspace_generation,r.epoch,r.state,w.state,w.generation,w.base_revision,w.profile FROM th_agent_runs r JOIN th_workspaces w USING(tenant,repository,workspace) WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 AND r.principal=$4 AND w.principal=$4 AND r.origin='external_api' AND w.delivery='api'", &[&s.tenant(),&s.repository(),&run,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        let result = json!({"run":run.to_string(),"workspace":row.get::<_,Uuid>(0).to_string(),
            "generation":row.get::<_,i64>(1),"epoch":row.get::<_,i64>(2),"state":row.get::<_,String>(3),
            "workspace_state":row.get::<_,String>(4),"workspace_generation":row.get::<_,i64>(5),
            "base":row.get::<_,String>(6),"profile_digest":treeharbor_core::ObjectId::from_bytes(row.get::<_,Vec<u8>>(7).try_into().map_err(|_|Error::SchemaMismatch)?).to_string(),
            "origin":"external_api","delivery":"api","execution_observed":false});
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    /// Fences server reads/writes and releases only the logical source pin.
    /// This does not stop or delete any process/files owned by an external caller.
    pub async fn close_api_run(
        &mut self,
        ctx: &CommandContext,
        run: Uuid,
        generation: i64,
    ) -> Result<Value> {
        let request = ctx.request("api.run.close.v1", json!([1, run.to_string(), generation]))?;
        let tx = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let row=tx.tx.query_opt("SELECT workspace FROM th_agent_runs WHERE tenant=$1 AND repository=$2 AND run=$3 AND principal=$4 AND origin='external_api' AND epoch=$5 AND workspace_generation=$6", &[&s.tenant(),&s.repository(),&run,&ctx.principal,&ctx.epoch,&generation]).await?.ok_or(Error::ExecutionState)?;
        let result = tx
            .close_api_workspace_effect(row.get(0), generation)
            .await?;
        tx.complete(result, vec![]).await
    }
}

impl Mutation<'_> {
    pub(crate) async fn close_api_workspace_effect(
        &self,
        workspace: Uuid,
        generation: i64,
    ) -> Result<Value> {
        let next = generation
            .checked_add(1)
            .filter(|_| generation > 0)
            .ok_or(Error::Invalid("workspace generation"))?;
        let ctx = &self.request;
        let s = &ctx.scope;
        // Fail closed if a future/incorrect producer attached resources. A logical
        // close must never bypass the physical cleanup/removal protocol.
        let row=self.tx.query_opt(r#"SELECT w.pin_holder,r.run FROM th_workspaces w
            JOIN th_agent_runs r USING(tenant,repository,workspace)
            WHERE w.tenant=$1 AND w.repository=$2 AND w.workspace=$3 AND w.principal=$4 AND r.principal=$4
            AND w.epoch=$5 AND r.epoch=$5 AND w.generation=$6 AND r.workspace_generation=$6
            AND w.delivery='api' AND w.state='source_ready' AND w.node IS NULL AND w.runtime IS NULL
            AND r.origin='external_api' AND r.state='recorded' AND r.runtime IS NULL AND r.started_at IS NULL
            AND NOT EXISTS(SELECT 1 FROM th_agent_runs x WHERE x.tenant=w.tenant AND x.repository=w.repository AND x.workspace=w.workspace AND x.run<>r.run)
            AND NOT EXISTS(SELECT 1 FROM th_workspace_preparations p WHERE p.tenant=w.tenant AND p.repository=w.repository AND p.workspace=w.workspace)
            AND NOT EXISTS(SELECT 1 FROM th_managed_runs m WHERE m.tenant=r.tenant AND m.repository=r.repository AND m.run=r.run)
            AND NOT EXISTS(SELECT 1 FROM th_workspace_cleanups c WHERE c.tenant=w.tenant AND c.repository=w.repository AND c.workspace=w.workspace)"#,
            &[&s.tenant(),&s.repository(),&workspace,&ctx.principal,&ctx.epoch,&generation]).await?.ok_or(Error::ExecutionState)?;
        let holder: String = row.get(0);
        let run: Uuid = row.get(1);
        self.tx.execute("UPDATE th_agent_runs SET state='closed' WHERE tenant=$1 AND repository=$2 AND run=$3", &[&s.tenant(),&s.repository(),&run]).await?;
        self.tx.execute("UPDATE th_workspaces SET state='deleted',generation=$4,pin_holder=NULL WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&s.tenant(),&s.repository(),&workspace,&next]).await?;
        self.unpin_object(&holder).await?;
        Ok(
            json!({"workspace":workspace.to_string(),"generation":next,"state":"deleted",
            "run":run.to_string(),"run_state":"closed","origin":"external_api","delivery":"api","execution_observed":false}),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_creation_request_matches_independent_v1_vector() {
        let vector: Value =
            serde_json::from_str(include_str!("../tests/fixtures/api-run-v1.json")).unwrap();
        let envelope = &vector["envelope"];
        let c = &envelope["command"];
        let ctx = CommandContext::trusted(
            crate::Scope::new(
                "example",
                envelope["repository"].as_str().unwrap().parse().unwrap(),
            )
            .unwrap(),
            c["key"].as_str().unwrap().into(),
            vector["principal"].as_str().unwrap().into(),
            crate::Authority::Standalone,
            1,
        );
        let spec = ApiRunSpec {
            workspace: c["workspace"].as_str().unwrap().parse().unwrap(),
            run: c["run"].as_str().unwrap().parse().unwrap(),
            base: c["base"].as_str().unwrap().parse().unwrap(),
            profile: [4; 32],
            agent: c["agent"].as_str().unwrap().into(),
            task: c["task"].as_str().unwrap().into(),
        };
        let request = spec.request(&ctx).unwrap();
        let digest: String = request.digest.iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(digest, vector["request_digest_hex"]);
    }
}
