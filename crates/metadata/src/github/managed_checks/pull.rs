use super::*;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedPullCheckSpec {
    pub pull: Uuid,
    pub import: Uuid,
    pub validator: String,
    pub program: CheckProgram,
}

// Derived from a completed, assigned-bridge PR publication. This is a retained
// source identity, not a fresh remote observation or an authorization to merge.
pub(super) async fn source(
    tx: &Transaction<'_>,
    s: &Scope,
    id: Uuid,
    epoch: i64,
) -> Result<(Uuid, Value)> {
    let p = pulls::load(tx, s, id).await?;
    let b = binding(tx, s, p.get("binding")).await?;
    let projection = projections::load(tx, s, p.get("projection")).await?;
    if p.get::<_, String>("state") != "observed"
        || p.get::<_, i64>("epoch") != epoch
        || p.get::<_, bool>("cancel_requested")
        || b.get::<_, String>("state") != "active"
        || b.get::<_, i64>("epoch") != epoch
        || projection.row.get::<_, String>("state") != "ready"
        || projection.row.get::<_, i64>("epoch") != epoch
        || projection.row.get::<_, i64>("version") != projection.row.get::<_, i64>("latest_version")
        || projection.object.id().as_bytes().as_slice() != p.get::<_, Vec<u8>>("native_commit")
    {
        return Err(Error::ExecutionState);
    }
    // A completed publication does not expire with its former dispatch lease.
    // Draft PRs may be tested, but still need the host's normal admission rules.
    let report: PullReport =
        serde_json::from_value(p.get("observation")).map_err(|_| Error::SchemaMismatch)?;
    let observed = report.pull.ok_or(Error::SchemaMismatch)?;
    let target: PullTarget =
        serde_json::from_value(p.get("target")).map_err(|_| Error::SchemaMismatch)?;
    let spec = specification(&b)?;
    let git: String = p.get("git_revision");
    if observed.state != "open"
        || observed.merged
        || observed.head_repository != spec.upstream.repository_id
        || observed.base_repository != spec.upstream.repository_id
        || observed.head_ref != format!("treeharbor/{}", target.branch_id.simple())
        || observed.base_ref != target.base
        || observed.head_revision != git
        || target.git_revision != git
        || report.git_revision != git
        || report.binding != p.get::<_, Uuid>("binding").to_string()
        || !report.token_revoked
    {
        return Err(Error::ExecutionState);
    }
    Ok((
        p.get("binding"),
        json!({
            "version":1,"repository_id":spec.upstream.repository_id,
            "pull_number":observed.identity.number,"pull_node":observed.identity.node_id,
            "head_ref":observed.head_ref,"base_ref":observed.base_ref,"head_revision":git
        }),
    ))
}

pub(super) async fn fresh(tx: &Transaction<'_>, s: &Scope, c: &Row) -> Result<()> {
    let (_, subject) = source(tx, s, c.get("pull"), c.get("epoch")).await?;
    if subject != c.get::<_, Value>("subject") {
        return Err(Error::ExecutionState);
    }
    Ok(())
}

impl Metadata {
    pub async fn approve_github_managed_pull_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        id: Uuid,
        spec: &ManagedPullCheckSpec,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(spec.pull)?;
        uuid(spec.import)?;
        crate::identifier(&spec.validator, 128)?;
        let definition = spec.program.canonical()?;
        let program = spec.program.digest()?;
        let profile: ObjectId = spec.program.runtime_profile.parse()?;
        let m = match self
            .begin(request(
                auth,
                cmd,
                "github.managed_check.approve_pull.v1",
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
        let (binding, subject) = source(&m.tx, s, spec.pull, cmd.epoch).await?;
        let i=m.tx.query_opt("SELECT i.*,b.snapshot,b.revision FROM th_github_imports i JOIN th_bases b ON b.tenant=i.tenant AND b.repository=i.repository AND b.revision='native:sha256:'||encode(i.native_commit,'hex') WHERE i.tenant=$1 AND i.repository=$2 AND i.import=$3 AND i.state='completed'", &[&s.tenant(),&s.repository(),&spec.import]).await?.ok_or(Error::NotFound)?;
        if i.get::<_, i64>("epoch") != cmd.epoch
            || i.get::<_, Uuid>("binding") != binding
            || Some(i.get::<_, String>("git_revision").as_str())
                != subject["head_revision"].as_str()
        {
            return Err(Error::Invalid("managed pull check import subject"));
        }
        m.tx.execute("INSERT INTO th_check_programs(tenant,repository,digest,definition,profile,principal) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING", &[&s.tenant(),&s.repository(),&program.as_bytes().as_slice(),&definition,&profile.as_bytes().as_slice(),&auth.principal()]).await?;
        load_program(&m.tx, s, program).await?;
        m.tx.execute("INSERT INTO th_github_managed_checks(tenant,repository,check_run,epoch,pull,import,revision,snapshot,subject,program,validator,principal,token) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)", &[&s.tenant(),&s.repository(),&id,&cmd.epoch,&spec.pull,&spec.import,&i.get::<_,String>("revision"),&i.get::<_,Vec<u8>>("snapshot"),&subject,&program.as_bytes().as_slice(),&spec.validator,&auth.principal(),&auth.token_id()]).await?;
        let c = check(&m.tx, s, id).await?;
        admission_policy::pin_check(&m.tx, s, &c).await?;
        super::fresh(&m.tx, s, &c).await?;
        validator(&m.tx, s, &c, None).await?;
        let v = describe(&m.tx, s, &c).await?;
        m.complete(v, vec![]).await
    }
}
