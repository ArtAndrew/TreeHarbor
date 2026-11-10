//! Repository-scoped assignment of external controller tasks. The lease
//! fences source and proposal access. Execution remains controller-owned.
use crate::{
    Begin, Error, Metadata, Mutation, Request, Result, Scope,
    access::{Authorization, Permission, repository_lock},
    api_runs::ApiRunSpec,
    catalog::settings,
    intake::CommandContext,
};
use serde_json::{Value, json};
use tokio_postgres::{Row, Transaction};
use treeharbor_core::{ProfileId, RevisionRef};
use uuid::Uuid;

pub struct RemoteTaskSpec {
    pub task: Uuid,
    pub base: RevisionRef,
    pub profile: ProfileId,
    pub agent: String,
    pub label: String,
    pub workers: Vec<String>,
}

impl RemoteTaskSpec {
    fn workers(&self) -> Result<Vec<String>> {
        if self.task.is_nil()
            || !matches!(self.base, RevisionRef::Native(_))
            || !(1..=16).contains(&self.workers.len())
        {
            return Err(Error::Invalid("remote task identity or workers"));
        }
        crate::identifier(&self.agent, 128)?;
        crate::identifier(&self.label, 256)?;
        let mut workers = self.workers.clone();
        workers.sort();
        for worker in &workers {
            crate::identifier(worker, 128)?;
        }
        if workers.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(Error::Invalid("duplicate remote worker"));
        }
        Ok(workers)
    }

    fn request(&self, ctx: &CommandContext, workers: &[String]) -> Result<Request> {
        ctx.request(
            "remote.task.enqueue.v1",
            json!([
                1,
                self.task,
                self.base.to_string(),
                self.profile.to_string(),
                self.agent,
                self.label,
                workers
            ]),
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RemoteAttempt {
    task: Uuid,
    generation: i64,
    workspace: Uuid,
}

pub(crate) enum AttemptTarget {
    Workspace(Uuid),
    Run(Uuid),
}

/// Called under the existing repository lock. Unassigned API runs retain the
/// legacy contract. An expired or superseded assignment never falls through.
pub(crate) async fn live_attempt_in(
    tx: &Transaction<'_>,
    scope: &Scope,
    principal: &str,
    target: AttemptTarget,
) -> Result<Option<RemoteAttempt>> {
    let (workspace, run) = match target {
        AttemptTarget::Workspace(id) => (Some(id), None),
        AttemptTarget::Run(id) => (None, Some(id)),
    };
    let row = tx
        .query_opt(
            r#"SELECT a.task,a.generation,a.workspace,a.worker,
        t.state='leased' AND a.generation=t.generation AND t.lease_until>clock_timestamp() AS live
        FROM th_remote_task_attempts a JOIN th_remote_tasks t USING(tenant,repository,task)
        WHERE a.tenant=$1 AND a.repository=$2 AND (a.workspace=$3 OR a.run=$4)"#,
            &[&scope.tenant(), &scope.repository(), &workspace, &run],
        )
        .await?;
    let Some(row) = row else { return Ok(None) };
    if row.get::<_, &str>("worker") != principal || !row.get::<_, bool>("live") {
        return Err(Error::StaleLease);
    }
    Ok(Some(RemoteAttempt {
        task: row.get("task"),
        generation: row.get("generation"),
        workspace: row.get("workspace"),
    }))
}

fn claim_request(ctx: &CommandContext, task: Option<Uuid>, ttl: i32) -> Result<Request> {
    crate::ttl_valid(ttl)?;
    if task.is_some_and(|id| id.is_nil()) {
        return Err(Error::Invalid("remote task identity"));
    }
    ctx.request("remote.task.claim.v1", json!([1, task, ttl]))
}

fn assigned_workspace(row: &Row) -> Result<Option<(Uuid, &str)>> {
    match (row.get("workspace"), row.get("worker")) {
        (Some(workspace), Some(worker)) => Ok(Some((workspace, worker))),
        (None, None) if row.get::<_, i64>("generation") == 0 => Ok(None),
        _ => Err(Error::SchemaMismatch),
    }
}

impl Metadata {
    pub async fn enqueue_remote_task(
        &mut self,
        ctx: &CommandContext,
        spec: &RemoteTaskSpec,
    ) -> Result<Value> {
        let workers = spec.workers()?;
        let tx = match self.begin(spec.request(ctx, &workers)?).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let base = spec.base.to_string();
        let profile = spec.profile.as_bytes().as_slice();
        let registered = tx.tx.query_one(r#"SELECT
            EXISTS(SELECT 1 FROM th_bases WHERE tenant=$1 AND repository=$2 AND revision=$3)
            AND EXISTS(SELECT 1 FROM th_source_profiles WHERE tenant=$1 AND repository=$2 AND digest=$4)"#,
            &[&s.tenant(),&s.repository(),&base,&profile]).await?.get::<_, bool>(0);
        if !registered {
            return Err(Error::NotFound);
        }
        let count = tx
            .tx
            .query_one(
                r#"SELECT count(*) FROM th_repository_access a
            JOIN th_principals p USING(tenant,principal)
            WHERE a.tenant=$1 AND a.repository=$2 AND a.principal=ANY($3)
            AND p.kind='agent' AND p.enabled
            AND a.permissions @> ARRAY['read','workspace.write','change.write']::text[]"#,
                &[&s.tenant(), &s.repository(), &workers],
            )
            .await?
            .get::<_, i64>(0);
        if count != workers.len() as i64 {
            return Err(Error::Forbidden);
        }
        let inserted = tx
            .tx
            .execute(
                r#"INSERT INTO th_remote_tasks
            (tenant,repository,task,owner,authority,epoch,base_revision,profile,agent,label,workers)
            VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) ON CONFLICT DO NOTHING"#,
                &[
                    &s.tenant(),
                    &s.repository(),
                    &spec.task,
                    &ctx.principal,
                    &ctx.authority.as_str(),
                    &ctx.epoch,
                    &base,
                    &profile,
                    &spec.agent,
                    &spec.label,
                    &workers,
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(Error::ExecutionState);
        }
        tx.complete(json!({"task":spec.task,"state":"queued","base":base,
            "profile_digest":spec.profile.to_string(),"authority":ctx.authority.as_str(),"epoch":ctx.epoch}), vec![]).await
    }

    /// A lost claim response can be replayed by key. No work means no durable
    /// claim and does not consume the key. The repository lock orders expiry,
    /// reclaim, cancel and accepted changes with respect to one another.
    pub async fn claim_remote_task(
        &mut self,
        ctx: &CommandContext,
        task: Option<Uuid>,
        ttl: i32,
    ) -> Result<Value> {
        let tx = match self.begin(claim_request(ctx, task, ttl)?).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let row = tx.tx.query_opt(r#"SELECT t.task,t.generation,t.base_revision,t.profile,t.agent,t.label,a.workspace,a.worker
            FROM th_remote_tasks t LEFT JOIN th_remote_task_attempts a USING(tenant,repository,task,generation)
            WHERE t.tenant=$1 AND t.repository=$2 AND ($3::uuid IS NULL OR t.task=$3)
              AND t.authority=$4 AND t.epoch=$5 AND $6=ANY(t.workers)
              AND (t.state='queued' OR (t.state='leased' AND t.lease_until<=clock_timestamp()))
              AND NOT EXISTS(SELECT 1 FROM th_remote_task_attempts p WHERE p.tenant=t.tenant
                  AND p.repository=t.repository AND p.task=t.task AND p.worker=$6)
            ORDER BY t.created_at,t.task LIMIT 1"#,
            &[&s.tenant(),&s.repository(),&task,&ctx.authority.as_str(),&ctx.epoch,&ctx.principal]).await?;
        let Some(row) = row else {
            tx.tx.commit().await?;
            return Ok(json!({"state":"empty"}));
        };
        if let Some((workspace, worker)) = assigned_workspace(&row)? {
            tx.close_api_workspace_for(workspace, 1, worker).await?;
        }
        let task: Uuid = row.get("task");
        let generation = row.get::<_, i64>("generation") + 1;
        let spec = ApiRunSpec {
            workspace: Uuid::new_v4(),
            run: Uuid::new_v4(),
            base: row.get::<_, &str>("base_revision").parse()?,
            profile: row
                .get::<_, Vec<u8>>("profile")
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?,
            agent: row.get("agent"),
            task: row.get("label"),
        };
        tx.create_api_run_effect(&spec).await?;
        tx.tx.execute(r#"INSERT INTO th_remote_task_attempts
            (tenant,repository,task,generation,worker,workspace,run) VALUES ($1,$2,$3,$4,$5,$6,$7)"#,
            &[&s.tenant(),&s.repository(),&task,&generation,&ctx.principal,&spec.workspace,&spec.run]).await?;
        let expiry: i64 = tx
            .tx
            .query_one(
                r#"UPDATE th_remote_tasks SET state='leased',generation=$4,
            lease_until=clock_timestamp()+($5::integer * interval '1 second')
            WHERE tenant=$1 AND repository=$2 AND task=$3
            RETURNING (extract(epoch FROM lease_until)*1000)::bigint"#,
                &[&s.tenant(), &s.repository(), &task, &generation, &ttl],
            )
            .await?
            .get(0);
        tx.complete(json!({"task":task,"state":"leased","attempt":generation,"worker":ctx.principal,
            "lease_expires_at_unix_ms":expiry,"run":spec.run,"workspace":spec.workspace,"generation":1,
            "authority":ctx.authority.as_str(),"epoch":ctx.epoch,"base":spec.base.to_string(),
            "profile_digest":treeharbor_core::ObjectId::from_bytes(spec.profile).to_string(),
            "agent":spec.agent,"label":spec.task,"execution_observed":false}), vec![]).await
    }

    pub async fn renew_remote_task(
        &mut self,
        ctx: &CommandContext,
        task: Uuid,
        attempt: i64,
        ttl: i32,
    ) -> Result<Value> {
        crate::ttl_valid(ttl)?;
        if task.is_nil() || !(1..=16).contains(&attempt) {
            return Err(Error::Invalid("remote task attempt"));
        }
        let tx = match self
            .begin(ctx.request("remote.task.renew.v1", json!([1, task, attempt, ttl]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let expiry: i64 = tx
            .tx
            .query_opt(
                r#"UPDATE th_remote_tasks t
            SET lease_until=GREATEST(t.lease_until,clock_timestamp()+($5::integer * interval '1 second'))
            FROM th_remote_task_attempts a WHERE t.tenant=$1 AND t.repository=$2 AND t.task=$3
              AND t.generation=$4 AND t.state='leased' AND t.lease_until>clock_timestamp()
              AND t.authority=$6 AND t.epoch=$7 AND a.tenant=t.tenant AND a.repository=t.repository
              AND a.task=t.task AND a.generation=t.generation AND a.worker=$8
            RETURNING (extract(epoch FROM t.lease_until)*1000)::bigint"#,
                &[
                    &s.tenant(),
                    &s.repository(),
                    &task,
                    &attempt,
                    &ttl,
                    &ctx.authority.as_str(),
                    &ctx.epoch,
                    &ctx.principal,
                ],
            )
            .await?
            .ok_or(Error::StaleLease)?
            .get(0);
        tx.complete(json!({"task":task,"attempt":attempt,"state":"leased","lease_expires_at_unix_ms":expiry}), vec![]).await
    }

    pub async fn remote_task_status(&mut self, auth: &Authorization, task: Uuid) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let row = tx.query_opt(r#"SELECT t.state,t.generation,t.authority,t.epoch,t.result_changeset,t.result_version,
            (extract(epoch FROM t.lease_until)*1000)::bigint AS expiry,
            t.state='leased' AND t.lease_until>clock_timestamp() AND r.state='active'
                AND r.authority=t.authority AND r.epoch=t.epoch AS live,
            a.workspace,a.run,a.worker,t.owner FROM th_remote_tasks t
            JOIN th_repositories r USING(tenant,repository)
            LEFT JOIN th_remote_task_attempts a USING(tenant,repository,task,generation)
            WHERE t.tenant=$1 AND t.repository=$2 AND t.task=$3 AND (t.owner=$4 OR $4=ANY(t.workers))"#,
            &[&s.tenant(),&s.repository(),&task,&auth.principal()]).await?.ok_or(Error::NotFound)?;
        let mut result = json!({"task":task,"state":row.get::<_,&str>("state"),
            "attempt":row.get::<_,i64>("generation"),"lease_active":row.get::<_,bool>("live"),
            "authority":row.get::<_,&str>("authority"),"epoch":row.get::<_,i64>("epoch"),
            "lease_expires_at_unix_ms":row.get::<_,Option<i64>>("expiry")});
        let worker: Option<&str> = row.get("worker");
        if row.get::<_, &str>("owner") == auth.principal() || worker == Some(auth.principal()) {
            result["worker"] = json!(worker);
            result["workspace"] = json!(row.get::<_, Option<Uuid>>("workspace"));
            result["run"] = json!(row.get::<_, Option<Uuid>>("run"));
            result["changeset"] = json!(row.get::<_, Option<Uuid>>("result_changeset"));
            result["version"] = json!(row.get::<_, Option<i64>>("result_version"));
        }
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(result)
    }

    pub async fn cancel_remote_task(&mut self, ctx: &CommandContext, task: Uuid) -> Result<Value> {
        let tx = match self
            .begin(ctx.request("remote.task.cancel.v1", json!([1, task]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(tx) => tx,
        };
        let s = &ctx.scope;
        let row = tx.tx.query_opt(r#"SELECT t.generation,a.workspace,a.worker
            FROM th_remote_tasks t LEFT JOIN th_remote_task_attempts a USING(tenant,repository,task,generation)
            WHERE t.tenant=$1 AND t.repository=$2 AND t.task=$3 AND t.owner=$4 AND t.authority=$5 AND t.epoch=$6
              AND t.state IN ('queued','leased')"#,
            &[&s.tenant(),&s.repository(),&task,&ctx.principal,&ctx.authority.as_str(),&ctx.epoch]).await?.ok_or(Error::ExecutionState)?;
        if let Some((workspace, worker)) = assigned_workspace(&row)? {
            tx.close_api_workspace_for(workspace, 1, worker).await?;
        }
        tx.tx.execute("UPDATE th_remote_tasks SET state='cancelled',lease_until=NULL WHERE tenant=$1 AND repository=$2 AND task=$3",
            &[&s.tenant(),&s.repository(),&task]).await?;
        tx.complete(json!({"task":task,"state":"cancelled"}), vec![])
            .await
    }
}

impl Mutation<'_> {
    pub(crate) async fn accept_remote_task(
        &self,
        attempt: &RemoteAttempt,
        changeset: Uuid,
    ) -> Result<Value> {
        let ctx = &self.request;
        let s = &ctx.scope;
        let updated = self
            .tx
            .execute(
                r#"UPDATE th_remote_tasks SET state='accepted',lease_until=NULL,
            result_changeset=$5,result_version=1
            WHERE tenant=$1 AND repository=$2 AND task=$3 AND generation=$4 AND state='leased'
            AND lease_until>clock_timestamp() AND authority=$6 AND epoch=$7"#,
                &[
                    &s.tenant(),
                    &s.repository(),
                    &attempt.task,
                    &attempt.generation,
                    &changeset,
                    &ctx.authority.as_str(),
                    &ctx.epoch,
                ],
            )
            .await?;
        if updated != 1 {
            return Err(Error::StaleLease);
        }
        self.close_api_workspace_for(attempt.workspace, 1, &ctx.principal)
            .await?;
        Ok(json!({"task":attempt.task,"attempt":attempt.generation}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Authority, commands::agent_https_command_allowed};

    #[test]
    fn remote_task_requests_match_independent_v1_vectors() {
        let vectors: Value =
            serde_json::from_str(include_str!("../tests/fixtures/remote-task-v1.json")).unwrap();
        for (name, network_allowed) in [("enqueue", false), ("claim", true)] {
            let vector = &vectors[name];
            let envelope = &vector["envelope"];
            let command = &envelope["command"];
            let text = |key: &str| command[key].as_str().unwrap();
            let ctx = CommandContext::trusted(
                Scope::new(
                    envelope["tenant"].as_str().unwrap(),
                    envelope["repository"].as_str().unwrap().parse().unwrap(),
                )
                .unwrap(),
                text("key").into(),
                vector["principal"].as_str().unwrap().into(),
                Authority::Standalone,
                1,
            );
            let task = text("task").parse().unwrap();
            let request = if name == "enqueue" {
                let spec = RemoteTaskSpec {
                    task,
                    base: text("base").parse().unwrap(),
                    profile: text("profile_digest").parse().unwrap(),
                    agent: text("agent").into(),
                    label: text("label").into(),
                    workers: serde_json::from_value(command["workers"].clone()).unwrap(),
                };
                spec.request(&ctx, &spec.workers().unwrap()).unwrap()
            } else {
                claim_request(
                    &ctx,
                    Some(task),
                    command["ttl_seconds"].as_i64().unwrap() as i32,
                )
                .unwrap()
            };
            let digest: String = request
                .digest
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(digest, vector["request_digest_hex"]);
            assert_eq!(
                agent_https_command_allowed(&serde_json::to_vec(envelope).unwrap()).unwrap(),
                network_allowed
            );
        }
    }
}
