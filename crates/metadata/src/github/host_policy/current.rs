//! Derived, bounded-current policy evidence. Never a provider transaction or write grant.
use super::*;
use treeharbor_github_contract::publication::Target;

pub(in crate::github) async fn evaluate(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
    sequence: i64,
    selected: &Target,
    checks_finished: i64,
) -> Result<(Value, bool)> {
    if !(1..=4096).contains(&sequence) {
        return Err(Error::Invalid("host policy sequence"));
    }
    let id: Uuid = q.get("queue");
    let t = queue::target(q)?;
    let row=tx.query_opt("SELECT report,reporter,token FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4", &[&s.tenant(),&s.repository(),&id,&sequence]).await?.ok_or(Error::NotFound)?;
    let report: QueueObservation =
        serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
    report.validate(&t).map_err(|_| Error::SchemaMismatch)?;
    let p = report
        .host_policy
        .as_ref()
        .ok_or(Error::Invalid("host policy report"))?;
    validate_report(tx, s, q, p).await?;
    let analysis = he::evaluate(p, &t).map_err(|_| Error::SchemaMismatch)?;
    let mut reasons = analysis.reasons.clone();
    let b = binding(tx, s, q.get("binding")).await?;
    let spec = specification(&b)?;
    let mut pinned = None;
    if let Some((pin, policy, active)) = admission_policy::pinned_queue(tx, s, q).await? {
        if !active {
            reasons.insert("owner_policy_not_current".into());
        }
        if let Some(c) = policy.host_configuration {
            let digest = c
                .digest(&c.visibility.target)
                .map_err(|_| Error::SchemaMismatch)?;
            if p.version != 2 || analysis.before_digest != digest || analysis.after_digest != digest
            {
                reasons.insert("owner_configuration_mismatch".into());
            }
            pinned = Some(json!({"policy":pin,"configuration_digest":digest}));
        } else {
            reasons.insert("complete_configuration_not_pinned".into());
        }
    } else {
        reasons.insert("admission_policy_unconfigured".into());
    }
    if p.target != *selected {
        reasons.insert("policy_candidate_mismatch".into());
    }
    let latest:Option<i64>=tx.query_one("SELECT max(sequence) FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report->'host_policy' IS NOT NULL AND report->'host_policy'<>'null'::jsonb", &[&s.tenant(),&s.repository(),&id]).await?.get(0);
    if latest != Some(sequence) {
        reasons.insert("newer_policy_capture_exists".into());
    }
    if !report.token_revoked {
        reasons.insert("provider_token_revocation_unproven".into());
    }
    let now: i64 = q.get("now_ms");
    if !fresh_window(
        report.started_at_unix_ms,
        report.finished_at_unix_ms,
        now,
        checks_finished,
    ) {
        reasons.insert("policy_capture_not_fresh_after_checks".into());
    }
    let latest: Option<QueueObservation> = q
        .get::<_, Option<Value>>("latest_observation")
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| Error::SchemaMismatch)?;
    let membership = latest.and_then(|r| r.group_capture);
    let matching = membership.as_ref().is_some_and(|g| {
        if !g.consistent(&t) {
            return false;
        }
        let mut a = g.before.clone();
        // Membership captures intentionally omit privileged queue settings. Every
        // other field must match; actual settings come only from this report.
        if a.target.configuration.is_none() {
            a.target.configuration = p.before.group.target.configuration.clone();
        }
        a == p.before.group && a == p.after.group
    });
    if !matching {
        reasons.insert("latest_membership_changed".into());
    }
    let latest_source:Option<i64>=tx.query_one("SELECT max(sequence) FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report=$4", &[&s.tenant(),&s.repository(),&id,&q.get::<_,Option<Value>>("latest_observation")]).await?.get(0);
    let latest_trusted = if let Some(sequence) = latest_source {
        trusted_sources(tx, s, q, &[sequence]).await?
    } else {
        false
    };
    if !latest_trusted {
        reasons.insert("latest_membership_not_trusted".into());
    }
    if b.get::<_, String>("state") != "active"
        || b.get::<_, i64>("epoch") != q.get::<_, i64>("epoch")
        || q.get::<_, bool>("cancel_requested")
        || q.get::<_, bool>("mutation_not_sent")
        || q.get::<_, bool>("dispatch_failed")
        || !q.get::<_, bool>("unexpired")
        || !queue::current(tx, s, q).await?
    {
        reasons.insert("queue_not_current".into());
    }
    let reporter: Option<String> = row.get("reporter");
    let token: Option<Uuid> = row.get("token");
    let trusted = if let (Some(actor), Some(token)) = (reporter, token) {
        actor == spec.bridge
            && actor == q.get::<_, String>("bridge")
            && crate::integration::trusted(tx, s, &actor, "bridge", "host.merge", Some(token))
                .await?
    } else {
        false
    };
    if !trusted {
        reasons.insert("capture_submitter_not_trusted".into());
    }
    if !crate::integration::trusted(
        tx,
        s,
        &q.get::<_, String>("principal"),
        "human",
        "host.queue",
        None,
    )
    .await?
    {
        reasons.insert("queue_owner_not_trusted".into());
    }
    let verified = reasons.is_empty();
    let evidence = json!({"version":1,"sequence":sequence,"membership_sequence":p.target.membership_sequence,
        "capture_digest":p.digest(&t).map_err(|_|Error::SchemaMismatch)?,"owner_pin":pinned,"evaluation":analysis,
        "started_at_unix_ms":report.started_at_unix_ms,"finished_at_unix_ms":report.finished_at_unix_ms,
        "reasons":reasons});
    Ok((evidence, verified))
}

/// Historical payloads remain readable; only attributable, non-revoked reports
/// may support a new admission assessment. Expiry alone does not erase history.
pub(in crate::github) async fn trusted_sources(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
    sequences: &[i64],
) -> Result<bool> {
    let spec = specification(&binding(tx, s, q.get("binding")).await?)?;
    let t = queue::target(q)?;
    for sequence in sequences {
        let row=tx.query_opt("SELECT report,reporter,token FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4", &[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue"),sequence]).await?.ok_or(Error::NotFound)?;
        let r: QueueObservation =
            serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
        r.validate(&t).map_err(|_| Error::SchemaMismatch)?;
        let (Some(actor), Some(token)) = (
            row.get::<_, Option<String>>("reporter"),
            row.get::<_, Option<Uuid>>("token"),
        ) else {
            return Ok(false);
        };
        if !r.token_revoked
            || actor != spec.bridge
            || actor != q.get::<_, String>("bridge")
            || !crate::integration::trusted(tx, s, &actor, "bridge", "host.merge", Some(token))
                .await?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn fresh_window(start: i64, finish: i64, now: i64, checks_finished: i64) -> bool {
    start >= now.saturating_sub(120_000)
        && finish <= now.saturating_add(15_000)
        && start >= checks_finished
}
#[cfg(test)]
mod tests {
    use super::fresh_window;
    #[test]
    fn freshness_is_bounded_from_first_read_and_cannot_predate_checked_results() {
        assert!(fresh_window(880_000, 1_000_000, 1_000_000, 880_000));
        assert!(!fresh_window(879_999, 1_000_000, 1_000_000, 879_999));
        assert!(fresh_window(1_000_000, 1_015_000, 1_000_000, 1_000_000));
        assert!(!fresh_window(1_000_000, 1_015_001, 1_000_000, 1_000_000));
        assert!(!fresh_window(999_999, 1_000_000, 1_000_000, 1_000_000));
    }
}
