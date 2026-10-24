//! One-shot Connected queue delivery with immutable observation history.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_contract::{
    InspectionReport, InspectionTarget,
    queue::{QueueAsyncOutcome, QueueAsyncResult, QueueObservation, QueueTarget},
};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueueSpec {
    pub inspection: String,
    pub ttl_seconds: i32,
}
pub(super) async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(extract(epoch from expires_at)*1000)::bigint AS deadline,(extract(epoch from started_at)*1000)::bigint AS started,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_queue WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
pub(super) fn target(r: &Row) -> Result<QueueTarget> {
    serde_json::from_value(r.get("target")).map_err(|_| Error::SchemaMismatch)
}
async fn current(tx: &Transaction<'_>, s: &Scope, r: &Row) -> Result<bool> {
    let p = projections::load(tx, s, r.get("projection")).await?;
    Ok(p.row.get::<_, String>("state") == "ready"
        && p.row.get::<_, i64>("epoch") == r.get::<_, i64>("epoch")
        && p.row.get::<_, i64>("version") == p.row.get::<_, i64>("latest_version")
        && p.object.id().as_bytes().as_slice() == r.get::<_, Vec<u8>>("native_commit"))
}
pub(super) async fn retained_merge(
    tx: &Transaction<'_>,
    s: &Scope,
    id: Uuid,
    t: &QueueTarget,
) -> Result<Option<Value>> {
    Ok(tx.query_opt("SELECT report#>'{snapshot,final_commit}' AS commit FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report#>>'{snapshot,pull,merged}'='true' AND report#>>'{snapshot,pull,head_revision}'=$4 ORDER BY sequence DESC LIMIT 1",&[&s.tenant(),&s.repository(),&id,&t.pull.head_revision]).await?.map(|r|r.get("commit")))
}
pub(super) async fn retained_result(
    tx: &Transaction<'_>,
    s: &Scope,
    id: Uuid,
) -> Result<Option<QueueAsyncResult>> {
    tx.query_opt("SELECT report->'async_result' AS result FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report#>>'{async_result,request_uuid}' IS NOT NULL ORDER BY (report#>>'{async_result,outcome,status}' IN ('failed','enqueued','merged')) DESC,(report->>'finished_at_unix_ms')::bigint DESC,sequence DESC LIMIT 1", &[&s.tenant(),&s.repository(),&id]).await?
        .map(|r| serde_json::from_value(r.get("result")).map_err(|_| Error::SchemaMismatch)).transpose()
}
async fn retained_enqueue(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Option<Value>> {
    Ok(tx.query_opt("SELECT report->'enqueue_receipt' FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report->'enqueue_receipt' IS NOT NULL AND report->'enqueue_receipt' <> 'null'::jsonb ORDER BY sequence LIMIT 1",&[&s.tenant(),&s.repository(),&id]).await?.map(|r|r.get(0)))
}
async fn describe(tx: &Transaction<'_>, s: &Scope, r: &Row, dynamic: bool) -> Result<Value> {
    let t = target(r)?;
    let observation: Option<QueueObservation> = r
        .get::<_, Option<Value>>("latest_observation")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::SchemaMismatch)?;
    let attempt = r.get::<_, Option<Uuid>>("attempt");
    let cancelled = r.get::<_, bool>("cancel_requested");
    let retained = retained_merge(tx, s, r.get("queue"), &t).await?;
    let provider_result = retained_result(tx, s, r.get("queue")).await?;
    let enqueue_receipt = retained_enqueue(tx, s, r.get("queue")).await?;
    let failed = r.get::<_, bool>("dispatch_failed");
    let not_sent = r.get::<_, bool>("mutation_not_sent");
    let state = if retained.is_some()
        || provider_result
            .as_ref()
            .is_some_and(|r| matches!(r.outcome, QueueAsyncOutcome::Merged { .. }))
    {
        "host_merged_unverified"
    } else if attempt.is_none() {
        if cancelled { "cancelled" } else { "requested" }
    } else if (failed || not_sent)
        && observation
            .as_ref()
            .and_then(|o| o.snapshot.as_ref())
            .is_none_or(|s| s.entry.is_none())
    {
        if not_sent {
            "not_sent"
        } else {
            "request_failed"
        }
    } else {
        observation
            .as_ref()
            .map(|o| o.state(&t))
            .unwrap_or("uncertain")
    };
    let mut v = json!({"version":1,"queue":r.get::<_,Uuid>("queue").to_string(),"inspection":r.get::<_,Uuid>("inspection").to_string(),"pull":r.get::<_,Uuid>("pull").to_string(),
        "projection":r.get::<_,Uuid>("projection").to_string(),"binding":r.get::<_,Uuid>("binding").to_string(),"epoch":r.get::<_,i64>("epoch"),
        "target":t,"state":state,"cancel_requested":cancelled,"attempt":attempt.map(|u|u.to_string()),"expires_at_unix_ms":r.get::<_,i64>("deadline"),
        "provider_request":r.get::<_,Option<Uuid>>("provider_request").map(|u|u.to_string()),"report_sequence":r.get::<_,i64>("report_sequence"),
        "observation":observation,"retained_merge":retained,"provider_result":provider_result,
        "dispatch_failed":failed,"mutation_not_sent":not_sent,"merge_verified":false,"host_policy_verified":false});
    if dynamic {
        v["is_current_version"] = json!(current(tx, s, r).await?);
        v["unexpired"] = json!(r.get::<_, bool>("unexpired"));
    }
    if let Some(receipt) = enqueue_receipt {
        v["retained_enqueue"] = receipt;
    }
    Ok(v)
}
pub(super) async fn assigned(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    r: &Row,
) -> Result<BindingSpec> {
    context(tx, auth, epoch, Permission::HostMerge).await?;
    if r.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let spec = specification(&binding(tx, auth.scope(), r.get("binding")).await?)?;
    if spec.bridge != auth.principal() || r.get::<_, String>("bridge") != auth.principal() {
        return Err(Error::Forbidden);
    }
    Ok(spec)
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
    if r.get::<_, bool>("cancel_requested")
        || r.get::<_, bool>("mutation_not_sent")
        || !r.get::<_, bool>("unexpired")
        || !current(tx, auth.scope(), &r).await?
    {
        return Err(Error::HostQueueState);
    }
    projections::active_binding(tx, auth.scope(), r.get("binding"), epoch).await?;
    if permission == Permission::HostMerge {
        assigned(tx, auth, epoch, &r).await?;
    }
    Ok(r)
}
impl Metadata {
    pub async fn request_github_queue(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &QueueSpec,
    ) -> Result<Value> {
        uuid(id)?;
        let inspection: Uuid = spec
            .inspection
            .parse()
            .map_err(|_| Error::Invalid("inspection UUID"))?;
        uuid(inspection)?;
        if inspection.to_string() != spec.inspection || !(1..=900).contains(&spec.ttl_seconds) {
            return Err(Error::Invalid("queue specification"));
        }
        let m = match self
            .begin(request(
                auth,
                c,
                "github.queue.request.v1",
                json!([1, id.to_string(), spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::HostQueue).await?;
        let s = auth.scope();
        let i=m.tx.query_opt("SELECT * FROM th_github_inspections WHERE tenant=$1 AND repository=$2 AND inspection=$3",&[&s.tenant(),&s.repository(),&inspection]).await?.ok_or(Error::NotFound)?;
        if i.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        if i.get::<_, String>("state") != "observed" {
            return Err(Error::HostInspectionState);
        }
        let report: InspectionReport =
            serde_json::from_value(i.get("report")).map_err(|_| Error::SchemaMismatch)?;
        let pull: InspectionTarget =
            serde_json::from_value(i.get("target")).map_err(|_| Error::SchemaMismatch)?;
        let t = QueueTarget {
            pull,
            expected_base_revision: report.before.base_revision.clone(),
            expected_rules: report.rules_before.clone(),
        };
        if !t.admits_capture(&report) {
            return Err(Error::HostQueueState);
        }
        let binding_id: Uuid = i.get("binding");
        let binding = projections::active_binding(&m.tx, s, binding_id, c.epoch).await?;
        let old=m.tx.query_opt("SELECT q.queue FROM th_github_queue q WHERE q.tenant=$1 AND q.repository=$2 AND q.binding=$3 AND q.target#>>'{pull,pull_number}'=$4 AND NOT q.dispatch_failed AND NOT q.mutation_not_sent AND (NOT q.cancel_requested OR q.attempt IS NOT NULL) AND NOT (q.cancel_requested AND EXISTS(SELECT 1 FROM th_github_dequeues d WHERE d.tenant=q.tenant AND d.repository=q.repository AND d.queue=q.queue AND d.attempt IS NOT NULL AND (d.retained_receipt IS NOT NULL OR d.mutation_not_sent) AND d.latest_observation#>>'{after,pull,merged}'='false' AND d.latest_observation#>'{after,entry}'='null'::jsonb)) LIMIT 1",&[&s.tenant(),&s.repository(),&binding_id,&t.pull.pull_number.to_string()]).await?;
        // Cancellation is not proof that an already dispatched remote operation stopped.
        if old.is_some() {
            return Err(Error::HostQueueState);
        }
        if dequeues::unsettled_for_pull(&m.tx, s, binding_id, t.pull.pull_number).await? {
            return Err(Error::HostQueueState);
        }
        let pin = format!("github-queue:{id}");
        m.copy_root_pin(&i.get::<_, String>("pin_holder"), &pin)
            .await?;
        m.tx.execute("INSERT INTO th_github_queue(tenant,repository,queue,inspection,pull,projection,binding,epoch,principal,bridge,native_commit,target,expires_at,pin_holder) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,clock_timestamp()+($13::integer*interval '1 second'),$14)",&[&s.tenant(),&s.repository(),&id,&inspection,&i.get::<_,Uuid>("pull"),&i.get::<_,Uuid>("projection"),&binding_id,&c.epoch,&auth.principal(),&binding.bridge,&i.get::<_,Vec<u8>>("native_commit"),&json!(t),&spec.ttl_seconds,&pin]).await?;
        let r = fresh(&m.tx, auth, c.epoch, id, Permission::HostQueue).await?;
        let v = describe(&m.tx, s, &r, false).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.queue.requested".into(),
                payload: json!({"queue":id.to_string()}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, Permission::HostQueue).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    pub async fn github_queue_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        bridge: bool,
        attempt: Option<Uuid>,
    ) -> Result<Value> {
        uuid(id)?;
        if attempt.is_some() && !bridge {
            return Err(Error::Forbidden);
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let r = load(&tx, auth.scope(), id).await?;
        if r.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let mut v = describe(&tx, auth.scope(), &r, true).await?;
        if bridge {
            v["specification"] = json!(assigned(&tx, auth, epoch, &r).await?);
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        if let Some(attempt) = attempt {
            uuid(attempt)?;
            if r.get::<_, Option<Uuid>>("attempt") != Some(attempt) {
                return Err(Error::HostQueueState);
            }
            fresh(&tx, auth, epoch, id, Permission::HostMerge).await?;
        }
        if bridge {
            assigned(&tx, auth, epoch, &r).await?;
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        tx.commit().await?;
        Ok(v)
    }
    pub async fn begin_github_queue(
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
                "github.queue.begin.v1",
                json!([1, id.to_string(), attempt.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(json!({"receipt":v,"dispatch_granted":false})),
            Begin::Fresh(m) => m,
        };
        let r = fresh(&m.tx, auth, c.epoch, id, Permission::HostMerge).await?;
        if r.get::<_, Option<Uuid>>("attempt").is_some() {
            return Err(Error::HostQueueState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_queue SET attempt=$4,started_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&id,&attempt]).await?;
        let v = describe(&m.tx, s, &load(&m.tx, s, id).await?, false).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.queue.dispatched".into(),
                payload: json!({"queue":id.to_string()}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, Permission::HostMerge).await?;
        m.tx.commit().await?;
        Ok(json!({"receipt":v,"dispatch_granted":true}))
    }
    pub async fn report_github_queue(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        attempt: Uuid,
        report: &QueueObservation,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.queue.report.v1",
                json!([1, id.to_string(), attempt.to_string(), report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let r = load(&m.tx, s, id).await?;
        assigned(&m.tx, auth, c.epoch, &r).await?;
        if r.get::<_, Option<Uuid>>("attempt") != Some(attempt) {
            return Err(Error::HostQueueState);
        }
        let t = target(&r)?;
        report
            .validate(&t)
            .map_err(|_| Error::Invalid("queue observation"))?;
        let last = r
            .get::<_, Option<Value>>("latest_observation")
            .and_then(|v| v["finished_at_unix_ms"].as_i64())
            .unwrap_or(
                r.get::<_, Option<i64>>("started")
                    .ok_or(Error::SchemaMismatch)?
                    .saturating_sub(15000),
            );
        if report.finished_at_unix_ms > r.get::<_, i64>("now_ms").saturating_add(15000)
            || report.started_at_unix_ms
                < r.get::<_, Option<i64>>("started")
                    .ok_or(Error::SchemaMismatch)?
                    .saturating_sub(15000)
        {
            return Err(Error::HostQueueState);
        }
        let mut provider = r.get::<_, Option<Uuid>>("provider_request");
        let previous_enqueue = retained_enqueue(&m.tx, s, id).await?;
        if let Some(receipt) = &report.enqueue_receipt {
            receipt
                .validate(&t, attempt)
                .map_err(|_| Error::HostQueueState)?;
            if provider.is_some() || previous_enqueue.as_ref().is_some_and(|old|*old != json!(receipt))
                || m.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND (report->'receipt' <> 'null'::jsonb OR report->'async_result' <> 'null'::jsonb))",&[&s.tenant(),&s.repository(),&id]).await?.get::<_,bool>(0)
            { return Err(Error::HostQueueState); }
        }
        if previous_enqueue.is_some() && (report.receipt.is_some() || report.async_result.is_some())
        {
            return Err(Error::HostQueueState);
        }
        let not_sent = r.get::<_, bool>("mutation_not_sent") || report.mutation_not_sent;
        if not_sent && (report.receipt.is_some() || report.async_result.is_some() || report.enqueue_receipt.is_some() || previous_enqueue.is_some()
            || m.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND (report->'receipt' <> 'null'::jsonb OR report->'async_result' <> 'null'::jsonb))",&[&s.tenant(),&s.repository(),&id]).await?.get::<_,bool>(0)) {
            return Err(Error::HostQueueState);
        }
        if let Some(result) = &report.async_result {
            if provider.map(|p| p.to_string()).as_deref() != Some(&result.request_uuid) {
                return Err(Error::HostQueueState);
            }
            if let Some(previous) = retained_result(&m.tx, s, id).await?
                && previous.terminal()
                && result.outcome != QueueAsyncOutcome::Unavailable
                && previous != *result
            {
                return Err(Error::HostQueueState);
            }
        }
        if let Some(raw) = report
            .receipt
            .as_ref()
            .and_then(|r| r.request_uuid.as_ref())
        {
            let id: Uuid = raw
                .parse()
                .map_err(|_| Error::Invalid("provider request"))?;
            if provider.is_some_and(|old| old != id) {
                return Err(Error::HostQueueState);
            }
            provider = Some(id);
        }
        if let Some(new) = report
            .snapshot
            .as_ref()
            .filter(|s| s.pull.merged && s.pull.head_revision == t.pull.head_revision)
            .and_then(|s| s.final_commit.as_ref())
            && retained_merge(&m.tx, s, id, &t)
                .await?
                .is_some_and(|old| old != json!(new))
        {
            return Err(Error::HostQueueState);
        }
        let failed = r.get::<_, bool>("dispatch_failed")
            || report
                .receipt
                .as_ref()
                .is_some_and(|r| r.status == "failed")
            || report
                .async_result
                .as_ref()
                .is_some_and(|r| r.outcome == QueueAsyncOutcome::Failed);
        let seq = r.get::<_, i64>("report_sequence") + 1;
        if seq > 4096 {
            return Err(Error::UploadLimit);
        }
        m.tx.execute("INSERT INTO th_github_queue_reports(tenant,repository,queue,sequence,report) VALUES($1,$2,$3,$4,$5)",&[&s.tenant(),&s.repository(),&id,&seq,&json!(report)]).await?;
        // A late journal receipt can contain the only provider UUID. Retain it
        // without replacing a more recent live observation.
        let latest = if report.finished_at_unix_ms >= last {
            json!(report)
        } else {
            r.get::<_, Option<Value>>("latest_observation")
                .ok_or(Error::SchemaMismatch)?
        };
        m.tx.execute("UPDATE th_github_queue SET latest_observation=$4,report_sequence=$5,provider_request=$6,dispatch_failed=$7,mutation_not_sent=$8 WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&id,&latest,&seq,&provider,&failed,&not_sent]).await?;
        let mut v = describe(&m.tx, s, &load(&m.tx, s, id).await?, false).await?;
        v["recorded_observation"] = json!(report);
        m.write_completion(
            &v,
            &[Event {
                kind: "github.queue.observed".into(),
                payload: json!({"queue":id.to_string(),"sequence":seq}),
            }],
        )
        .await?;
        assigned(&m.tx, auth, c.epoch, &r).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    pub async fn cancel_github_queue(
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
                "github.queue.cancel.v1",
                json!([1, id.to_string()]),
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
        m.tx.execute("UPDATE th_github_queue SET cancel_requested=true WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&id]).await?;
        let v = describe(&m.tx, s, &load(&m.tx, s, id).await?, false).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.queue.cancel_requested".into(),
                payload: json!({"queue":id.to_string()}),
            }],
        )
        .await?;
        context(&m.tx, auth, c.epoch, Permission::HostQueue).await?;
        m.tx.commit().await?;
        Ok(v)
    }
}
