use super::*;
use treeharbor_github_contract::{
    group_attempts as ga, queue::QueueObservation, workflow_definitions as wd,
};

async fn source(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    sequence: i64,
) -> Result<(ga::Report, i64)> {
    if !(1..=4096).contains(&sequence) {
        return Err(Error::Invalid("attempt sequence"));
    }
    let row=tx.query_opt("SELECT report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4",&[&s.tenant(),&s.repository(),&r.get::<_,Uuid>("queue"),&sequence]).await?.ok_or(Error::NotFound)?;
    let v: QueueObservation =
        serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
    v.validate(&queue::target(r)?)
        .map_err(|_| Error::SchemaMismatch)?;
    let a = v.group_attempt.ok_or(Error::Invalid("attempt report"))?;
    group_attempts::validate_report(tx, s, r, &a).await?;
    Ok((a, v.finished_at_unix_ms))
}
pub(super) async fn validate_report(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    a: &wd::Report,
) -> Result<ga::Report> {
    let (source, finished) = source(tx, s, r, a.target.attempt_sequence).await?;
    let spec = specification(&binding(tx, s, r.get("binding")).await?)?;
    if !a
        .target
        .source()
        .map_err(|_| Error::Invalid("workflow source"))?
        .repository_name
        .eq_ignore_ascii_case(&format!("{}/{}", spec.upstream.owner, spec.upstream.name))
    {
        return Err(Error::Invalid("workflow repository"));
    }
    a.target
        .validate_source(&queue::target(r)?, &source, finished)
        .map_err(|_| Error::Invalid("definition source attempt"))?;
    Ok(source)
}
impl Metadata {
    pub async fn github_queue_definition_context(
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
        let (a, finished) = source(&tx, s, &r, sequence).await?;
        let selected = wd::Target {
            attempt_sequence: sequence,
            check: a.target.clone(),
            resolved: a.resolved.clone(),
            selected: a.before.selected.clone(),
        };
        selected
            .validate_source(&t, &a, finished)
            .map_err(|_| Error::Invalid("definition source attempt"))?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,"target":t,"workflow_definition_target":selected});
        tx.commit().await?;
        Ok(result)
    }
    pub async fn github_queue_definitions(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if !(0..=4096).contains(&after) {
            return Err(Error::Invalid("definition cursor"));
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
        let policy = check_policy::pinned(&tx, s, &r).await?;
        let admission = admission_policy::pinned_queue(&tx, s, &r).await?;
        let rows=tx.query("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence>$4 AND report->'workflow_definition' IS NOT NULL AND report->'workflow_definition'<>'null'::jsonb ORDER BY sequence LIMIT 2",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut captures = Vec::new();
        if let Some(row) = rows.first() {
            let report: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
            let d = report
                .workflow_definition
                .as_ref()
                .ok_or(Error::SchemaMismatch)?;
            let source = validate_report(&tx, s, &r, d).await?;
            let approved = if let Some((_, p, _)) = &admission {
                p.workflows
                    .iter()
                    .any(|c| d.workflow_file_matches_requirement(&t, c))
            } else {
                policy
                    .as_ref()
                    .is_some_and(|(_, p, _)| d.workflow_file_approved(&t, p))
            };
            captures.push(json!({"sequence":row.get::<_,i64>("sequence"),"report":report,
                "workflow_file_matches_pinned_policy":approved,"source_current_attempt_matches":source.current_attempt_matches(),
                "execution_context_verified":false,"definition_verified":false}));
        }
        let next = if rows.len() > 1 {
            Some(rows[0].get::<_, i64>("sequence"))
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,"captures":captures,"next_after":next,
            "workflow_policy":policy.map(|(pin,_,current)|json!({"pin":pin,"current":current})),"admission_policy":admission.map(|(pin,_,current)|json!({"pin":pin,"current":current})),"merge_verified":false,"host_policy_verified":false});
        tx.commit().await?;
        Ok(result)
    }
}
