use super::*;
use treeharbor_github_contract::{
    group_checks as gc,
    groups::GroupCapture,
    queue::{QueueObservation, QueueTarget},
};

pub(super) async fn membership(
    tx: &Transaction<'_>,
    s: &Scope,
    id: Uuid,
    sequence: i64,
    t: &QueueTarget,
) -> Result<GroupCapture> {
    if !(1..=4096).contains(&sequence) {
        return Err(Error::Invalid("membership sequence"));
    }
    let r=tx.query_opt("SELECT report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4",&[&s.tenant(),&s.repository(),&id,&sequence]).await?.ok_or(Error::NotFound)?;
    let v: QueueObservation =
        serde_json::from_value(r.get("report")).map_err(|_| Error::SchemaMismatch)?;
    v.validate(t).map_err(|_| Error::SchemaMismatch)?;
    v.group_capture.ok_or(Error::Invalid("membership report"))
}
pub(super) async fn validate_report(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    v: &gc::GroupCheckReport,
) -> Result<GroupCapture> {
    let t = queue::target(r)?;
    let g = membership(tx, s, r.get("queue"), v.target.membership_sequence, &t).await?;
    v.target
        .validate_membership(&t, &g)
        .map_err(|_| Error::Invalid("group check subject"))?;
    Ok(g)
}
fn matches(v: &gc::Snapshot, required: &treeharbor_github_contract::RequiredCheck) -> Vec<Value> {
    v.suites
        .iter()
        .filter(|s| s.app_id == Some(required.app_id))
        .flat_map(|s| {
            s.runs
                .iter()
                .filter(|r| r.name == required.name)
                .map(move |r| {
                    json!({"suite":s.id,"run":r,"workflow":s.workflow,
                "merge_group_event":s.workflow.as_ref().is_some_and(|w|w.event=="merge_group")})
                })
        })
        .collect()
}
impl Metadata {
    pub async fn github_queue_check_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        sequence: i64,
        side: gc::Side,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        let r = queue::load(&tx, s, id).await?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let t = queue::target(&r)?;
        let g = membership(&tx, s, id, sequence, &t).await?;
        let view = match side {
            gc::Side::Before => &g.before,
            gc::Side::After => &g.after,
        };
        let subject = view
            .selected(&t)
            .and_then(|m| m.candidate.as_ref())
            .ok_or(Error::Invalid("missing group candidate"))?
            .commit
            .clone();
        let selected = gc::GroupCheckTarget {
            membership_sequence: sequence,
            side,
            subject,
        };
        selected
            .validate_membership(&t, &g)
            .map_err(|_| Error::Invalid("group check subject"))?;
        queue::assigned(&tx, auth, epoch, &r).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,"target":t,"group_check_target":selected});
        tx.commit().await?;
        Ok(result)
    }
    pub async fn github_queue_checks(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        after: i64,
    ) -> Result<Value> {
        uuid(id)?;
        if !(0..=4096).contains(&after) {
            return Err(Error::Invalid("queue check cursor"));
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
        let rows=tx.query("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence>$4 AND report->'group_checks' IS NOT NULL AND report->'group_checks'<>'null'::jsonb ORDER BY sequence LIMIT 2",&[&s.tenant(),&s.repository(),&id,&after]).await?;
        let mut captures = Vec::new();
        if let Some(row) = rows.first() {
            let report: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
            let checks = report.group_checks.as_ref().ok_or(Error::SchemaMismatch)?;
            let g = validate_report(&tx, s, &r, checks).await?;
            let view = match checks.target.side {
                gc::Side::Before => &g.before,
                gc::Side::After => &g.after,
            };
            let events = groups::matched_events(&tx, s, &r, view).await?;
            let required:Vec<_>=t.pull.required.iter().map(|p|json!({"required":p,"before_matches":matches(&checks.checks.before,p),"after_matches":matches(&checks.checks.after,p)})).collect();
            captures.push(json!({"sequence":row.get::<_,i64>("sequence"),"report":report,
                "checks_consistent":checks.checks.consistent(t.pull.repository_id),
                "membership_consistent":g.consistent(&t),"signed_events":events,"selected_checks":required,
                "attempt_verified":false,"definition_verified":false}));
        }
        let next = if rows.len() > 1 {
            Some(rows[0].get::<_, i64>("sequence"))
        } else {
            None
        };
        context(&tx, auth, epoch, Permission::Read).await?;
        let result = json!({"version":1,"queue":id,"binding":r.get::<_,Uuid>("binding"),"epoch":epoch,
            "captures":captures,"next_after":next,"merge_verified":false,"host_policy_verified":false});
        tx.commit().await?;
        Ok(result)
    }
}
