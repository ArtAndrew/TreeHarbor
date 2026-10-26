use super::*;
use treeharbor_github_contract::{host_policy as hp, queue::QueueObservation};

pub(super) async fn validate_report(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    p: &hp::Report,
) -> Result<()> {
    let t = queue::target(r)?;
    p.validate(&t)
        .map_err(|_| Error::Invalid("host policy report"))?;
    let g =
        group_checks::membership(tx, s, r.get("queue"), p.target.membership_sequence, &t).await?;
    p.target
        .validate_source(&t, &g)
        .map_err(|_| Error::Invalid("host policy membership"))?;
    let spec = specification(&binding(tx, s, r.get("binding")).await?)?;
    if !p
        .repository_name
        .eq_ignore_ascii_case(&format!("{}/{}", spec.upstream.owner, spec.upstream.name))
    {
        return Err(Error::Invalid("host policy repository"));
    }
    Ok(())
}
impl Metadata {
    pub async fn github_queue_host_policy_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        sequence: i64,
    ) -> Result<Value> {
        let mut v = self
            .github_queue_publication_context(auth, epoch, id, sequence)
            .await?;
        let object = v.as_object_mut().ok_or(Error::SchemaMismatch)?;
        let target = object
            .remove("publication_target")
            .ok_or(Error::SchemaMismatch)?;
        object.insert("host_policy_target".into(), target);
        Ok(v)
    }
    pub async fn github_queue_host_policies(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if !(0..=4096).contains(&after) {
            return Err(Error::Invalid("host policy cursor"));
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
        let rows=tx.query("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence>$4 AND report->'host_policy' IS NOT NULL AND report->'host_policy'<>'null'::jsonb ORDER BY sequence LIMIT 2",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut captures = Vec::new();
        if let Some(row) = rows.first() {
            let report: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
            let p = report.host_policy.as_ref().ok_or(Error::SchemaMismatch)?;
            validate_report(&tx, s, &r, p).await?;
            captures.push(
                json!({"sequence":row.get::<_,i64>("sequence"),"report":report,
                "capture_digest":p.digest(&t).map_err(|_|Error::SchemaMismatch)?,
                "capture_reasons":p.capture_reasons(&t).map_err(|_|Error::SchemaMismatch)?}),
            );
        }
        let next = if rows.len() > 1 {
            Some(rows[0].get::<_, i64>("sequence"))
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,
            "captures":captures,"next_after":next,"host_policy_verified":false,"admission_verified":false,"merge_verified":false});
        tx.commit().await?;
        Ok(result)
    }
}
