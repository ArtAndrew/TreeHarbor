//! Explicit owner authorization for PR-scoped host removal. Local cancellation
//! records never acquire this authority implicitly during migration.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_contract::dequeue::DequeueObservation;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DequeueSpec {
    pub queue: String,
    pub ttl_seconds: i32,
    pub scope: String,
}
async fn load(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt("SELECT *,expires_at>clock_timestamp() AS unexpired,(extract(epoch from expires_at)*1000)::bigint AS deadline,(extract(epoch from started_at)*1000)::bigint AS started,(extract(epoch from clock_timestamp())*1000)::bigint AS now_ms FROM th_github_dequeues WHERE tenant=$1 AND repository=$2 AND dequeue=$3",&[&s.tenant(),&s.repository(),&id]).await?.ok_or(Error::NotFound)
}
async fn settled(tx: &Transaction<'_>, s: &Scope, q: &Row) -> Result<bool> {
    if !q.get::<_, bool>("cancel_requested") {
        return Err(Error::SchemaMismatch);
    }
    if q.get::<_, Option<Uuid>>("attempt").is_none()
        || q.get::<_, bool>("dispatch_failed")
        || q.get::<_, bool>("mutation_not_sent")
    {
        return Ok(true);
    }
    if queue::retained_result(tx, s, q.get("queue"))
        .await?
        .is_some_and(|r| r.terminal())
    {
        return Ok(true);
    }
    Ok(tx.query_one("SELECT EXISTS(SELECT 1 FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND (report#>>'{receipt,status}' IN ('failed','enqueued','merged') OR report#>>'{enqueue_receipt,request_id}' IS NOT NULL))",&[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue")]).await?.get(0))
}
pub(super) async fn unsettled_for_pull(
    tx: &Transaction<'_>,
    s: &Scope,
    binding: Uuid,
    pull: u64,
) -> Result<bool> {
    Ok(tx.query_one("SELECT EXISTS(SELECT 1 FROM th_github_dequeues d JOIN th_github_queue q ON q.tenant=d.tenant AND q.repository=d.repository AND q.queue=d.queue WHERE q.tenant=$1 AND q.repository=$2 AND q.binding=$3 AND q.target#>>'{pull,pull_number}'=$4 AND ((d.attempt IS NULL AND d.expires_at>clock_timestamp()) OR (d.attempt IS NOT NULL AND d.retained_receipt IS NULL AND NOT d.mutation_not_sent)))",&[&s.tenant(),&s.repository(),&binding,&pull.to_string()]).await?.get(0))
}
async fn merged(tx: &Transaction<'_>, s: &Scope, r: &Row, q: &Row) -> Result<Option<Value>> {
    if let Some(fact)=tx.query_opt("SELECT jsonb_build_object('head_revision',report#>'{after,pull,head_revision}','commit',report#>'{after,final_commit}') AS fact FROM th_github_dequeue_reports WHERE tenant=$1 AND repository=$2 AND dequeue=$3 AND report#>>'{after,pull,merged}'='true' ORDER BY sequence DESC LIMIT 1",&[&s.tenant(),&s.repository(),&r.get::<_,Uuid>("dequeue")]).await? {
        return Ok(Some(fact.get("fact")));
    }
    if let Some(fact)=tx.query_opt("SELECT jsonb_build_object('head_revision',report#>'{snapshot,pull,head_revision}','commit',report#>'{snapshot,final_commit}') AS fact FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report#>>'{snapshot,pull,merged}'='true' ORDER BY sequence DESC LIMIT 1",&[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue")]).await? {
        return Ok(Some(fact.get("fact")));
    }
    let t = queue::target(q)?;
    if let Some(result) = queue::retained_result(tx, s, q.get("queue")).await?
        && let treeharbor_github_contract::queue::QueueAsyncOutcome::Merged { final_revision } =
            result.outcome
    {
        return Ok(Some(
            json!({"head_revision":t.pull.head_revision,"commit":{"revision":final_revision,"tree":null}}),
        ));
    }
    Ok(tx.query_opt("SELECT report#>>'{receipt,final_revision}' AS revision FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report#>>'{receipt,status}'='merged' ORDER BY sequence DESC LIMIT 1",&[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue")]).await?.map(|r| json!({"head_revision":t.pull.head_revision,"commit":{"revision":r.get::<_,String>("revision"),"tree":null}})))
}
fn conflicting_merge(
    old: &Value,
    snapshot: &treeharbor_github_contract::queue::QueueSnapshot,
) -> bool {
    let incoming =
        json!({"head_revision":snapshot.pull.head_revision,"commit":snapshot.final_commit});
    old["head_revision"] != incoming["head_revision"]
        || old["commit"]["revision"] != incoming["commit"]["revision"]
        || (!old["commit"]["tree"].is_null() && old["commit"]["tree"] != incoming["commit"]["tree"])
}
async fn describe(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    q: &Row,
    dynamic: bool,
) -> Result<Value> {
    let id = r.get::<_, Uuid>("dequeue");
    let t = queue::target(q)?;
    let done = settled(tx, s, q).await?;
    let retained = merged(tx, s, r, q).await?;
    let o: Option<DequeueObservation> = r
        .get::<_, Option<Value>>("latest_observation")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::SchemaMismatch)?;
    let state = if let Some(m) = &retained {
        if m["head_revision"] == t.pull.head_revision {
            "host_merged_unverified"
        } else {
            "other_head_merged"
        }
    } else if let Some(o) = &o {
        o.state(&t, id, done)
    } else if !done {
        "enqueue_unresolved"
    } else if r.get::<_, Option<Uuid>>("attempt").is_some() {
        "uncertain"
    } else {
        "requested"
    };
    let mut v = json!({"version":1,"dequeue":id.to_string(),"queue":q.get::<_,Uuid>("queue").to_string(),
        "binding":q.get::<_,Uuid>("binding").to_string(),"epoch":r.get::<_,i64>("epoch"),"scope":"pull_request",
        "target":t,"expires_at_unix_ms":r.get::<_,i64>("deadline"),"attempt":r.get::<_,Option<Uuid>>("attempt").map(|u|u.to_string()),
        "state":state,"enqueue_settled":done,"report_sequence":r.get::<_,i64>("report_sequence"),"observation":o,
        "retained_receipt":r.get::<_,Option<Value>>("retained_receipt"),"mutation_not_sent":r.get::<_,bool>("mutation_not_sent"),"retained_merge":retained,"merge_verified":false});
    if dynamic {
        v["unexpired"] = json!(r.get::<_, bool>("unexpired"));
    }
    Ok(v)
}
async fn fresh(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    id: Uuid,
    permission: Permission,
    require_settled: bool,
) -> Result<(Row, Row)> {
    context(tx, auth, epoch, permission).await?;
    let r = load(tx, auth.scope(), id).await?;
    let q = queue::load(tx, auth.scope(), r.get("queue")).await?;
    if r.get::<_, i64>("epoch") != epoch || q.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    if !r.get::<_, bool>("unexpired")
        || (require_settled && !settled(tx, auth.scope(), &q).await?)
        || r.get::<_, Option<Value>>("retained_receipt").is_some()
        || r.get::<_, bool>("mutation_not_sent")
    {
        return Err(Error::HostQueueState);
    }
    projections::active_binding(tx, auth.scope(), q.get("binding"), epoch).await?;
    if permission == Permission::HostMerge {
        queue::assigned(tx, auth, epoch, &q).await?;
    }
    Ok((r, q))
}
impl Metadata {
    pub async fn request_github_dequeue(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &DequeueSpec,
    ) -> Result<Value> {
        uuid(id)?;
        let qid: Uuid = spec
            .queue
            .parse()
            .map_err(|_| Error::Invalid("queue UUID"))?;
        uuid(qid)?;
        if qid.to_string() != spec.queue
            || spec.scope != "pull_request"
            || !(1..=900).contains(&spec.ttl_seconds)
        {
            return Err(Error::Invalid("dequeue specification"));
        }
        let m = match self
            .begin(request(
                auth,
                c,
                "github.dequeue.request.v1",
                json!([1, id.to_string(), spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::HostQueue).await?;
        let s = auth.scope();
        let q = queue::load(&m.tx, s, qid).await?;
        if q.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        projections::active_binding(&m.tx, s, q.get("binding"), c.epoch).await?;
        // An uncertain prior dequeue must not later remove a newly queued entry.
        // A retained correlated response proves that particular mutation returned.
        let t = queue::target(&q)?;
        if unsettled_for_pull(&m.tx, s, q.get("binding"), t.pull.pull_number).await? {
            return Err(Error::HostQueueState);
        }
        // Use the same settled-removal boundary as re-enqueue: a completed
        // predecessor must not block removing its successor. Unknown writes and
        // absence without a correlated receipt or positive no-send remain fenced.
        if m.tx
            .query_one(
                "SELECT EXISTS(
            SELECT 1 FROM th_github_queue q
            WHERE q.tenant=$1 AND q.repository=$2 AND q.binding=$3
              AND q.target#>>'{pull,pull_number}'=$4 AND q.queue<>$5
              AND NOT q.dispatch_failed AND NOT q.mutation_not_sent
              AND (NOT q.cancel_requested OR q.attempt IS NOT NULL)
              AND NOT (q.cancel_requested AND EXISTS(
                SELECT 1 FROM th_github_dequeues d
                WHERE d.tenant=q.tenant AND d.repository=q.repository AND d.queue=q.queue
                  AND d.attempt IS NOT NULL
                  AND (d.retained_receipt IS NOT NULL OR d.mutation_not_sent)
                  AND d.latest_observation#>>'{after,pull,merged}'='false'
                  AND d.latest_observation#>'{after,entry}'='null'::jsonb)))",
                &[
                    &s.tenant(),
                    &s.repository(),
                    &q.get::<_, Uuid>("binding"),
                    &t.pull.pull_number.to_string(),
                    &qid,
                ],
            )
            .await?
            .get::<_, bool>(0)
        {
            return Err(Error::HostQueueState);
        }
        m.tx.execute("UPDATE th_github_queue SET cancel_requested=true WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&qid]).await?;
        m.tx.execute("INSERT INTO th_github_dequeues(tenant,repository,dequeue,queue,epoch,principal,expires_at) VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()+($7::integer*interval '1 second'))",&[&s.tenant(),&s.repository(),&id,&qid,&c.epoch,&auth.principal(),&spec.ttl_seconds]).await?;
        let (r, q) = fresh(&m.tx, auth, c.epoch, id, Permission::HostQueue, false).await?;
        let v = describe(&m.tx, s, &r, &q, false).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.dequeue.requested".into(),
                payload: json!({"dequeue":id.to_string(),"queue":qid.to_string()}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, Permission::HostQueue, false).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    pub async fn github_dequeue_context(
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
        let q = queue::load(&tx, auth.scope(), r.get("queue")).await?;
        if r.get::<_, i64>("epoch") != epoch || q.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let mut v = describe(&tx, auth.scope(), &r, &q, true).await?;
        if bridge {
            v["specification"] = json!(queue::assigned(&tx, auth, epoch, &q).await?);
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        if let Some(a) = attempt {
            uuid(a)?;
            if r.get::<_, Option<Uuid>>("attempt") != Some(a) {
                return Err(Error::HostQueueState);
            }
            fresh(&tx, auth, epoch, id, Permission::HostMerge, true).await?;
        }
        if bridge {
            queue::assigned(&tx, auth, epoch, &q).await?;
        } else {
            context(&tx, auth, epoch, Permission::Read).await?;
        }
        tx.commit().await?;
        Ok(v)
    }
    pub async fn begin_github_dequeue(
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
                "github.dequeue.begin.v1",
                json!([1, id.to_string(), attempt.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(json!({"receipt":v,"dispatch_granted":false})),
            Begin::Fresh(m) => m,
        };
        let (r, q) = fresh(&m.tx, auth, c.epoch, id, Permission::HostMerge, true).await?;
        if r.get::<_, Option<Uuid>>("attempt").is_some() {
            return Err(Error::HostQueueState);
        }
        let s = auth.scope();
        m.tx.execute("UPDATE th_github_dequeues SET attempt=$4,started_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND dequeue=$3",&[&s.tenant(),&s.repository(),&id,&attempt]).await?;
        let v = describe(&m.tx, s, &load(&m.tx, s, id).await?, &q, false).await?;
        m.write_completion(
            &v,
            &[Event {
                kind: "github.dequeue.dispatched".into(),
                payload: json!({"dequeue":id.to_string()}),
            }],
        )
        .await?;
        fresh(&m.tx, auth, c.epoch, id, Permission::HostMerge, true).await?;
        m.tx.commit().await?;
        Ok(json!({"receipt":v,"dispatch_granted":true}))
    }
    pub async fn report_github_dequeue(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        attempt: Uuid,
        report: &DequeueObservation,
    ) -> Result<Value> {
        uuid(id)?;
        uuid(attempt)?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.dequeue.report.v1",
                json!([1, id.to_string(), attempt.to_string(), report]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let s = auth.scope();
        let r = load(&m.tx, s, id).await?;
        let q = queue::load(&m.tx, s, r.get("queue")).await?;
        queue::assigned(&m.tx, auth, c.epoch, &q).await?;
        if r.get::<_, i64>("epoch") != c.epoch {
            return Err(Error::Fenced);
        }
        if r.get::<_, Option<Uuid>>("attempt") != Some(attempt) {
            return Err(Error::HostQueueState);
        }
        let t = queue::target(&q)?;
        report
            .validate(&t, id)
            .map_err(|_| Error::Invalid("dequeue observation"))?;
        if report.started_at_unix_ms
            < r.get::<_, Option<i64>>("started")
                .ok_or(Error::SchemaMismatch)?
                .saturating_sub(15000)
            || report.finished_at_unix_ms > r.get::<_, i64>("now_ms").saturating_add(15000)
        {
            return Err(Error::HostQueueState);
        }
        let mut retained = r.get::<_, Option<Value>>("retained_receipt");
        let not_sent = r.get::<_, bool>("mutation_not_sent") || report.mutation_not_sent;
        if let Some(receipt) = &report.receipt {
            if retained.as_ref().is_some_and(|old| *old != json!(receipt)) {
                return Err(Error::HostQueueState);
            }
            retained = Some(json!(receipt));
        }
        if not_sent && retained.is_some() {
            return Err(Error::HostQueueState);
        }
        if let Some(snapshot) = report.after.as_ref().filter(|s| s.pull.merged)
            && merged(&m.tx, s, &r, &q)
                .await?
                .is_some_and(|old| conflicting_merge(&old, snapshot))
        {
            return Err(Error::HostQueueState);
        }
        let seq = r.get::<_, i64>("report_sequence") + 1;
        if seq > 4096 {
            return Err(Error::UploadLimit);
        }
        let old = r.get::<_, Option<Value>>("latest_observation");
        let latest = if old
            .as_ref()
            .and_then(|v| v["finished_at_unix_ms"].as_i64())
            .is_some_and(|v| v > report.finished_at_unix_ms)
        {
            old.ok_or(Error::SchemaMismatch)?
        } else {
            json!(report)
        };
        m.tx.execute("INSERT INTO th_github_dequeue_reports(tenant,repository,dequeue,sequence,report) VALUES($1,$2,$3,$4,$5)",&[&s.tenant(),&s.repository(),&id,&seq,&json!(report)]).await?;
        m.tx.execute("UPDATE th_github_dequeues SET report_sequence=$4,latest_observation=$5,retained_receipt=$6,mutation_not_sent=$7 WHERE tenant=$1 AND repository=$2 AND dequeue=$3",&[&s.tenant(),&s.repository(),&id,&seq,&latest,&retained,&not_sent]).await?;
        let mut v = describe(&m.tx, s, &load(&m.tx, s, id).await?, &q, false).await?;
        v["recorded_observation"] = json!(report);
        m.write_completion(
            &v,
            &[Event {
                kind: "github.dequeue.observed".into(),
                payload: json!({"dequeue":id.to_string(),"sequence":seq}),
            }],
        )
        .await?;
        queue::assigned(&m.tx, auth, c.epoch, &q).await?;
        m.tx.commit().await?;
        Ok(v)
    }
}
