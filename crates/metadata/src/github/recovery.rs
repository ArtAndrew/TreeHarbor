//! Owner-bounded scans and one-shot redelivery; receipt comes only from the inbox.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_contract::deliveries::{
    DeliveryAttempt, DeliveryDetails, DeliveryPageReport, payload_digest,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RecoverySpec {
    pub channel: String,
    pub callback_url: String,
    pub since_unix_ms: i64,
    pub ttl_seconds: i32,
    pub max_pages: i32,
    pub max_deliveries: i32,
}
impl RecoverySpec {
    fn validate(&self) -> Result<Uuid> {
        let id = wire::id(&self.channel)?;
        treeharbor_github_contract::deliveries::callback_url(&self.callback_url)
            .map_err(|_| Error::Invalid("recovery callback"))?;
        if self.since_unix_ms <= 0
            || !(1..=3600).contains(&self.ttl_seconds)
            || !(1..=100).contains(&self.max_pages)
            || !(1..=100).contains(&self.max_deliveries)
        {
            return Err(Error::Invalid("recovery budget"));
        }
        Ok(id)
    }
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedeliveryOutcome {
    Accepted,
    Unknown,
    NotSent,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RedeliveryReport {
    pub version: u8,
    pub observed_at_unix_ms: i64,
    pub outcome: RedeliveryOutcome,
}
fn provider(id: i64) -> Result<()> {
    if id <= 0 {
        Err(Error::Invalid("provider delivery id"))
    } else {
        Ok(())
    }
}
async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(extract(epoch from created_at)*1000)::bigint AS created_ms,(extract(epoch from expires_at)*1000)::bigint AS deadline,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_event_recoveries WHERE tenant=$1 AND repository=$2 AND recovery=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
async fn attempt(tx: &Transaction<'_>, s: &Scope, id: Uuid, p: i64) -> Result<Row> {
    tx.query_opt("SELECT *,(extract(epoch from started_at)*1000)::bigint AS started_ms FROM th_github_event_recovery_attempts WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND provider_id=$4",&[&s.tenant(),&s.repository(),&id,&p]).await?.ok_or(Error::NotFound)
}
fn observed(r: &Row) -> Result<DeliveryAttempt> {
    serde_json::from_value(r.get("observation")).map_err(|_| Error::SchemaMismatch)
}
fn details(r: &Row) -> Result<Option<DeliveryDetails>> {
    r.get::<_, Option<Value>>("details")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::SchemaMismatch)
}
async fn active_channel(tx: &Transaction<'_>, s: &Scope, id: Uuid, epoch: i64) -> Result<Row> {
    let ch=tx.query_opt("SELECT * FROM th_github_event_channels WHERE tenant=$1 AND repository=$2 AND channel=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)?;
    if ch.get::<_, String>("state") != "active" || ch.get::<_, i64>("epoch") != epoch {
        return Err(Error::HostEventState);
    }
    Ok(ch)
}
async fn fresh(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    id: Uuid,
    live: bool,
) -> Result<(Row, BindingSpec)> {
    context(tx, auth, epoch, Permission::HostRecover).await?;
    let r = load(tx, auth.scope(), id).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let ch = active_channel(tx, auth.scope(), r.get("channel"), epoch).await?;
    if ch.get::<_, Uuid>("binding") != r.get::<_, Uuid>("binding") {
        return Err(Error::HostEventState);
    }
    let (_, spec) =
        assigned_for(tx, auth, epoch, r.get("binding"), Permission::HostRecover).await?;
    if r.get::<_, String>("bridge") != auth.principal() {
        return Err(Error::Forbidden);
    }
    if live && (r.get::<_, bool>("cancelled") || !r.get::<_, bool>("unexpired")) {
        return Err(Error::HostEventState);
    }
    Ok((r, spec))
}
fn description(r: &Row) -> Value {
    json!({"version":1,"recovery":r.get::<_,Uuid>("recovery"),"channel":r.get::<_,Uuid>("channel"),
        "binding":r.get::<_,Uuid>("binding"),"epoch":r.get::<_,i64>("epoch"),"callback_url":r.get::<_,String>("callback_url"),
        "since_unix_ms":r.get::<_,i64>("since_unix_ms"),"until_unix_ms":r.get::<_,i64>("created_ms"),
        "expires_at_unix_ms":r.get::<_,i64>("deadline"),"unexpired":r.get::<_,bool>("unexpired"),
        "cancelled":r.get::<_,bool>("cancelled"),"max_pages":r.get::<_,i32>("max_pages"),
        "max_deliveries":r.get::<_,i32>("max_deliveries"),"page_count":r.get::<_,i32>("page_count"),
        "scanned_count":r.get::<_,i32>("scanned_count"),"detail_count":r.get::<_,i32>("detail_count"),
        "dispatch_count":r.get::<_,i32>("dispatch_count"),"next_cursor":r.get::<_,Option<String>>("next_cursor"),
        "scan_exhausted":r.get::<_,bool>("scan_exhausted"),
        "retention_gap":r.get::<_,i64>("since_unix_ms")<r.get::<_,i64>("now_ms").saturating_sub(259200000),
        "coverage_complete":false,"merge_verified":false})
}
fn time_valid(time: i64, r: &Row) -> Result<()> {
    if time < r.get::<_, i64>("created_ms").saturating_sub(15000)
        || time > r.get::<_, i64>("now_ms").saturating_add(15000)
    {
        return Err(Error::HostEventState);
    }
    Ok(())
}
async fn inbox(tx: &Transaction<'_>, s: &Scope, r: &Row, a: &Row) -> Result<Value> {
    let row=tx.query_opt("SELECT e.event,e.id,e.body,encode(e.digest,'hex') AS raw_digest FROM th_github_event_deliveries d JOIN th_github_events e ON (e.tenant,e.repository,e.channel,e.digest)=(d.tenant,d.repository,d.channel,d.digest) WHERE d.tenant=$1 AND d.repository=$2 AND d.channel=$3 AND d.delivery=$4",&[&s.tenant(),&s.repository(),&r.get::<_,Uuid>("channel"),&a.get::<_,Uuid>("delivery")]).await?;
    let Some(row) = row else {
        return Ok(json!({"state":"missing"}));
    };
    let Some(d) = details(a)? else {
        return Ok(json!({"state":"needs_content_check"}));
    };
    let body: Value = serde_json::from_slice(&row.get::<_, Vec<u8>>("body"))
        .map_err(|_| Error::SchemaMismatch)?;
    let Ok(digest) = payload_digest(&body) else {
        return Ok(json!({"state":"comparison_unavailable"}));
    };
    if digest != d.payload_sha256 {
        return Ok(json!({"state":"payload_conflict"}));
    }
    Ok(
        json!({"state":"received","event":row.get::<_,Uuid>("event"),"sequence":row.get::<_,i64>("id"),
        "body_sha256":row.get::<_,String>("raw_digest"),"payload_sha256":digest}),
    )
}
async fn delivery_description(tx: &Transaction<'_>, s: &Scope, r: &Row, a: &Row) -> Result<Value> {
    let related=tx.query_opt("SELECT dispatch FROM th_github_event_recovery_attempts WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND delivery=$4 AND provider_id<>$5 AND dispatch IS NOT NULL LIMIT 1",&[&s.tenant(),&s.repository(),&r.get::<_,Uuid>("recovery"),&a.get::<_,Uuid>("delivery"),&a.get::<_,i64>("provider_id")]).await?.map(|r|r.get::<_,Uuid>(0));
    Ok(
        json!({"provider_id":a.get::<_,i64>("provider_id"),"observation":a.get::<_,Value>("observation"),
        "details":a.get::<_,Option<Value>>("details"),"dispatch":a.get::<_,Option<Uuid>>("dispatch"),
        "report":a.get::<_,Option<Value>>("report"),"related_dispatch":related,"inbox":inbox(tx,s,r,a).await?}),
    )
}
impl Metadata {
    pub async fn request_github_recovery(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &RecoverySpec,
    ) -> Result<Value> {
        uuid(id)?;
        let channel = spec.validate()?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.recovery.request.v1",
                json!([1, id, spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let ch = active_channel(&m.tx, s, channel, c.epoch).await?;
        let binding_id: Uuid = ch.get("binding");
        let b = projections::active_binding(&m.tx, s, binding_id, c.epoch).await?;
        let now: i64 =
            m.tx.query_one(
                "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
                &[],
            )
            .await?
            .get(0);
        if spec.since_unix_ms > now {
            return Err(Error::Invalid("recovery window starts in future"));
        }
        let count:i64=m.tx.query_one("SELECT count(*) FROM th_github_event_recoveries WHERE tenant=$1 AND repository=$2 AND NOT cancelled AND expires_at>clock_timestamp()",&[&s.tenant(),&s.repository()]).await?.get(0);
        if count >= 8 {
            return Err(Error::HostEventLimit);
        }
        let inserted=m.tx.execute("INSERT INTO th_github_event_recoveries(tenant,repository,recovery,channel,binding,epoch,principal,bridge,callback_url,since_unix_ms,max_pages,max_deliveries,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,clock_timestamp()+($13::integer*interval '1 second')) ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&id,&channel,&binding_id,&c.epoch,&auth.principal(),&b.bridge,&spec.callback_url,&spec.since_unix_ms,&spec.max_pages,&spec.max_deliveries,&spec.ttl_seconds]).await?;
        if inserted != 1 {
            return Err(Error::HostEventState);
        }
        let result = description(&load(&m.tx, s, id).await?);
        m.complete(
            result,
            vec![Event {
                kind: "github.recovery.requested".into(),
                payload: json!({"recovery":id}),
            }],
        )
        .await
    }
    pub async fn cancel_github_recovery(
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
                "github.recovery.cancel.v1",
                json!([1, id]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        if load(&m.tx, s, id).await?.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        m.tx.execute("UPDATE th_github_event_recoveries SET cancelled=true WHERE tenant=$1 AND repository=$2 AND recovery=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        let result = description(&load(&m.tx, s, id).await?);
        m.complete(
            result,
            vec![Event {
                kind: "github.recovery.cancelled".into(),
                payload: json!({"recovery":id}),
            }],
        )
        .await
    }
    pub async fn github_recovery_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
        bridge: bool,
    ) -> Result<Value> {
        uuid(id)?;
        if after < 0 {
            return Err(Error::Invalid("recovery cursor"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let s = auth.scope();
        let r = load(&tx, s, id).await?;
        if r.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let mut v = description(&r);
        if bridge {
            v["specification"] = json!(fresh(&tx, auth, epoch, id, false).await?.1);
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        let rows=tx.query("SELECT * FROM th_github_event_recovery_attempts WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND provider_id>$4 ORDER BY provider_id LIMIT 9",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut list = Vec::new();
        for a in rows.iter().take(8) {
            list.push(delivery_description(&tx, s, &r, a).await?);
        }
        v["deliveries"] = json!(list);
        v["next_after"] = if rows.len() > 8 {
            json!(rows[7].get::<_, i64>("provider_id"))
        } else {
            Value::Null
        };
        if bridge {
            fresh(&tx, auth, epoch, id, false).await?;
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        tx.commit().await?;
        Ok(v)
    }
    pub async fn record_github_recovery_page(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        page: &DeliveryPageReport,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.recovery.page.v1",
                json!([1, id, page]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (r, b) = fresh(&m.tx, auth, c.epoch, id, true).await?;
        let s = auth.scope();
        page.validate(b.upstream.installation_id, b.upstream.repository_id)
            .map_err(|_| Error::Invalid("recovery page"))?;
        time_valid(page.observed_at_unix_ms, &r)?;
        if r.get::<_, bool>("scan_exhausted")
            || r.get::<_, i32>("page_count") >= r.get::<_, i32>("max_pages")
            || page.cursor != r.get::<_, Option<String>>("next_cursor")
        {
            return Err(Error::HostEventState);
        }
        if let Some(next) = &page.next_cursor {
            let cycle:bool=m.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_github_event_recovery_pages WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND report->>'cursor'=$4)",&[&s.tenant(),&s.repository(),&id,next]).await?.get(0);
            if cycle {
                return Err(Error::HostEventState);
            }
        }
        let sequence = r.get::<_, i32>("page_count") + 1;
        for a in &page.attempts {
            if a.delivered_at_unix_ms < r.get::<_, i64>("since_unix_ms")
                || a.delivered_at_unix_ms > r.get::<_, i64>("created_ms")
            {
                continue;
            }
            let p = a.id as i64;
            let old=m.tx.query_opt("SELECT observation FROM th_github_event_recovery_attempts WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND provider_id=$4",&[&s.tenant(),&s.repository(),&id,&p]).await?;
            if let Some(old) = old {
                if !observed(&old)?.same_attempt(a) {
                    return Err(Error::HostEventState);
                }
            } else {
                m.tx.execute("INSERT INTO th_github_event_recovery_attempts(tenant,repository,recovery,provider_id,delivery,observation) VALUES($1,$2,$3,$4,$5,$6)",&[&s.tenant(),&s.repository(),&id,&p,&a.guid,&json!(a)]).await?;
            }
        }
        m.tx.execute("INSERT INTO th_github_event_recovery_pages(tenant,repository,recovery,sequence,report) VALUES($1,$2,$3,$4,$5)",&[&s.tenant(),&s.repository(),&id,&sequence,&json!(page)]).await?;
        m.tx.execute("UPDATE th_github_event_recoveries SET page_count=$4,scanned_count=scanned_count+$5,next_cursor=$6,scan_exhausted=$7 WHERE tenant=$1 AND repository=$2 AND recovery=$3",&[&s.tenant(),&s.repository(),&id,&sequence,&i32::from(page.scanned),&page.next_cursor,&page.next_cursor.is_none()]).await?;
        let v = json!({"recovery":id,"sequence":sequence,"recorded_page":page});
        m.write_completion(
            &v,
            &[Event {
                kind: "github.recovery.page".into(),
                payload: json!({"recovery":id,"sequence":sequence}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, true).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    pub async fn record_github_recovery_details(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        d: &DeliveryDetails,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.recovery.detail.v1",
                json!([1, id, d]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (r, b) = fresh(&m.tx, auth, c.epoch, id, true).await?;
        let s = auth.scope();
        d.validate(b.upstream.installation_id, b.upstream.repository_id)
            .map_err(|_| Error::Invalid("recovery detail"))?;
        time_valid(d.observed_at_unix_ms, &r)?;
        let p = d.attempt.id as i64;
        let a = attempt(&m.tx, s, id, p).await?;
        if !observed(&a)?.same_attempt(&d.attempt) {
            return Err(Error::HostEventState);
        }
        if let Some(old) = details(&a)? {
            if old != *d {
                return Err(Error::HostEventState);
            }
        } else {
            if r.get::<_, i32>("detail_count") >= r.get::<_, i32>("max_deliveries") {
                return Err(Error::HostEventLimit);
            }
            m.tx.execute("UPDATE th_github_event_recovery_attempts SET details=$5 WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND provider_id=$4",&[&s.tenant(),&s.repository(),&id,&p,&json!(d)]).await?;
            m.tx.execute("UPDATE th_github_event_recoveries SET detail_count=detail_count+1 WHERE tenant=$1 AND repository=$2 AND recovery=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        }
        let v = delivery_description(&m.tx, s, &r, &attempt(&m.tx, s, id, p).await?).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.recovery.detail".into(),
                payload: json!({"recovery":id,"provider_id":p}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, true).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    pub async fn begin_github_redelivery(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        p: i64,
        dispatch: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(dispatch)?;
        provider(p)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.recovery.begin.v1",
                json!([1, id, p, dispatch]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(json!({"receipt":v,"dispatch_granted":false})),
            Begin::Fresh(m) => m,
        };
        let (r, _) = fresh(&m.tx, auth, c.epoch, id, true).await?;
        let s = auth.scope();
        let a = attempt(&m.tx, s, id, p).await?;
        if details(&a)?.is_none()
            || a.get::<_, Option<Uuid>>("dispatch").is_some()
            || inbox(&m.tx, s, &r, &a).await?["state"] != "missing"
        {
            return Err(Error::HostEventState);
        }
        let duplicate:bool=m.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_github_event_recovery_attempts WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND delivery=$4 AND dispatch IS NOT NULL)",&[&s.tenant(),&s.repository(),&id,&a.get::<_,Uuid>("delivery")]).await?.get(0);
        if duplicate {
            return Err(Error::HostEventState);
        }
        m.tx.execute("UPDATE th_github_event_recovery_attempts SET dispatch=$5,started_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND provider_id=$4",&[&s.tenant(),&s.repository(),&id,&p,&dispatch]).await?;
        m.tx.execute("UPDATE th_github_event_recoveries SET dispatch_count=dispatch_count+1 WHERE tenant=$1 AND repository=$2 AND recovery=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        let v = json!({"recovery":id,"provider_id":p,"dispatch":dispatch,"details":details(&a)?});
        m.write_completion(
            &v,
            &[Event {
                kind: "github.recovery.dispatched".into(),
                payload: json!({"recovery":id,"provider_id":p,"dispatch":dispatch}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, true).await?;
        m.tx.commit().await?;
        Ok(json!({"receipt":v,"dispatch_granted":true}))
    }
    pub async fn github_redelivery_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        p: i64,
        dispatch: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(dispatch)?;
        provider(p)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let (r, _) = fresh(&tx, auth, epoch, id, true).await?;
        let a = attempt(&tx, auth.scope(), id, p).await?;
        if a.get::<_, Option<Uuid>>("dispatch") != Some(dispatch)
            || a.get::<_, Option<Value>>("report").is_some()
            || inbox(&tx, auth.scope(), &r, &a).await?["state"] != "missing"
        {
            return Err(Error::HostEventState);
        }
        let v = json!({"recovery":id,"provider_id":p,"dispatch":dispatch,"details":details(&a)?});
        fresh(&tx, auth, epoch, id, true).await?;
        tx.commit().await?;
        Ok(v)
    }
    pub async fn report_github_redelivery(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        p: i64,
        dispatch: Uuid,
        report: &RedeliveryReport,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(dispatch)?;
        provider(p)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.recovery.report.v1",
                json!([1, id, p, dispatch, report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (r, _) = fresh(&m.tx, auth, c.epoch, id, false).await?;
        let s = auth.scope();
        let a = attempt(&m.tx, s, id, p).await?;
        if a.get::<_, Option<Uuid>>("dispatch") != Some(dispatch) || report.version != 1 {
            return Err(Error::HostEventState);
        }
        time_valid(report.observed_at_unix_ms, &r)?;
        if report.observed_at_unix_ms
            < a.get::<_, Option<i64>>("started_ms")
                .ok_or(Error::SchemaMismatch)?
                .saturating_sub(15000)
        {
            return Err(Error::HostEventState);
        }
        if let Some(old) = a.get::<_, Option<Value>>("report") {
            if old != json!(report) {
                return Err(Error::HostEventState);
            }
        } else {
            m.tx.execute("UPDATE th_github_event_recovery_attempts SET report=$5 WHERE tenant=$1 AND repository=$2 AND recovery=$3 AND provider_id=$4",&[&s.tenant(),&s.repository(),&id,&p,&json!(report)]).await?;
        }
        let v = delivery_description(&m.tx, s, &r, &attempt(&m.tx, s, id, p).await?).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.recovery.reported".into(),
                payload: json!({"recovery":id,"provider_id":p}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, false).await?;
        m.tx.commit().await?;
        Ok(v)
    }
}
