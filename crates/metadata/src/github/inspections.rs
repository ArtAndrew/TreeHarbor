use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_contract::{InspectionReport, InspectionTarget, RequiredCheck};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectionSpec {
    pub pull: String,
    pub required: Vec<RequiredCheck>,
    pub ttl_seconds: i32,
}
async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(extract(epoch from created_at)*1000)::bigint AS created,(extract(epoch from expires_at)*1000)::bigint AS deadline,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_inspections WHERE tenant=$1 AND repository=$2 AND inspection=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
fn target(r: &Row) -> Result<InspectionTarget> {
    serde_json::from_value(r.get("target")).map_err(|_| Error::SchemaMismatch)
}
// Idempotent mutation replies are historical receipts. Do not freeze a dynamic
// current-version/deadline assertion inside a receipt that may replay much later.
fn receipt(mut value: Value) -> Value {
    let fields = value
        .as_object_mut()
        .expect("inspection description is an object");
    fields.remove("is_current_version");
    fields.remove("unexpired");
    value
}
async fn current(tx: &Transaction<'_>, s: &Scope, r: &Row) -> Result<bool> {
    let p = projections::load(tx, s, r.get("projection")).await?;
    Ok(p.row.get::<_, String>("state") == "ready"
        && p.row.get::<_, i64>("epoch") == r.get::<_, i64>("epoch")
        && p.row.get::<_, i64>("version") == p.row.get::<_, i64>("latest_version")
        && p.object.id().as_bytes().to_vec() == r.get::<_, Vec<u8>>("native_commit"))
}
async fn describe(tx: &Transaction<'_>, s: &Scope, r: &Row) -> Result<Value> {
    let t = target(r)?;
    let report: Option<InspectionReport> = r
        .get::<_, Option<Value>>("report")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::SchemaMismatch)?;
    let is_current = current(tx, s, r).await?;
    Ok(
        json!({"version":1,"inspection":r.get::<_,Uuid>("inspection").to_string(),"pull":r.get::<_,Uuid>("pull").to_string(),"projection":r.get::<_,Uuid>("projection").to_string(),"binding":r.get::<_,Uuid>("binding").to_string(),"epoch":r.get::<_,i64>("epoch"),"target":t,
        "state":r.get::<_,String>("state"),"is_current_version":is_current,"unexpired":r.get::<_,bool>("unexpired"),"expires_at_unix_ms":r.get::<_,i64>("deadline"),
        "capture_consistent":report.as_ref().is_some_and(|v|v.consistent(&t)),"selected_checks_passed_at_capture":report.as_ref().is_some_and(|v|v.selected_checks_passed(&t)),
        "report":report,"legacy_branch_protection_included":false,"host_policy_verified":false,"merge_authorized":false}),
    )
}
async fn assigned(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    r: &Row,
) -> Result<BindingSpec> {
    context(tx, auth, epoch, Permission::HostRead).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let spec = specification(&binding(tx, auth.scope(), r.get("binding")).await?)?;
    if r.get::<_, String>("bridge") != auth.principal() || spec.bridge != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok(spec)
}
async fn fresh(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    r: &Row,
    permission: Permission,
) -> Result<()> {
    context(tx, auth, epoch, permission).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    if r.get::<_, String>("state") != "requested"
        || !r.get::<_, bool>("unexpired")
        || !current(tx, auth.scope(), r).await?
    {
        return Err(Error::HostInspectionState);
    }
    projections::active_binding(tx, auth.scope(), r.get("binding"), epoch).await?;
    if permission == Permission::HostRead {
        assigned(tx, auth, epoch, r).await?;
    }
    Ok(())
}
impl Metadata {
    pub async fn request_github_inspection(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &InspectionSpec,
    ) -> Result<Value> {
        uuid(id)?;
        let pull: Uuid = spec.pull.parse().map_err(|_| Error::Invalid("pull UUID"))?;
        uuid(pull)?;
        if pull.to_string() != spec.pull || !(1..=900).contains(&spec.ttl_seconds) {
            return Err(Error::Invalid("inspection request"));
        }
        let m = match self
            .begin(request(
                auth,
                c,
                "github.inspection.request.v1",
                json!([1, id.to_string(), spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let p = pulls::load(&m.tx, s, pull).await?;
        if p.get::<_, String>("state") != "observed" || p.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::HostPullState);
        }
        let binding_id: Uuid = p.get("binding");
        let binding = projections::active_binding(&m.tx, s, binding_id, c.epoch).await?;
        let report: PullReport =
            serde_json::from_value(p.get("observation")).map_err(|_| Error::SchemaMismatch)?;
        let observed = report.pull.ok_or(Error::SchemaMismatch)?;
        let t = InspectionTarget {
            repository_id: binding.upstream.repository_id,
            pull_number: observed.identity.number,
            pull_node: observed.identity.node_id,
            head_ref: observed.head_ref,
            base_ref: observed.base_ref,
            head_revision: p.get("git_revision"),
            required: spec.required.clone(),
        };
        t.validate()
            .map_err(|_| Error::Invalid("inspection target"))?;
        let count:i64=m.tx.query_one("SELECT count(*) FROM th_github_inspections WHERE tenant=$1 AND repository=$2 AND state='requested' AND expires_at>clock_timestamp()",&[&s.tenant(),&s.repository()]).await?.get(0);
        if count >= 4 {
            return Err(Error::UploadLimit);
        }
        let pin = format!("github-inspection:{id}");
        m.copy_root_pin(&p.get::<_, String>("pin_holder"), &pin)
            .await?;
        m.tx.execute("INSERT INTO th_github_inspections(tenant,repository,inspection,pull,projection,binding,epoch,principal,bridge,native_commit,target,state,expires_at,pin_holder) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,'requested',clock_timestamp()+($12::integer*interval '1 second'),$13)",&[&s.tenant(),&s.repository(),&id,&pull,&p.get::<_,Uuid>("projection"),&binding_id,&c.epoch,&auth.principal(),&binding.bridge,&p.get::<_,Vec<u8>>("native_commit"),&json!(t),&spec.ttl_seconds,&pin]).await?;
        let r = load(&m.tx, s, id).await?;
        let result = receipt(describe(&m.tx, s, &r).await?);
        m.write_completion(
            &result,
            &[Event {
                kind: "github.inspection.requested".into(),
                payload: json!({"inspection":id.to_string()}),
            }],
        )
        .await?;
        fresh(
            &m.tx,
            auth,
            c.epoch,
            &load(&m.tx, s, id).await?,
            Permission::RepositoryWrite,
        )
        .await?;
        m.tx.commit().await?;
        Ok(result)
    }
    pub async fn github_inspection_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        bridge: bool,
        collect: bool,
    ) -> Result<Value> {
        uuid(id)?;
        if collect && !bridge {
            return Err(Error::Forbidden);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let r = load(&tx, auth.scope(), id).await?;
        if r.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let mut v = describe(&tx, auth.scope(), &r).await?;
        if bridge {
            v["specification"] = json!(assigned(&tx, auth, epoch, &r).await?);
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        if collect {
            fresh(&tx, auth, epoch, &r, Permission::HostRead).await?;
        }
        if bridge {
            assigned(&tx, auth, epoch, &r).await?;
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        tx.commit().await?;
        Ok(v)
    }
    pub async fn report_github_inspection(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        report: &InspectionReport,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.inspection.report.v1",
                json!([1, id.to_string(), report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let r = load(&m.tx, auth.scope(), id).await?;
        assigned(&m.tx, auth, c.epoch, &r).await?;
        if r.get::<_, String>("state") != "requested" {
            return Err(Error::HostInspectionState);
        }
        report
            .validate(&target(&r)?)
            .map_err(|_| Error::Invalid("inspection report"))?;
        if report.started_at_unix_ms < r.get::<_, i64>("created").saturating_sub(15000)
            || report.finished_at_unix_ms > r.get::<_, i64>("deadline")
            || report.finished_at_unix_ms > r.get::<_, i64>("now_ms").saturating_add(15000)
        {
            return Err(Error::HostInspectionState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_inspections SET state='observed',report=$4 WHERE tenant=$1 AND repository=$2 AND inspection=$3",&[&s.tenant(),&s.repository(),&id,&json!(report)]).await?;
        let result = receipt(describe(&m.tx, s, &load(&m.tx, s, id).await?).await?);
        m.write_completion(
            &result,
            &[Event {
                kind: "github.inspection.observed".into(),
                payload: json!({"inspection":id.to_string()}),
            }],
        )
        .await?;
        assigned(&m.tx, auth, c.epoch, &r).await?;
        m.tx.commit().await?;
        Ok(result)
    }
    pub async fn cancel_github_inspection(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.inspection.cancel.v1",
                json!([1, id.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let r = load(&m.tx, s, id).await?;
        if r.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        m.tx.execute("UPDATE th_github_inspections SET state='cancelled' WHERE tenant=$1 AND repository=$2 AND inspection=$3 AND state='requested'",&[&s.tenant(),&s.repository(),&id]).await?;
        let result = receipt(describe(&m.tx, s, &load(&m.tx, s, id).await?).await?);
        m.write_completion(
            &result,
            &[Event {
                kind: "github.inspection.cancelled".into(),
                payload: json!({"inspection":id.to_string()}),
            }],
        )
        .await?;
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        m.tx.commit().await?;
        Ok(result)
    }
}
