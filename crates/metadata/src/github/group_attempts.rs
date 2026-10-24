use super::*;
use treeharbor_github_contract::{
    group_attempts as ga, group_checks as gc, queue::QueueObservation,
};

async fn source(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    sequence: i64,
) -> Result<gc::GroupCheckReport> {
    if !(1..=4096).contains(&sequence) {
        return Err(Error::Invalid("check sequence"));
    }
    let id: Uuid = r.get("queue");
    let row=tx.query_opt("SELECT report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4",&[&s.tenant(),&s.repository(),&id,&sequence]).await?.ok_or(Error::NotFound)?;
    let v: QueueObservation =
        serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
    v.validate(&queue::target(r)?)
        .map_err(|_| Error::SchemaMismatch)?;
    let c = v.group_checks.ok_or(Error::Invalid("check report"))?;
    group_checks::validate_report(tx, s, r, &c).await?;
    Ok(c)
}
pub(super) async fn validate_report(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    a: &ga::Report,
) -> Result<gc::GroupCheckReport> {
    let c = source(tx, s, r, a.target.check_sequence).await?;
    a.target
        .validate_source(&queue::target(r)?, &c)
        .map_err(|_| Error::Invalid("attempt source check"))?;
    Ok(c)
}
impl Metadata {
    pub async fn github_queue_attempt_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        selection: &ga::Selection,
    ) -> Result<Value> {
        uuid(id)?;
        selection
            .validate()
            .map_err(|_| Error::Invalid("attempt selection"))?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        let r = queue::load(&tx, s, id).await?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let t = queue::target(&r)?;
        let c = source(&tx, s, &r, selection.check_sequence).await?;
        let snapshot = match selection.side {
            gc::Side::Before => &c.checks.before,
            gc::Side::After => &c.checks.after,
        };
        let suite = snapshot
            .suites
            .iter()
            .find(|s| s.id == selection.suite_id)
            .ok_or(Error::NotFound)?;
        let check = suite
            .runs
            .iter()
            .find(|c| c.id == selection.check_id)
            .ok_or(Error::NotFound)?;
        let selected = ga::Target {
            check_sequence: selection.check_sequence,
            side: selection.side,
            subject: c.checks.subject.clone(),
            suite_id: suite.id.clone(),
            app_id: suite.app_id.ok_or(Error::Invalid("missing check App"))?,
            workflow: suite
                .workflow
                .clone()
                .ok_or(Error::Invalid("missing workflow"))?,
            check: check.clone(),
        };
        selected
            .validate_source(&t, &c)
            .map_err(|_| Error::Invalid("attempt source check"))?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,"target":t,"group_attempt_target":selected});
        tx.commit().await?;
        Ok(result)
    }
    pub async fn github_queue_attempts(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if !(0..=4096).contains(&after) {
            return Err(Error::Invalid("attempt cursor"));
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
        let rows=tx.query("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence>$4 AND report->'group_attempt' IS NOT NULL AND report->'group_attempt'<>'null'::jsonb ORDER BY sequence LIMIT 2",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut captures = Vec::new();
        if let Some(row) = rows.first() {
            let report: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
            let a = report.group_attempt.as_ref().ok_or(Error::SchemaMismatch)?;
            let source = validate_report(&tx, s, &r, a).await?;
            captures.push(json!({"sequence":row.get::<_,i64>("sequence"),"report":report,
                "attempt_linked":a.attempt_linked(&t,report.finished_at_unix_ms),"current_attempt_matches":a.current_attempt_matches(),
                "source_checks_consistent":source.checks.consistent(t.pull.repository_id),"definition_verified":false}));
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
