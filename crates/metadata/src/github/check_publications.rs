//! Trusted execution-derived intent, one-shot dispatch and immutable host receipts.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_core::ObjectId;
use treeharbor_github_contract::check_publication::{Outcome, Preflight, Report, Target};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckPublicationSpec {
    pub check_run: Uuid,
    pub ttl_seconds: i32,
}
async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(extract(epoch from expires_at)*1000)::bigint AS deadline,(extract(epoch from started_at)*1000)::bigint AS started,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_check_publications WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
fn target(r: &Row) -> Result<Target> {
    let t: Target = serde_json::from_value(r.get("target")).map_err(|_| Error::SchemaMismatch)?;
    t.validate().map_err(|_| Error::SchemaMismatch)?;
    if t.publication != r.get::<_, Uuid>("publication")
        || t.epoch != r.get::<_, i64>("epoch")
        || t.source.head().map_err(|_| Error::SchemaMismatch)? != r.get::<_, String>("head")
        || t.program.parse::<ObjectId>()?.as_bytes().as_slice() != r.get::<_, Vec<u8>>("program")
    {
        return Err(Error::SchemaMismatch);
    }
    let context = if t.admission.is_some() {
        t.name().map_err(|_| Error::SchemaMismatch)?
    } else {
        String::new()
    };
    if r.get::<_, String>("context_name") != context
        || t.admission
            .as_ref()
            .is_some_and(|a| a.binding != r.get::<_, Uuid>("binding"))
    {
        return Err(Error::SchemaMismatch);
    }
    Ok(t)
}
async fn assigned(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    r: &Row,
) -> Result<BindingSpec> {
    context(tx, auth, epoch, Permission::HostCheck).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let spec = specification(&binding(tx, auth.scope(), r.get("binding")).await?)?;
    if spec.bridge != auth.principal() || r.get::<_, String>("bridge") != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok(spec)
}
async fn conditions(tx: &Transaction<'_>, s: &Scope, r: &Row) -> Result<()> {
    if !r.get::<_, bool>("unexpired") || r.get::<_, bool>("cancel_requested") {
        return Err(Error::HostPublicationState);
    }
    if !crate::integration::trusted(
        tx,
        s,
        &r.get::<_, String>("principal"),
        "human",
        "host.queue",
        Some(r.get("token")),
    )
    .await?
    {
        return Err(Error::Forbidden);
    }
    projections::active_binding(tx, s, r.get("binding"), r.get("epoch")).await?;
    let (b, t) =
        managed_checks::publication::derive(tx, s, r.get("check_run"), r.get("publication"))
            .await?;
    if b != r.get::<_, Uuid>("binding") || t != target(r)? {
        return Err(Error::ExecutionState);
    }
    if let Some(value) = r.get::<_, Option<Value>>("preflight") {
        let preflight: Preflight =
            serde_json::from_value(value).map_err(|_| Error::SchemaMismatch)?;
        admission_policy::publication_preflight(tx, s, r.get("check_run"), &preflight).await?;
    }
    Ok(())
}
async fn fresh(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    id: Uuid,
    permission: Permission,
) -> Result<Row> {
    context(tx, auth, epoch, permission).await?;
    let r = load(tx, auth.scope(), id).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    if permission == Permission::HostCheck {
        assigned(tx, auth, epoch, &r).await?;
        if r.get::<_, Option<Uuid>>("attempt_token")
            .is_some_and(|t| t != auth.token_id())
        {
            return Err(Error::Forbidden);
        }
    }
    conditions(tx, auth.scope(), &r).await?;
    Ok(r)
}
fn describe(r: &Row) -> Result<Value> {
    Ok(
        json!({"version":1,"publication":r.get::<_,Uuid>("publication"),"check_run":r.get::<_,Uuid>("check_run"),"binding":r.get::<_,Uuid>("binding"),"epoch":r.get::<_,i64>("epoch"),"target":target(r)?,"state":r.get::<_,String>("state"),"cancel_requested":r.get::<_,bool>("cancel_requested"),"unexpired":r.get::<_,bool>("unexpired"),"expires_at_unix_ms":r.get::<_,i64>("deadline"),"attempt":r.get::<_,Option<Uuid>>("attempt"),"preflight":r.get::<_,Option<Value>>("preflight"),"report_sequence":r.get::<_,i64>("report_sequence"),"observation":r.get::<_,Option<Value>>("latest_report"),"merge_verified":false,"host_policy_verified":false,"admission_verified":false}),
    )
}
async fn finish_fresh(
    m: crate::Mutation<'_>,
    auth: &Authorization,
    c: &Command,
    id: Uuid,
    permission: Permission,
    kind: &str,
) -> Result<Value> {
    let value = describe(&load(&m.tx, auth.scope(), id).await?)?;
    m.write_completion(
        &value,
        &[Event {
            kind: kind.into(),
            payload: json!({"publication":id}),
        }],
    )
    .await?;
    fresh(&m.tx, auth, c.epoch, id, permission).await?;
    m.tx.commit().await?;
    Ok(value)
}

impl Metadata {
    pub async fn request_github_check_publication(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &CheckPublicationSpec,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(spec.check_run)?;
        if !(1..=900).contains(&spec.ttl_seconds) {
            return Err(Error::Invalid("check publication deadline"));
        }
        let m = match self
            .begin(request(
                auth,
                c,
                "github.check_publication.request.v1",
                json!([1, id, spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::HostQueue).await?;
        let s = auth.scope();
        let (binding_id, t) =
            managed_checks::publication::derive(&m.tx, s, spec.check_run, id).await?;
        if t.epoch != c.epoch {
            return Err(Error::Fenced);
        }
        let b = specification(&binding(&m.tx, s, binding_id).await?)?;
        let head = t.source.head().map_err(|_| Error::SchemaMismatch)?;
        let program: ObjectId = t.program.parse()?;
        let context = if t.admission.is_some() {
            t.name().map_err(|_| Error::SchemaMismatch)?
        } else {
            String::new()
        };
        if m.tx.query_opt("SELECT 1 FROM th_github_check_publications WHERE tenant=$1 AND repository=$2 AND (publication=$3 OR (binding=$4 AND epoch=$5 AND head=$6 AND program=$7 AND context_name=$8 AND state IN ('requested','attempting','uncertain')))", &[&s.tenant(),&s.repository(),&id,&binding_id,&c.epoch,&head,&program.as_bytes().as_slice(),&context]).await?.is_some() {
            return Err(Error::HostPublicationState);
        }
        m.tx.execute("INSERT INTO th_github_check_publications(tenant,repository,publication,check_run,binding,epoch,principal,token,bridge,target,head,program,context_name,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$14,'requested',clock_timestamp()+make_interval(secs=>$13))", &[&s.tenant(),&s.repository(),&id,&spec.check_run,&binding_id,&c.epoch,&auth.principal(),&auth.token_id(),&b.bridge,&json!(t),&head,&program.as_bytes().as_slice(),&(spec.ttl_seconds as f64),&context]).await?;
        finish_fresh(
            m,
            auth,
            c,
            id,
            Permission::HostQueue,
            "github.check_publication.requested",
        )
        .await
    }
    /// Historical context never grants another dispatch. Recovery remains possible
    /// after cancellation, expiry or binding revocation within the same active epoch.
    pub async fn github_check_publication_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        bridge: bool,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let permission = if bridge {
            Permission::HostCheck
        } else {
            Permission::Read
        };
        context(&tx, auth, epoch, permission).await?;
        let r = load(&tx, auth.scope(), id).await?;
        if r.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let mut v = describe(&r)?;
        if bridge {
            v["specification"] = json!(assigned(&tx, auth, epoch, &r).await?);
        }
        v["current_locally"] = json!(match conditions(&tx, auth.scope(), &r).await {
            Ok(()) => true,
            Err(
                Error::Forbidden
                | Error::Fenced
                | Error::HostBindingState
                | Error::HostPublicationState
                | Error::ExecutionState
                | Error::HostQueueState,
            ) => false,
            Err(e) => return Err(e),
        });
        context(&tx, auth, epoch, permission).await?;
        tx.commit().await?;
        Ok(v)
    }
    pub async fn begin_github_check_publication(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        attempt: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.check_publication.begin.v1",
                json!([1, id, attempt]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(json!({"receipt":v,"dispatch_granted":false})),
            Begin::Fresh(m) => m,
        };
        let r = fresh(&m.tx, auth, c.epoch, id, Permission::HostCheck).await?;
        if r.get::<_, String>("state") != "requested" {
            return Err(Error::HostPublicationState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_check_publications SET state='attempting',attempt=$4,attempt_token=$5,started_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&id,&attempt,&auth.token_id()]).await?;
        let receipt = finish_fresh(
            m,
            auth,
            c,
            id,
            Permission::HostCheck,
            "github.check_publication.attempting",
        )
        .await?;
        Ok(json!({"receipt":receipt,"dispatch_granted":true}))
    }
    /// Authorize the irreversible boundary exactly once after the actual host
    /// reads. Replaying a lost reply never returns another permission to POST.
    pub async fn dispatch_github_check_publication(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        attempt: Uuid,
        preflight: &Preflight,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.check_publication.dispatch.v1",
                json!([1, id, attempt, preflight]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(json!({"receipt":v,"dispatch_granted":false})),
            Begin::Fresh(m) => m,
        };
        let r = fresh(&m.tx, auth, c.epoch, id, Permission::HostCheck).await?;
        if r.get::<_, String>("state") != "attempting"
            || r.get::<_, Option<Uuid>>("attempt") != Some(attempt)
            || r.get::<_, Option<Value>>("preflight").is_some()
            || !preflight.eligible(&target(&r)?)
        {
            return Err(Error::HostPublicationState);
        }
        let s = auth.scope();
        admission_policy::publication_preflight(&m.tx, s, r.get("check_run"), preflight).await?;
        m.tx.execute("UPDATE th_github_check_publications SET preflight=$4,dispatched_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&id,&json!(preflight)]).await?;
        let receipt = finish_fresh(
            m,
            auth,
            c,
            id,
            Permission::HostCheck,
            "github.check_publication.dispatched",
        )
        .await?;
        Ok(json!({"receipt":receipt,"dispatch_granted":true}))
    }
    pub async fn report_github_check_publication(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        attempt: Uuid,
        report: &Report,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.check_publication.report.v1",
                json!([1, id, attempt, report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let r = load(&m.tx, s, id).await?;
        assigned(&m.tx, auth, c.epoch, &r).await?;
        let t = target(&r)?;
        report
            .validate(&t)
            .map_err(|_| Error::Invalid("check publication report"))?;
        let state: String = r.get("state");
        let sequence: i64 = r.get("report_sequence");
        let retained: Option<Preflight> = r
            .get::<_, Option<Value>>("preflight")
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::SchemaMismatch)?;
        if !matches!(state.as_str(), "attempting" | "uncertain")
            || r.get::<_, Option<Uuid>>("attempt") != Some(attempt)
            || report.preflight != retained
            || sequence >= 1024
            || (state == "uncertain" && report.outcome == Outcome::NotSent)
            || (report.outcome == Outcome::Observed
                && retained.as_ref().is_none_or(|p| !p.eligible(&t)))
        {
            return Err(Error::HostPublicationState);
        }
        let started: i64 = r
            .get::<_, Option<i64>>("started")
            .ok_or(Error::SchemaMismatch)?;
        let previous = r
            .get::<_, Option<Value>>("latest_report")
            .and_then(|v| v["finished_at_unix_ms"].as_i64())
            .unwrap_or(started.saturating_sub(15000));
        if report.started_at_unix_ms < started.saturating_sub(15000)
            || report.finished_at_unix_ms < previous
            || report.finished_at_unix_ms > r.get::<_, i64>("now_ms").saturating_add(15000)
        {
            return Err(Error::HostPublicationState);
        }
        let next = match report.outcome {
            Outcome::Observed => "observed",
            Outcome::Unknown => "uncertain",
            Outcome::NotSent => "not_sent",
        };
        let sequence = sequence + 1;
        m.tx.execute("INSERT INTO th_github_check_publication_reports(tenant,repository,publication,sequence,attempt,reporter,token,report) VALUES($1,$2,$3,$4,$5,$6,$7,$8)", &[&s.tenant(),&s.repository(),&id,&sequence,&attempt,&auth.principal(),&auth.token_id(),&json!(report)]).await?;
        m.tx.execute("UPDATE th_github_check_publications SET state=$4,report_sequence=$5,latest_report=$6 WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&id,&next,&sequence,&json!(report)]).await?;
        // A bounded acknowledgment avoids duplicating a large preflight/target.
        let v = json!({"version":1,"publication":id,"attempt":attempt,"sequence":sequence,"recorded_observation":report});
        m.write_completion(
            &v,
            &[Event {
                kind: "github.check_publication.reported".into(),
                payload: json!({"publication":id,"sequence":sequence}),
            }],
        )
        .await?;
        assigned(&m.tx, auth, c.epoch, &r).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    pub async fn cancel_github_check_publication(
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
                "github.check_publication.cancel.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::HostQueue).await?;
        let s = auth.scope();
        let r = load(&m.tx, s, id).await?;
        if r.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        if matches!(
            r.get::<_, String>("state").as_str(),
            "requested" | "attempting" | "uncertain"
        ) {
            m.tx.execute("UPDATE th_github_check_publications SET cancel_requested=true,state=CASE WHEN attempt IS NULL THEN 'cancelled' ELSE state END WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&id]).await?;
        }
        let v = describe(&load(&m.tx, s, id).await?)?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.check_publication.cancel_requested".into(),
                payload: json!({"publication":id}),
            }],
        )
        .await?;
        context(&m.tx, auth, c.epoch, Permission::HostQueue).await?;
        m.tx.commit().await?;
        Ok(v)
    }
}
