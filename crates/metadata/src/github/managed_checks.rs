//! Owner-approved offline checks of an imported, exact GitHub PR or queue candidate.
//! Runtime evidence is independent of GitHub workflow results and admission.
use super::*;
mod pull;
use crate::{
    check_runtime::{self, Subject},
    intake::{WorkspaceFormat, WorkspaceSpec},
    integration::{CheckProgram, load_program, trusted},
    runs::RunRequest,
};
pub use pull::ManagedPullCheckSpec;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treeharbor_core::{ObjectId, SourceProfile};
use treeharbor_github_contract::{publication::Target, queue::QueueObservation};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedCheckSpec {
    pub queue: Uuid,
    pub membership_sequence: i64,
    pub import: Uuid,
    pub validator: String,
    pub program: CheckProgram,
}
fn oid(v: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        v.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
async fn check(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt(
        "SELECT * FROM th_github_managed_checks WHERE tenant=$1 AND repository=$2 AND check_run=$3",
        &[&s.tenant(), &s.repository(), &id],
    )
    .await?
    .ok_or(Error::NotFound)
}
async fn execution(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Option<Row>> {
    Ok(tx.query_opt("SELECT * FROM th_github_managed_executions WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant(), &s.repository(), &id]).await?)
}
async fn validator(tx: &Transaction<'_>, s: &Scope, c: &Row, e: Option<&Row>) -> Result<()> {
    if !trusted(
        tx,
        s,
        &c.get::<_, String>("validator"),
        "validator",
        "validation.report",
        e.map(|r| r.get("token")),
    )
    .await?
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}
async fn fresh(tx: &Transaction<'_>, s: &Scope, c: &Row) -> Result<()> {
    let epoch: i64 = c.get("epoch");
    let repo = tx
        .query_one(
            "SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&s.tenant(), &s.repository()],
        )
        .await?;
    if repo.get::<_, String>(0) != "connected"
        || repo.get::<_, i64>(1) != epoch
        || repo.get::<_, String>(2) != "active"
    {
        return Err(Error::Fenced);
    }
    if !trusted(
        tx,
        s,
        &c.get::<_, String>("principal"),
        "human",
        "policy.write",
        Some(c.get("token")),
    )
    .await?
    {
        return Err(Error::Forbidden);
    }
    if tx.query_opt("SELECT 1 FROM th_github_managed_check_revocations WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant(), &s.repository(), &c.get::<_,Uuid>("check_run")]).await?.is_some(){return Err(Error::ExecutionState);}
    match (
        c.get::<_, Option<Uuid>>("queue"),
        c.get::<_, Option<Uuid>>("pull"),
    ) {
        (Some(_), None) => fresh_queue(tx, s, c, epoch).await?,
        (None, Some(_)) => pull::fresh(tx, s, c).await?,
        _ => return Err(Error::SchemaMismatch),
    }
    let (_, profile) = load_program(tx, s, oid(c.get("program"))?).await?;
    if tx.query_opt("SELECT 1 FROM th_runtime_profile_revocations WHERE tenant=$1 AND repository=$2 AND digest=$3", &[&s.tenant(), &s.repository(), &profile.digest().as_slice()]).await?.is_some(){return Err(Error::ExecutionState);}
    Ok(())
}
async fn fresh_queue(tx: &Transaction<'_>, s: &Scope, c: &Row, epoch: i64) -> Result<()> {
    let q = queue::load(tx, s, c.get("queue")).await?;
    let b = binding(tx, s, q.get("binding")).await?;
    if q.get::<_, i64>("epoch") != epoch
        || b.get::<_, i64>("epoch") != epoch
        || b.get::<_, String>("state") != "active"
        || q.get::<_, bool>("cancel_requested")
        || !queue::current(tx, s, &q).await?
    {
        return Err(Error::HostQueueState);
    }
    check_policy::fresh(tx, s, &q).await?;
    let t = queue::target(&q)?;
    let g =
        group_checks::membership(tx, s, c.get("queue"), c.get("membership_sequence"), &t).await?;
    let selected: Target =
        serde_json::from_value(c.get("subject")).map_err(|_| Error::SchemaMismatch)?;
    selected
        .validate_source(&t, &g)
        .map_err(|_| Error::SchemaMismatch)?;
    // This only proves freshness against retained provider observations. The
    // publishing gate must still reread GitHub; no network transaction is implied.
    let latest: QueueObservation = serde_json::from_value(
        q.get::<_, Option<Value>>("latest_observation")
            .ok_or(Error::HostQueueState)?,
    )
    .map_err(|_| Error::SchemaMismatch)?;
    let latest = latest.group_capture.ok_or(Error::HostQueueState)?;
    if !latest.consistent(&t) || latest.before.selected(&t) != Some(&selected.member) {
        return Err(Error::HostQueueState);
    }
    Ok(())
}
async fn owned(tx: &Transaction<'_>, auth: &Authorization, epoch: i64, id: Uuid) -> Result<Row> {
    context(tx, auth, epoch, Permission::ValidationReport).await?;
    let c = check(tx, auth.scope(), id).await?;
    if c.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    if c.get::<_, String>("validator") != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok(c)
}
async fn evidence(
    tx: &Transaction<'_>,
    s: &Scope,
    c: &Row,
    e: &Row,
) -> Result<(Vec<u8>, Vec<u8>, String)> {
    let id: Uuid = c.get("check_run");
    let (p, profile) = load_program(tx, s, oid(c.get("program"))?).await?;
    let subject = Subject {
        run: e.get("execution_run"),
        workspace: e.get("workspace"),
        validator: &c.get::<_, String>("validator"),
        epoch: c.get::<_, i64>("epoch"),
        authority: "connected",
        revision: c.get::<_, String>("revision").parse()?,
        snapshot: oid(c.get("snapshot"))?,
    };
    let (observed, outcome) = check_runtime::observe(tx, s, &subject, &p, &profile).await?;
    let definition = json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"check_run":id,"epoch":c.get::<_,i64>("epoch"),"queue":c.get::<_,Option<Uuid>>("queue"),"subject":c.get::<_,Value>("subject"),"import":c.get::<_,Uuid>("import"),"revision":subject.revision.to_string(),"snapshot":subject.snapshot.to_string(),"program":oid(c.get("program"))?.to_string(),"runtime_profile":p.runtime_profile,"image":profile.definition().image,"argv":p.argv,"owner":c.get::<_,String>("principal"),"owner_token":c.get::<_,Uuid>("token"),"validator":subject.validator,"validator_token":e.get::<_,Uuid>("token"),"workspace":subject.workspace,"run":subject.run,"runtime":check_runtime::facts(&observed),"outcome":outcome});
    let (definition, evidence) = encode_evidence(definition, c.get("pull"))?;
    Ok((definition, evidence, outcome))
}
fn encode_evidence(mut definition: Value, pull: Option<Uuid>) -> Result<(Vec<u8>, Vec<u8>)> {
    let domain = if let Some(pull) = pull {
        definition["version"] = json!(2);
        definition
            .as_object_mut()
            .ok_or(Error::SchemaMismatch)?
            .remove("queue");
        definition["pull"] = json!(pull);
        b"treeharbor-github-check-execution\0v2\0"
    } else {
        b"treeharbor-github-check-execution\0v1\0"
    };
    let definition = serde_json::to_vec(&definition).map_err(|_| Error::SchemaMismatch)?;
    if definition.len() > 65536 {
        return Err(Error::Invalid("check evidence size"));
    }
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(&definition);
    let evidence: Vec<u8> = hash.finalize().to_vec();
    Ok((definition, evidence))
}
async fn describe(tx: &Transaction<'_>, s: &Scope, c: &Row) -> Result<Value> {
    let id: Uuid = c.get("check_run");
    let e = execution(tx, s, id).await?;
    let mut current = match fresh(tx, s, c).await {
        Ok(()) => match validator(tx, s, c, e.as_ref()).await {
            Ok(()) => true,
            Err(Error::Forbidden) => false,
            Err(e) => return Err(e),
        },
        Err(Error::Forbidden | Error::Fenced | Error::HostQueueState | Error::ExecutionState) => {
            false
        }
        Err(e) => return Err(e),
    };
    let mut v = json!({"version":1,"check_run":id,"queue":c.get::<_,Option<Uuid>>("queue"),"subject":c.get::<_,Value>("subject"),"revision":c.get::<_,String>("revision"),"snapshot":oid(c.get("snapshot"))?.to_string(),"program":oid(c.get("program"))?.to_string(),"validator":c.get::<_,String>("validator"),"state":"approved","current_locally":current,"execution_context_verified":false,"admission_verified":false,"host_policy_verified":false,"merge_verified":false});
    if let Some(pull) = c.get::<_, Option<Uuid>>("pull") {
        v["version"] = json!(2);
        v.as_object_mut()
            .ok_or(Error::SchemaMismatch)?
            .remove("queue");
        v["pull"] = json!(pull);
    }
    if let Some(e) = e {
        v["state"] = json!(e.get::<_, String>("state"));
        v["workspace"] = json!(e.get::<_, Uuid>("workspace"));
        v["run"] = json!(e.get::<_, Uuid>("execution_run"));
        v["outcome"] = json!(e.get::<_, Option<String>>("outcome"));
        if let Some(bytes) = e.get::<_, Option<Vec<u8>>>("evidence") {
            v["evidence"] = json!(oid(bytes)?.to_string());
            let verified = match evidence(tx, s, c, &e).await {
                Ok((definition, digest, outcome)) => {
                    if e.get::<_, Option<Vec<u8>>>("definition").as_deref()
                        != Some(definition.as_slice())
                        || e.get::<_, Option<Vec<u8>>>("evidence").as_deref()
                            != Some(digest.as_slice())
                        || e.get::<_, Option<String>>("outcome").as_deref()
                            != Some(outcome.as_str())
                    {
                        return Err(Error::SchemaMismatch);
                    }
                    outcome == "passed"
                }
                Err(Error::Forbidden) => {
                    current = false;
                    false
                }
                Err(e) => return Err(e),
            };
            v["execution_context_verified"] = json!(verified && current);
        }
    }
    v["current_locally"] = json!(current);
    Ok(v)
}
impl Metadata {
    pub async fn approve_github_managed_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        spec: &ManagedCheckSpec,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(spec.queue)?;
        uuid(spec.import)?;
        crate::identifier(&spec.validator, 128)?;
        let definition = spec.program.canonical()?;
        let program = spec.program.digest()?;
        let profile: ObjectId = spec.program.runtime_profile.parse()?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.approve.v1",
                json!([1, id, spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        context(&m.tx, auth, cmd.epoch, Permission::PolicyWrite).await?;
        if m.tx.query_opt("SELECT 1 FROM th_github_managed_checks WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant(), &s.repository(), &id]).await?.is_some() {
            return Err(Error::ExecutionState);
        }
        if m.tx
            .query_opt(
                "SELECT 1 FROM th_runtime_profiles WHERE tenant=$1 AND repository=$2 AND digest=$3",
                &[&s.tenant(), &s.repository(), &profile.as_bytes().as_slice()],
            )
            .await?
            .is_none()
        {
            return Err(Error::NotFound);
        }
        let q = queue::load(&m.tx, s, spec.queue).await?;
        let t = queue::target(&q)?;
        let g =
            group_checks::membership(&m.tx, s, spec.queue, spec.membership_sequence, &t).await?;
        let subject = Target {
            membership_sequence: spec.membership_sequence,
            member: g.before.selected(&t).ok_or(Error::HostQueueState)?.clone(),
        };
        subject
            .validate_source(&t, &g)
            .map_err(|_| Error::HostQueueState)?;
        let git = &subject
            .member
            .candidate
            .as_ref()
            .ok_or(Error::HostQueueState)?
            .commit
            .revision;
        let i=m.tx.query_opt("SELECT i.*,b.snapshot,b.revision FROM th_github_imports i JOIN th_bases b ON b.tenant=i.tenant AND b.repository=i.repository AND b.revision='native:sha256:'||encode(i.native_commit,'hex') WHERE i.tenant=$1 AND i.repository=$2 AND i.import=$3 AND i.state='completed'", &[&s.tenant(),&s.repository(),&spec.import]).await?.ok_or(Error::NotFound)?;
        if i.get::<_, i64>("epoch") != cmd.epoch
            || i.get::<_, Uuid>("binding") != q.get::<_, Uuid>("binding")
            || i.get::<_, String>("git_revision") != *git
        {
            return Err(Error::Invalid("managed check import subject"));
        }
        m.tx.execute("INSERT INTO th_check_programs(tenant,repository,digest,definition,profile,principal) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING", &[&s.tenant(),&s.repository(),&program.as_bytes().as_slice(),&definition,&profile.as_bytes().as_slice(),&auth.principal()]).await?;
        load_program(&m.tx, s, program).await?;
        m.tx.execute("INSERT INTO th_github_managed_checks(tenant,repository,check_run,epoch,queue,membership_sequence,import,revision,snapshot,subject,program,validator,principal,token) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)", &[&s.tenant(),&s.repository(),&id,&cmd.epoch,&spec.queue,&spec.membership_sequence,&spec.import,&i.get::<_,String>("revision"),&i.get::<_,Vec<u8>>("snapshot"),&json!(subject),&program.as_bytes().as_slice(),&spec.validator,&auth.principal(),&auth.token_id()]).await?;
        let c = check(&m.tx, s, id).await?;
        fresh(&m.tx, s, &c).await?;
        validator(&m.tx, s, &c, None).await?;
        let v = describe(&m.tx, s, &c).await?;
        m.complete(v, vec![]).await
    }
    pub async fn revoke_github_managed_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.revoke.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::PolicyWrite).await?;
        let s = auth.scope();
        let c = check(&m.tx, s, id).await?;
        if c.get::<_, i64>("epoch") != cmd.epoch {
            return Err(Error::Fenced);
        }
        m.tx.execute("INSERT INTO th_github_managed_check_revocations(tenant,repository,check_run,principal) VALUES($1,$2,$3,$4) ON CONFLICT DO NOTHING", &[&s.tenant(),&s.repository(),&id,&auth.principal()]).await?;
        m.complete(json!({"check_run":id,"revoked":true}), vec![])
            .await
    }
    pub async fn prepare_github_managed_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.prepare.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let c = owned(&m.tx, auth, cmd.epoch, id).await?;
        fresh(&m.tx, s, &c).await?;
        if let Some(e) = execution(&m.tx, s, id).await? {
            validator(&m.tx, s, &c, Some(&e)).await?;
            let v = describe(&m.tx, s, &c).await?;
            return m.complete(v, vec![]).await;
        }
        validator(&m.tx, s, &c, None).await?;
        let full = SourceProfile::full();
        m.tx.execute("INSERT INTO th_source_profiles(tenant,repository,digest,definition,principal) VALUES($1,$2,$3,$4,$5) ON CONFLICT DO NOTHING", &[&s.tenant(),&s.repository(),&full.id().as_bytes().as_slice(),&full.encode(),&auth.principal()]).await?;
        let workspace = Uuid::new_v4();
        let run = Uuid::new_v4();
        m.create_workspace_effect(&WorkspaceSpec {
            id: workspace,
            base: c.get::<_, String>("revision").parse()?,
            profile: *full.id().as_bytes(),
            format: WorkspaceFormat::Native,
        })
        .await?;
        m.tx.execute("INSERT INTO th_github_managed_executions(tenant,repository,check_run,token,workspace,execution_run,state) VALUES($1,$2,$3,$4,$5,$6,'preparing')", &[&s.tenant(),&s.repository(),&id,&auth.token_id(),&workspace,&run]).await?;
        let v = describe(&m.tx, s, &c).await?;
        m.complete(
            v,
            vec![Event {
                kind: "workspace.prepare".into(),
                payload: json!({"workspace":workspace,"generation":1,"epoch":cmd.epoch}),
            }],
        )
        .await
    }
    pub async fn start_github_managed_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.start.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let c = owned(&m.tx, auth, cmd.epoch, id).await?;
        fresh(&m.tx, s, &c).await?;
        let e = execution(&m.tx, s, id)
            .await?
            .ok_or(Error::ExecutionState)?;
        validator(&m.tx, s, &c, Some(&e)).await?;
        if e.get::<_, String>("state") != "preparing" {
            let v = describe(&m.tx, s, &c).await?;
            return m.complete(v, vec![]).await;
        }
        let workspace: Uuid = e.get("workspace");
        let full = SourceProfile::full();
        if m.tx.query_opt("SELECT 1 FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3 AND principal=$4 AND generation=1 AND epoch=$5 AND base_revision=$6 AND resolved_profile=$7 AND profile=$7 AND format='native'", &[&s.tenant(),&s.repository(),&workspace,&auth.principal(),&cmd.epoch,&c.get::<_,String>("revision"),&full.id().as_bytes().as_slice()]).await?.is_none(){return Err(Error::ExecutionState);}
        let (p, profile) = load_program(&m.tx, s, oid(c.get("program"))?).await?;
        let payload = m
            .request_run_effect(&RunRequest {
                run: e.get("execution_run"),
                workspace,
                generation: 1,
                profile: profile.digest(),
                argv: p.argv,
                agent: "treeharbor-github-check-v1".into(),
                task: id.to_string(),
            })
            .await?;
        m.tx.execute("UPDATE th_github_managed_executions SET state='requested' WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant(),&s.repository(),&id]).await?;
        let v = describe(&m.tx, s, &c).await?;
        m.complete(
            v,
            vec![Event {
                kind: "run.execute".into(),
                payload,
            }],
        )
        .await
    }
    pub async fn collect_github_managed_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.collect.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let c = owned(&m.tx, auth, cmd.epoch, id).await?;
        let e = execution(&m.tx, s, id)
            .await?
            .ok_or(Error::ExecutionState)?;
        validator(&m.tx, s, &c, Some(&e)).await?;
        if e.get::<_, String>("state") == "collected" {
            let v = describe(&m.tx, s, &c).await?;
            return m.complete(v, vec![]).await;
        }
        if e.get::<_, String>("state") != "requested" {
            return Err(Error::ExecutionState);
        }
        let (definition, evidence, outcome) = evidence(&m.tx, s, &c, &e).await?;
        m.tx.execute("UPDATE th_github_managed_executions SET state='collected',outcome=$4,evidence=$5,definition=$6 WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant(),&s.repository(),&id,&outcome,&evidence,&definition]).await?;
        let v = describe(&m.tx, s, &c).await?;
        m.complete(v, vec![]).await
    }
    pub async fn github_managed_check_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let c = check(&tx, s, id).await?;
        if c.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let v = describe(&tx, s, &c).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(v)
    }
    pub async fn release_github_managed_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
    ) -> Result<Value> {
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.release.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let c = owned(&m.tx, auth, cmd.epoch, id).await?;
        let e = execution(&m.tx, s, id)
            .await?
            .ok_or(Error::ExecutionState)?;
        if e.get::<_, String>("state") != "collected" {
            // A cancelled approval may have allocated files before a runtime
            // was ever requested. It must also have a bounded cleanup path.
            if e.get::<_, String>("state") != "preparing" {
                return Err(Error::ExecutionState);
            }
            match fresh(&m.tx, s, &c).await {
                Err(Error::Forbidden | Error::HostQueueState | Error::ExecutionState) => {}
                Ok(()) => return Err(Error::ExecutionState),
                Err(e) => return Err(e),
            }
        }
        let workspace: Uuid = e.get("workspace");
        let w=m.tx.query_one("SELECT state,generation FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND workspace=$3", &[&s.tenant(),&s.repository(),&workspace]).await?;
        let state: String = w.get(0);
        let (v, events) = if matches!(state.as_str(), "draining" | "deleted") {
            if w.get::<_, i64>(1) != 2 {
                return Err(Error::ExecutionState);
            }
            (
                json!({"workspace":workspace,"generation":2,"state":state}),
                vec![],
            )
        } else {
            let v = m.drain_workspace_effect(workspace, 1).await?;
            (
                v.clone(),
                vec![Event {
                    kind: "workspace.drain".into(),
                    payload: v,
                }],
            )
        };
        m.complete(v, events).await
    }
}
pub(crate) async fn execution_fence(
    tx: &Transaction<'_>,
    s: &Scope,
    run: Uuid,
) -> Result<Option<bool>> {
    let Some(e)=tx.query_opt("SELECT * FROM th_github_managed_executions WHERE tenant=$1 AND repository=$2 AND execution_run=$3", &[&s.tenant(),&s.repository(),&run]).await? else{return Ok(None)};
    let c = check(tx, s, e.get("check_run")).await?;
    let result = async {
        fresh(tx, s, &c).await?;
        validator(tx, s, &c, Some(&e)).await?;
        if e.get::<_, String>("state") != "requested" {
            return Err(Error::ExecutionState);
        }
        Ok(())
    }
    .await;
    match result {
        Ok(()) => Ok(Some(false)),
        Err(Error::Forbidden | Error::Fenced | Error::HostQueueState | Error::ExecutionState) => {
            Ok(Some(true))
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod encoding_tests {
    use super::*;
    #[test]
    fn managed_evidence_golden_versions_separate_pull_from_group() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/github-managed-evidence.json"
        ))
        .unwrap();
        let input = fixture["input"].clone();
        for (kind, pull) in [
            ("group", None),
            (
                "pull",
                Some(Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap()),
            ),
        ] {
            let (bytes, digest) = encode_evidence(input.clone(), pull).unwrap();
            assert_eq!(
                String::from_utf8(bytes).unwrap(),
                fixture[kind]["canonical"].as_str().unwrap()
            );
            assert_eq!(
                oid(digest).unwrap().to_string(),
                fixture[kind]["digest"].as_str().unwrap()
            );
        }
        let original = encode_evidence(input.clone(), None).unwrap().1;
        for field in [
            "revision",
            "snapshot",
            "program",
            "owner_token",
            "validator_token",
        ] {
            let mut changed = input.clone();
            changed[field] = json!("different");
            assert_ne!(original, encode_evidence(changed, None).unwrap().1);
        }
    }
}
