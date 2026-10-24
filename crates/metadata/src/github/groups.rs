//! Historical membership and signed facts are exposed separately from merge proof.
use super::*;
use treeharbor_github_contract::{groups::GroupSnapshot, queue::QueueObservation};

pub(super) async fn matched_events(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    g: &GroupSnapshot,
) -> Result<Value> {
    let t = queue::target(r)?;
    let Some(member) = g.selected(&t) else {
        return Ok(json!({"events":[],"more_events":false}));
    };
    let (Some(base), Some(head)) = (&member.entry.base, &member.candidate) else {
        return Ok(json!({"events":[],"more_events":false}));
    };
    let spec = specification(&binding(tx, s, r.get("binding")).await?)?;
    let facts = json!({"version":1,"installation_id":spec.upstream.installation_id,"owner_id":spec.upstream.owner_id,"repository_id":spec.upstream.repository_id,"base_ref":format!("refs/heads/{}",t.pull.base_ref),"base_revision":base.revision,"head_revision":head.commit.revision,"head_tree":head.commit.tree});
    // Events are immutable and may precede or follow the capture. Channel state
    // does not erase old signed evidence; binding/epoch/provider identity does scope it.
    let rows=tx.query("SELECT e.id,e.event,e.channel,e.normalized,encode(e.digest,'hex') AS hash FROM th_github_events e JOIN th_github_event_channels c USING(tenant,repository,channel) WHERE e.tenant=$1 AND e.repository=$2 AND c.binding=$3 AND c.epoch=$4 AND c.app_id=$5 AND c.installation_id=$6 AND c.repository_id=$7 AND c.owner_id=$8 AND e.normalized @> $9::jsonb ORDER BY e.id LIMIT 9",&[&s.tenant(),&s.repository(),&r.get::<_,Uuid>("binding"),&r.get::<_,i64>("epoch"),&(spec.app_id as i64),&(spec.upstream.installation_id as i64),&(spec.upstream.repository_id as i64),&(spec.upstream.owner_id as i64),&facts]).await?;
    let events:Vec<_>=rows.iter().take(8).map(|e|json!({"sequence":e.get::<_,i64>("id"),"event":e.get::<_,Uuid>("event"),"channel":e.get::<_,Uuid>("channel"),"body_sha256":e.get::<_,String>("hash"),"fact":e.get::<_,Value>("normalized")})).collect();
    Ok(json!({"events":events,"more_events":rows.len()>8}))
}
impl Metadata {
    /// One retained capture per page, with a cursor in immutable report sequence.
    pub async fn github_queue_groups(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if !(0..=4096).contains(&after) {
            return Err(Error::Invalid("queue group cursor"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let r = queue::load(&tx, s, id).await?;
        if r.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let t = queue::target(&r)?;
        let rows=tx.query("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence>$4 AND report->'group_capture' IS NOT NULL AND report->'group_capture'<>'null'::jsonb ORDER BY sequence LIMIT 2",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut captures = Vec::new();
        if let Some(row) = rows.first() {
            let report: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
            let g = report.group_capture.as_ref().ok_or(Error::SchemaMismatch)?;
            let events_before = matched_events(&tx, s, &r, &g.before).await?;
            let events_after = matched_events(&tx, s, &r, &g.after).await?;
            captures.push(json!({"sequence":row.get::<_,i64>("sequence"),"report":report,
                "consistent":g.consistent(&t),
                "before_exact_merge_parents":g.before.selected(&t).is_some_and(|m|m.has_exact_merge_parents()),
                "after_exact_merge_parents":g.after.selected(&t).is_some_and(|m|m.has_exact_merge_parents()),
                "before_signed_events":events_before,"after_signed_events":events_after}));
        }
        let next = if rows.len() > 1 {
            Some(rows[0].get::<_, i64>("sequence"))
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(
            json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,
            "captures":captures,"next_after":next,"merge_verified":false,"host_policy_verified":false}),
        )
    }
}
