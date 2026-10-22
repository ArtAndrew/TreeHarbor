//! Narrow managed CI path. Callers choose a check run, never argv, outcome or
//! evidence. The trusted node's removed runtime observation supplies the result.
use super::candidates::{CandidateRecord, candidate, fresh};
use super::evidence::{current_run, trusted};
use super::program::load_program;
use super::*;
use crate::intake::{WorkspaceFormat, WorkspaceSpec};
use crate::runs::RunRequest;
use sha2::{Digest, Sha256};
use treeharbor_core::SourceProfile;
use treeharbor_runtime::Profile;

struct Execution {
    candidate: Uuid,
    context: Vec<u8>,
    program: ObjectId,
    validator: String,
    token: Uuid,
    workspace: Uuid,
    run: Uuid,
    state: String,
    receipt: Option<Uuid>,
}
impl Execution {
    fn json(&self, check: Uuid) -> Value {
        json!({"check_run":check.to_string(),"candidate":self.candidate.to_string(),"workspace":self.workspace.to_string(),"run":self.run.to_string(),"state":self.state,"receipt":self.receipt.map(|v|v.to_string())})
    }
}
async fn status(tx: &Transaction<'_>, s: &Scope, e: &Execution, check: Uuid) -> Result<Value> {
    let mut result = e.json(check);
    if let Some(receipt) = e.receipt {
        let row=tx.query_opt("SELECT outcome,evidence FROM th_native_validations WHERE tenant=$1 AND repository=$2 AND receipt=$3", &[&s.tenant,&s.repository,&receipt]).await?.ok_or(Error::SchemaMismatch)?;
        result["outcome"] = json!(row.get::<_, String>(0));
        result["evidence"] = json!(id(row.get(1))?.to_string());
    }
    Ok(result)
}
async fn execution(tx: &Transaction<'_>, s: &Scope, check: Uuid) -> Result<Option<Execution>> {
    tx.query_opt(
        "SELECT * FROM th_check_executions WHERE tenant=$1 AND repository=$2 AND check_run=$3",
        &[&s.tenant, &s.repository, &check],
    )
    .await?
    .map(|r| {
        Ok(Execution {
            candidate: r.get("candidate"),
            context: r.get("context"),
            program: id(r.get("program"))?,
            validator: r.get("validator"),
            token: r.get("token"),
            workspace: r.get("workspace"),
            run: r.get("execution_run"),
            state: r.get("state"),
            receipt: r.get("receipt"),
        })
    })
    .transpose()
}
async fn approved(
    tx: &Transaction<'_>,
    s: &Scope,
    check_run: Uuid,
    validator: &str,
) -> Result<(CandidateRecord, Vec<u8>, CheckProgram, Profile)> {
    let (cid, name, assigned, context) = current_run(tx, s, check_run).await?;
    if assigned != validator {
        return Err(Error::Forbidden);
    }
    let c = candidate(tx, s, cid).await?;
    if c.state != "checking" {
        return Err(Error::IntegrationState);
    }
    fresh(tx, s, &c).await?;
    let policy = load_policy(tx, s, c.policy.as_bytes()).await?;
    let definition = policy
        .checks
        .iter()
        .find(|d| d.name == name)
        .ok_or(Error::SchemaMismatch)?;
    let check = policy.check(&name, &check_run.to_string())?;
    if !definition.validators.contains(validator)
        || context != treeharbor_validation::context_digest(&c.validation(s)?, &check).as_bytes()
    {
        return Err(Error::Forbidden);
    }
    let (program, profile) = load_program(tx, s, definition.definition.parse()?).await?;
    program.matches(definition, &profile)?;
    if tx.query_opt("SELECT 1 FROM th_runtime_profile_revocations WHERE tenant=$1 AND repository=$2 AND digest=$3", &[&s.tenant,&s.repository,&profile.digest().as_slice()]).await?.is_some() {return Err(Error::ExecutionState);}
    Ok((c, context, program, profile))
}
async fn still_authorized(tx: &Transaction<'_>, s: &Scope, e: &Execution) -> Result<()> {
    if !trusted(
        tx,
        s,
        &e.validator,
        "validator",
        "validation.report",
        Some(e.token),
    )
    .await?
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}
impl Metadata {
    pub async fn prepare_native_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        check: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.check.prepare.v1",
                json!([1, check.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let (c, context, program, _) = approved(&tx.tx, s, check, auth.principal()).await?;
        if let Some(e) = execution(&tx.tx, s, check).await? {
            still_authorized(&tx.tx, s, &e).await?;
            return tx.complete(e.json(check), vec![]).await;
        }
        let full = SourceProfile::full();
        tx.tx.execute("INSERT INTO th_source_profiles(tenant,repository,digest,definition,principal) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING", &[&s.tenant,&s.repository,&full.id().as_bytes().as_slice(),&full.encode(),&auth.principal()]).await?;
        let workspace = Uuid::new_v4();
        let run = Uuid::new_v4();
        let _created = tx
            .create_workspace_effect(&WorkspaceSpec {
                id: workspace,
                base: RevisionRef::Native(c.revision.ok_or(Error::IntegrationState)?),
                profile: *full.id().as_bytes(),
                format: WorkspaceFormat::Native,
            })
            .await?;
        tx.tx.execute("INSERT INTO th_check_executions(tenant,repository,check_run,candidate,context,program,validator,token,workspace,execution_run,state) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'preparing')", &[&s.tenant,&s.repository,&check,&c.id,&context,&bytes(program.digest()?),&auth.principal(),&auth.token_id(),&workspace,&run]).await?;
        let e = execution(&tx.tx, s, check)
            .await?
            .ok_or(Error::SchemaMismatch)?;
        tx.complete(
            e.json(check),
            vec![Event {
                kind: "workspace.prepare".into(),
                payload:json!({"workspace":workspace.to_string(),"generation":1,"epoch":cmd.epoch}),
            }],
        )
        .await
    }
    pub async fn start_native_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        check: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.check.start.v1",
                json!([1, check.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let (c, context, program, profile) = approved(&tx.tx, s, check, auth.principal()).await?;
        let e = execution(&tx.tx, s, check).await?.ok_or(Error::NotFound)?;
        still_authorized(&tx.tx, s, &e).await?;
        if e.candidate != c.id || e.context != context || e.program != program.digest()? {
            return Err(Error::SchemaMismatch);
        }
        if e.state != "preparing" {
            return tx.complete(e.json(check), vec![]).await;
        }
        let full = SourceProfile::full();
        if tx.tx.query_opt("SELECT 1 FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND principal=$4 AND generation=1 AND epoch=$5 AND base_revision=$6 AND resolved_profile=$7 AND profile=$7 AND format='native'", &[&s.tenant,&s.repository,&e.workspace,&auth.principal(),&c.epoch,&RevisionRef::Native(c.revision.ok_or(Error::IntegrationState)?).to_string(),&full.id().as_bytes().as_slice()]).await?.is_none(){return Err(Error::ExecutionState);}
        let payload = tx
            .request_run_effect(&RunRequest {
                run: e.run,
                workspace: e.workspace,
                generation: 1,
                profile: profile.digest(),
                argv: program.argv,
                agent: "treeharbor-check-v1".into(),
                task: check.to_string(),
            })
            .await?;
        tx.tx.execute("UPDATE th_check_executions SET state='requested' WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant,&s.repository,&check]).await?;
        let result = execution(&tx.tx, s, check)
            .await?
            .ok_or(Error::SchemaMismatch)?
            .json(check);
        tx.complete(
            result,
            vec![Event {
                kind: "run.execute".into(),
                payload,
            }],
        )
        .await
    }
    pub async fn native_check_execution_status(
        &mut self,
        auth: &Authorization,
        check: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        read_authorized(&tx, auth).await?;
        let e = execution(&tx, auth.scope(), check)
            .await?
            .ok_or(Error::NotFound)?;
        if e.validator != auth.principal() {
            return Err(Error::Forbidden);
        }
        let result = status(&tx, auth.scope(), &e, check).await?;
        read_authorized(&tx, auth).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn collect_native_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        check: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.check.collect.v1",
                json!([1, check.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let e = execution(&tx.tx, s, check).await?.ok_or(Error::NotFound)?;
        if e.validator != auth.principal() {
            return Err(Error::Forbidden);
        }
        still_authorized(&tx.tx, s, &e).await?;
        if e.state == "collected" {
            let result = status(&tx.tx, s, &e, check).await?;
            return tx.complete(result, vec![]).await;
        }
        if e.state != "requested" {
            return Err(Error::ExecutionState);
        }
        // Collection after cancellation may record a terminal result, but cannot
        // make it current again. Land independently checks current context/trust.
        let (definition, result) = observed_evidence(&tx.tx, s, check, &e).await?;
        let evidence = evidence_digest(&definition);
        let receipt = Uuid::new_v4();
        tx.tx.execute("INSERT INTO th_native_validations(tenant,repository,receipt,run,principal,token,context,outcome,evidence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9)", &[&s.tenant,&s.repository,&receipt,&check,&auth.principal(),&auth.token_id(),&e.context,&result,&bytes(evidence)]).await?;
        tx.tx.execute("INSERT INTO th_check_execution_evidence(tenant,repository,receipt,check_run,digest,definition) VALUES($1,$2,$3,$4,$5,$6)", &[&s.tenant,&s.repository,&receipt,&check,&bytes(evidence),&definition]).await?;
        tx.tx.execute("UPDATE th_check_executions SET state='collected',receipt=$4 WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant,&s.repository,&check,&receipt]).await?;
        let saved = execution(&tx.tx, s, check)
            .await?
            .ok_or(Error::SchemaMismatch)?;
        let result = status(&tx.tx, s, &saved, check).await?;
        tx.complete(result, vec![]).await
    }
}
fn evidence_digest(definition: &[u8]) -> ObjectId {
    let mut h = Sha256::new();
    h.update(b"treeharbor-check-execution-evidence\0v1\0");
    h.update(definition);
    ObjectId::from_bytes(h.finalize().into())
}
async fn observed_evidence(
    tx: &Transaction<'_>,
    s: &Scope,
    check: Uuid,
    e: &Execution,
) -> Result<(Vec<u8>, String)> {
    let c = candidate(tx, s, e.candidate).await?;
    let (program, profile) = load_program(tx, s, e.program).await?;
    let row=tx.query_opt(r#"SELECT m.*,r.state AS run_state,r.principal,r.workspace,r.workspace_generation,r.epoch,
        p.base_revision,p.snapshot,p.profile AS source_profile,p.manifest_digest,p.node AS prepared_node,
        p.workspace AS prepared_workspace,p.generation AS prepared_generation,p.epoch AS prepared_epoch,
        p.storage_instance AS prepared_storage,p.slot AS prepared_slot
        FROM th_managed_runs m JOIN th_agent_runs r USING(tenant,repository,run)
        JOIN th_workspace_preparations p ON p.attempt=m.preparation
        WHERE m.tenant=$1 AND m.repository=$2 AND m.run=$3 AND p.tenant=m.tenant AND p.repository=m.repository"#,&[&s.tenant,&s.repository,&e.run]).await?.ok_or(Error::ExecutionState)?;
    let r = &row;
    let revision = RevisionRef::Native(c.revision.ok_or(Error::IntegrationState)?).to_string();
    let snapshot = c.snapshot.ok_or(Error::IntegrationState)?;
    let argv = serde_json::to_vec(&program.argv).map_err(|_| Error::SchemaMismatch)?;
    let node: String = r.get("node");
    if !r.get::<_, bool>("removed")
        || r.get::<_, String>("principal") != e.validator
        || r.get::<_, Uuid>("workspace") != e.workspace
        || r.get::<_, i64>("workspace_generation") != 1
        || r.get::<_, i64>("epoch") != c.epoch
        || r.get::<_, String>("authority") != "standalone"
        || r.get::<_, String>("base_revision") != revision
        || r.get::<_, Vec<u8>>("snapshot") != bytes(snapshot)
        || r.get::<_, Vec<u8>>("source_profile") != SourceProfile::full().id().as_bytes()
        || r.get::<_, Option<Vec<u8>>>("manifest_digest").is_none()
        || r.get::<_, Vec<u8>>("profile") != profile.digest()
        || r.get::<_, Vec<u8>>("argv") != argv
        || r.get::<_, String>("prepared_node") != node
        || r.get::<_, Uuid>("prepared_workspace") != e.workspace
        || r.get::<_, i64>("prepared_generation") != 1
        || r.get::<_, i64>("prepared_epoch") != c.epoch
        || r.get::<_, Uuid>("prepared_storage") != r.get::<_, Uuid>("storage_instance")
        || r.get::<_, Uuid>("prepared_slot") != r.get::<_, Uuid>("slot")
    {
        return Err(Error::ExecutionState);
    }
    if !trusted(tx, s, &node, "node", "runtime.control", None).await? {
        return Err(Error::Forbidden);
    }
    let state: String = r.get("run_state");
    let exit: Option<i64> = r.get("exit_code");
    let oom: Option<bool> = r.get("oom_killed");
    if state == "succeeded"
        && ["runtime_started_at", "runtime_finished_at"]
            .iter()
            .any(|field| {
                r.get::<_, Option<String>>(*field)
                    .is_none_or(|v| v.is_empty() || v == "0001-01-01T00:00:00Z")
            })
    {
        return Err(Error::ExecutionState);
    }
    let outcome = match state.as_str() {
        "succeeded"
            if exit == Some(0) && oom == Some(false) && !r.get::<_, bool>("stop_requested") =>
        {
            "passed"
        }
        "failed" if exit.is_some() => "failed",
        "cancelled" => "cancelled",
        "lost" => "error",
        _ => return Err(Error::ExecutionState),
    }
    .to_owned();
    let definition = serde_json::to_vec(&json!([
        1,
        s.tenant,
        s.repository.to_string(),
        check.to_string(),
        e.candidate.to_string(),
        ObjectId::from_bytes(
            e.context
                .clone()
                .try_into()
                .map_err(|_| Error::SchemaMismatch)?
        )
        .to_string(),
        e.program.to_string(),
        e.validator,
        e.token.to_string(),
        e.workspace.to_string(),
        e.run.to_string(),
        revision,
        snapshot.to_string(),
        program.runtime_profile,
        program.argv,
        r.get::<_, Uuid>("attempt").to_string(),
        r.get::<_, Uuid>("preparation").to_string(),
        id(r.get("binding"))?.to_string(),
        id(r.get("manifest_digest"))?.to_string(),
        node,
        r.get::<_, Uuid>("storage_instance").to_string(),
        r.get::<_, Uuid>("slot").to_string(),
        id(r.get("volume_digest"))?.to_string(),
        r.get::<_, Option<String>>("engine"),
        r.get::<_, Option<String>>("container"),
        state,
        exit,
        oom,
        r.get::<_, Option<String>>("runtime_started_at"),
        r.get::<_, Option<String>>("runtime_finished_at"),
        r.get::<_, bool>("stop_requested"),
        r.get::<_, Option<i64>>("finished_generation"),
        r.get::<_, Option<Uuid>>("finished_worker")
            .map(|v| v.to_string()),
        outcome
    ]))
    .map_err(|_| Error::SchemaMismatch)?;
    if definition.len() > 65536 {
        return Err(Error::Invalid("check evidence size"));
    }
    Ok((definition, outcome))
}
/// Called inside the publication transaction; takes current node/principal locks
/// and recomputes the immutable runtime evidence rather than trusting a JSON flag.
pub(super) async fn admission(
    tx: &Transaction<'_>,
    s: &Scope,
    check: Uuid,
    receipt: Uuid,
) -> Result<()> {
    let e = execution(tx, s, check)
        .await?
        .ok_or(Error::AdmissionBlocked)?;
    still_authorized(tx, s, &e).await?;
    if e.state != "collected" || e.receipt != Some(receipt) {
        return Err(Error::AdmissionBlocked);
    }
    approved(tx, s, check, &e.validator).await?;
    let (definition, outcome) = observed_evidence(tx, s, check, &e).await?;
    if outcome != "passed" {
        return Err(Error::AdmissionBlocked);
    }
    let digest = evidence_digest(&definition);
    if tx.query_opt("SELECT 1 FROM th_check_execution_evidence e JOIN th_native_validations v USING(tenant,repository,receipt) WHERE e.tenant=$1 AND e.repository=$2 AND e.receipt=$3 AND e.check_run=$4 AND e.digest=$5 AND e.definition=$6 AND v.evidence=e.digest AND v.outcome='passed' AND v.context=$7", &[&s.tenant,&s.repository,&receipt,&check,&bytes(digest),&definition,&e.context]).await?.is_none(){return Err(Error::AdmissionBlocked);}
    Ok(())
}

/// None denotes an ordinary owner task. A managed check must remain assigned to
/// this exact live context and its original credential throughout execution.
pub(crate) async fn execution_fence(
    tx: &Transaction<'_>,
    s: &Scope,
    run: Uuid,
) -> Result<Option<bool>> {
    let row=tx.query_opt("SELECT check_run FROM th_check_executions WHERE tenant=$1 AND repository=$2 AND execution_run=$3", &[&s.tenant,&s.repository,&run]).await?;
    let Some(row) = row else { return Ok(None) };
    let check = row.get(0);
    let e = execution(tx, s, check)
        .await?
        .ok_or(Error::SchemaMismatch)?;
    let result = async {
        still_authorized(tx, s, &e).await?;
        let (c, context, p, _) = approved(tx, s, check, &e.validator).await?;
        if e.state != "requested"
            || c.id != e.candidate
            || context != e.context
            || p.digest()? != e.program
        {
            return Err(Error::ExecutionState);
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(Some(false)),
        Err(
            Error::Forbidden
            | Error::IntegrationState
            | Error::StaleCandidate
            | Error::ExecutionState,
        ) => Ok(Some(true)),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_evidence_domain_and_encoding_match_independent_vector() {
        let vector: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/check-program-v1.json"))
                .unwrap();
        let bytes = serde_json::to_vec(&vector["evidence"]).unwrap();
        assert_eq!(
            bytes,
            vector["evidence_canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(evidence_digest(&bytes).hex(), vector["evidence_digest"]);
    }
}
