use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_webhook::{VerifiedDelivery, WebhookEvent};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EventChannelSpec {
    pub binding: String,
    pub max_events: i64,
    pub max_bytes: i64,
    pub max_aliases: i32,
}
impl EventChannelSpec {
    fn validate(&self) -> Result<Uuid> {
        let id = wire::id(&self.binding)?;
        if !(1..=1_000_000).contains(&self.max_events)
            || !(1..=10_737_418_240).contains(&self.max_bytes)
            || !(1..=16).contains(&self.max_aliases)
        {
            return Err(Error::Invalid("event channel resource budget"));
        }
        Ok(id)
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct EventReceipt {
    pub channel: Uuid,
    pub event: Uuid,
    pub sequence: i64,
}
fn channel_description(r: &Row) -> Value {
    json!({"version":1,"channel":r.get::<_,Uuid>("channel"),"binding":r.get::<_,Uuid>("binding"),
        "epoch":r.get::<_,i64>("epoch"),"state":r.get::<_,String>("state"),
        "max_events":r.get::<_,i64>("max_events"),"max_bytes":r.get::<_,i64>("max_bytes"),
        "max_aliases":r.get::<_,i32>("max_aliases"),"retained_events":r.get::<_,i64>("retained_events"),
        "retained_bytes":r.get::<_,i64>("retained_bytes")})
}
async fn channel(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<Row> {
    tx.query_opt(
        "SELECT * FROM th_github_event_channels WHERE tenant=$1 AND repository=$2 AND channel=$3",
        &[&s.tenant(), &s.repository(), &id],
    )
    .await?
    .ok_or(Error::NotFound)
}
async fn human(tx: &Transaction<'_>, auth: &Authorization) -> Result<()> {
    let r = tx
        .query_one(
            "SELECT kind FROM th_principals WHERE tenant=$1 AND principal=$2",
            &[&auth.scope().tenant(), &auth.principal()],
        )
        .await?;
    if r.get::<_, String>(0) != "human" {
        return Err(Error::Forbidden);
    }
    Ok(())
}
impl Metadata {
    pub async fn create_github_event_channel(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
        spec: &EventChannelSpec,
    ) -> Result<Value> {
        uuid(id)?;
        let binding_id = spec.validate()?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.events.create.v1",
                json!([1, id, spec]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        human(&m.tx, auth).await?;
        let s = auth.scope();
        let b = projections::active_binding(&m.tx, s, binding_id, c.epoch).await?;
        let p=m.tx.query_opt("SELECT observation FROM th_github_probes WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND state='observed' ORDER BY completed_at DESC,probe LIMIT 1",&[&s.tenant(),&s.repository(),&binding_id,&c.epoch]).await?.ok_or(Error::HostBindingState)?;
        let observed: Observation =
            serde_json::from_value(p.get(0)).map_err(|_| Error::SchemaMismatch)?;
        observed.validate(&b)?;
        if observed.archived || observed.disabled {
            return Err(Error::HostBindingState);
        }
        // A global route cannot point into two tenants. ON CONFLICT also covers
        // races between owners in different repository transactions.
        let inserted=m.tx.execute("INSERT INTO th_github_event_channels(tenant,repository,channel,binding,epoch,app_id,installation_id,repository_id,owner_id,principal,state,max_events,max_bytes,max_aliases) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,'active',$11,$12,$13) ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&id,&binding_id,&c.epoch,&(b.app_id as i64),&(b.upstream.installation_id as i64),&(b.upstream.repository_id as i64),&(b.upstream.owner_id as i64),&auth.principal(),&spec.max_events,&spec.max_bytes,&spec.max_aliases]).await?;
        if inserted != 1 {
            return Err(Error::HostEventState);
        }
        let result = channel_description(&channel(&m.tx, s, id).await?);
        m.complete(
            result,
            vec![Event {
                kind: "github.events.configured".into(),
                payload: json!({"channel":id}),
            }],
        )
        .await
    }
    pub async fn revoke_github_event_channel(
        &mut self,
        auth: &Authorization,
        c: &Command,
        id: Uuid,
    ) -> Result<Value> {
        uuid(id)?;
        let m = match self
            .begin(request(auth, c, "github.events.revoke.v1", json!([1, id]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::RepositoryWrite).await?;
        human(&m.tx, auth).await?;
        let s = auth.scope();
        channel(&m.tx, s, id).await?;
        m.tx.execute("UPDATE th_github_event_channels SET state='revoked',revoked_at=clock_timestamp() WHERE tenant=$1 AND repository=$2 AND channel=$3 AND state='active'",&[&s.tenant(),&s.repository(),&id]).await?;
        let result = channel_description(&channel(&m.tx, s, id).await?);
        m.complete(
            result,
            vec![Event {
                kind: "github.events.revoked".into(),
                payload: json!({"channel":id}),
            }],
        )
        .await
    }
    /// Only trusted receiver code can supply this opaque authenticated delivery.
    /// The caller must retry an unknown DB outcome using the original bytes/UUID.
    pub async fn accept_github_event(
        &mut self,
        delivery: Uuid,
        verified: &VerifiedDelivery,
    ) -> Result<EventReceipt> {
        uuid(delivery)?;
        let WebhookEvent::MergeGroup(e) = verified.event() else {
            return Err(Error::Invalid("merge-group delivery required"));
        };
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let route=tx.query_opt("SELECT tenant,repository,channel FROM th_github_event_channels WHERE app_id=$1 AND installation_id=$2 AND repository_id=$3 AND state='active'",&[&(verified.app_id() as i64),&(e.installation_id as i64),&(e.repository_id as i64)]).await?.ok_or(Error::HostEventState)?;
        let s = Scope::new(route.get::<_, String>(0), route.get(1))?;
        repository_lock(&tx, &s, true).await?;
        let channel_id: Uuid = route.get(2);
        let ch = channel(&tx, &s, channel_id).await?;
        let repo=tx.query_one("SELECT authority,epoch,state FROM th_repositories WHERE tenant=$1 AND repository=$2",&[&s.tenant(),&s.repository()]).await?;
        if repo.get::<_, String>(0) != "connected"
            || repo.get::<_, i64>(1) != ch.get::<_, i64>("epoch")
            || repo.get::<_, String>(2) != "active"
        {
            return Err(Error::Fenced);
        }
        if ch.get::<_, String>("state") != "active"
            || ch.get::<_, i64>("owner_id") != e.owner_id as i64
        {
            return Err(Error::HostEventState);
        }
        let b = projections::active_binding(&tx, &s, ch.get("binding"), ch.get("epoch")).await?;
        if b.app_id != verified.app_id()
            || b.upstream.installation_id != e.installation_id
            || b.upstream.repository_id != e.repository_id
            || b.upstream.owner_id != e.owner_id
        {
            return Err(Error::HostEventState);
        }
        let digest = verified.digest().to_vec();
        let alias=tx.query_opt("SELECT digest FROM th_github_event_deliveries WHERE tenant=$1 AND repository=$2 AND channel=$3 AND delivery=$4",&[&s.tenant(),&s.repository(),&channel_id,&delivery]).await?;
        if let Some(ref a) = alias
            && a.get::<_, Vec<u8>>(0) != digest
        {
            return Err(Error::IdempotencyConflict);
        }
        let existing=tx.query_opt("SELECT event,id,body FROM th_github_events WHERE tenant=$1 AND repository=$2 AND channel=$3 AND digest=$4",&[&s.tenant(),&s.repository(),&channel_id,&digest]).await?;
        let receipt = if let Some(r) = existing {
            // Defend even against an accidental hash collision; never conflate bytes.
            if r.get::<_, Vec<u8>>(2) != verified.body() {
                return Err(Error::IdempotencyConflict);
            }
            if alias.is_none() {
                let n:i64=tx.query_one("SELECT count(*) FROM th_github_event_deliveries WHERE tenant=$1 AND repository=$2 AND channel=$3 AND digest=$4",&[&s.tenant(),&s.repository(),&channel_id,&digest]).await?.get(0);
                if n >= i64::from(ch.get::<_, i32>("max_aliases")) {
                    return Err(Error::HostEventLimit);
                }
            }
            EventReceipt {
                channel: channel_id,
                event: r.get(0),
                sequence: r.get(1),
            }
        } else {
            let bytes = verified.body().len() as i64;
            if ch.get::<_, i64>("retained_events") >= ch.get::<_, i64>("max_events")
                || bytes > ch.get::<_, i64>("max_bytes") - ch.get::<_, i64>("retained_bytes")
            {
                return Err(Error::HostEventLimit);
            }
            let id = Uuid::new_v4();
            let r=tx.query_one("INSERT INTO th_github_events(tenant,repository,channel,event,key_id,digest,body,normalized) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",&[&s.tenant(),&s.repository(),&channel_id,&id,&verified.key_id(),&digest,&verified.body(),&json!(e)]).await?;
            let receipt = EventReceipt {
                channel: channel_id,
                event: id,
                sequence: r.get(0),
            };
            let operation = format!("github-event:{id}");
            tx.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'github-webhook','github.events.received.v1',$5)",&[&s.tenant(),&s.repository(),&operation,&digest,&json!(receipt)]).await?;
            tx.execute("INSERT INTO th_outbox(tenant,repository,operation_key,ordinal,kind,payload) VALUES($1,$2,$3,0,'github.events.received',$4)",&[&s.tenant(),&s.repository(),&operation,&json!(receipt)]).await?;
            tx.execute("UPDATE th_github_event_channels SET retained_events=retained_events+1,retained_bytes=retained_bytes+$4 WHERE tenant=$1 AND repository=$2 AND channel=$3",&[&s.tenant(),&s.repository(),&channel_id,&bytes]).await?;
            receipt
        };
        if alias.is_none() {
            tx.execute("INSERT INTO th_github_event_deliveries(tenant,repository,channel,delivery,digest,key_id) VALUES($1,$2,$3,$4,$5,$6)",&[&s.tenant(),&s.repository(),&channel_id,&delivery,&digest,&verified.key_id()]).await?;
        }
        tx.commit().await?;
        Ok(receipt)
    }
    pub async fn github_event_page(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if after < 0 {
            return Err(Error::Invalid("event cursor"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let ch = channel(&tx, s, id).await?;
        let rows=tx.query("SELECT id,event,normalized,key_id,encode(digest,'hex') AS hash,(extract(epoch from received_at)*1000)::bigint AS received FROM th_github_events WHERE tenant=$1 AND repository=$2 AND channel=$3 AND id>$4 ORDER BY id LIMIT 9",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let values:Vec<_>=rows.iter().take(8).map(|r|json!({"sequence":r.get::<_,i64>("id"),"event":r.get::<_,Uuid>("event"),"fact":r.get::<_,Value>("normalized"),"key_id":r.get::<_,Uuid>("key_id"),"body_sha256":r.get::<_,String>("hash"),"received_at_unix_ms":r.get::<_,i64>("received")})).collect();
        let next = if rows.len() > 8 {
            Some(rows[7].get::<_, i64>("id"))
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(
            json!({"version":1,"channel":channel_description(&ch),"events":values,"next_after":next,"merge_verified":false}),
        )
    }
}
