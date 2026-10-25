use super::*;
use treeharbor_github_contract::{groups::GroupCapture, publication as p, queue::QueueObservation};

pub(super) async fn validate_report(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    v: &p::Report,
) -> Result<GroupCapture> {
    let t = queue::target(r)?;
    let g =
        group_checks::membership(tx, s, r.get("queue"), v.target.membership_sequence, &t).await?;
    v.target
        .validate_source(&t, &g)
        .map_err(|_| Error::Invalid("publication membership"))?;
    Ok(g)
}
impl Metadata {
    pub async fn github_queue_publication_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        sequence: i64,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        let r = queue::load(&tx, s, id).await?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let t = queue::target(&r)?;
        let g = group_checks::membership(&tx, s, id, sequence, &t).await?;
        let selected = p::Target {
            membership_sequence: sequence,
            member: g
                .before
                .selected(&t)
                .ok_or(Error::Invalid("missing publication member"))?
                .clone(),
        };
        selected
            .validate_source(&t, &g)
            .map_err(|_| Error::Invalid("publication membership"))?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,"target":t,"publication_target":selected});
        tx.commit().await?;
        Ok(result)
    }
    pub async fn github_queue_publications(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if !(0..=4096).contains(&after) {
            return Err(Error::Invalid("publication cursor"));
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
        let rows=tx.query("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence>$4 AND report->'publication' IS NOT NULL AND report->'publication'<>'null'::jsonb ORDER BY sequence LIMIT 2",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut captures = Vec::new();
        if let Some(row) = rows.first() {
            let report: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
            let p = report.publication.as_ref().ok_or(Error::SchemaMismatch)?;
            let g = validate_report(&tx, s, &r, p).await?;
            let events = groups::matched_events(&tx, s, &r, &g.before).await?;
            captures.push(json!({"sequence":row.get::<_,i64>("sequence"),"report":report,"exact_candidate_published":p.exact_candidate_published(&t),"signed_events":events,"checks_verified":false}));
        }
        let next = if rows.len() > 1 {
            Some(rows[0].get::<_, i64>("sequence"))
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,"captures":captures,"next_after":next,"merge_verified":false,"host_policy_verified":false});
        tx.commit().await?;
        Ok(result)
    }
}
