use super::*;
use treeharbor_github_contract::groups::GroupCapture;
use treeharbor_github_webhook::{DestroyReason, GroupAction, MergeGroupEvent};

fn matched_lifecycle(events: &Value) -> Result<bool> {
    let Some(events_in_capture) = events["events"].as_array() else {
        return Err(Error::SchemaMismatch);
    };
    let [first, last] = events_in_capture.as_slice() else {
        return Ok(false);
    };
    if events["more_events"] != false {
        return Ok(false);
    }
    let decode = |record: &Value| -> Result<MergeGroupEvent> {
        let fact: MergeGroupEvent =
            serde_json::from_value(record["fact"].clone()).map_err(|_| Error::SchemaMismatch)?;
        fact.validate().map_err(|_| Error::SchemaMismatch)?;
        Ok(fact)
    };
    let (a, b) = (decode(first)?, decode(last)?);
    let (requested, destroyed) = if a.action == GroupAction::ChecksRequested {
        (a, b)
    } else {
        (b, a)
    };
    Ok(requested.action == GroupAction::ChecksRequested
        && destroyed.action == GroupAction::Destroyed
        && destroyed.reason == Some(DestroyReason::Merged)
        && requested.head_ref == destroyed.head_ref)
}

async fn latest_captures(
    tx: &Transaction<'_>,
    s: &Scope,
    queue: Uuid,
    checks: i64,
    publication: i64,
) -> Result<bool> {
    // A delayed receipt must not hide a newer capture. Equal end times with
    // different observations are ambiguous at the collector's clock precision.
    let row = tx
        .query_one(
            "WITH selected(sequence,field) AS
              (VALUES ($4::bigint,'group_checks'::text),($5::bigint,'publication'::text))
             SELECT NOT EXISTS (
              SELECT 1 FROM selected
               JOIN th_github_queue_reports c ON c.tenant=$1 AND c.repository=$2
                   AND c.queue=$3 AND c.sequence=selected.sequence
               JOIN th_github_queue_reports r USING (tenant,repository,queue)
              WHERE r.sequence<>c.sequence AND jsonb_typeof(r.report->selected.field)='object'
               AND (r.sequence>c.sequence
                    OR (r.report->>'finished_at_unix_ms')::bigint >
                       (c.report->>'finished_at_unix_ms')::bigint
                    OR (r.report->>'finished_at_unix_ms'=c.report->>'finished_at_unix_ms'
                        AND r.report->selected.field<>c.report->selected.field))
             )",
            &[&s.tenant(), &s.repository(), &queue, &checks, &publication],
        )
        .await?;
    Ok(row.get(0))
}

async fn check_dispatch(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
    requirement: &Value,
    configuration: &str,
    merged_at: i64,
) -> Result<Value> {
    let queue_id: Uuid = q.get("queue");
    let id: Uuid = serde_json::from_value(requirement["publication"].clone())
        .map_err(|_| Error::SchemaMismatch)?;
    let publication = check_publications::load(tx, s, id).await?;
    let mut result =
        json!({"publication":id,"host_check":requirement["host_check"],"verified":false});
    let Some((proof, digest)) =
        managed_checks::dispatch_policy::load_retained(tx, s, &publication).await?
    else {
        result["reason"] = json!("dispatch_policy_unrecorded");
        return Ok(result);
    };
    let target = queue::target(q)?;
    if proof.queue != queue_id {
        return Err(Error::SchemaMismatch);
    }
    let capture = report(tx, s, queue_id, proof.host_policy_sequence, &target).await?;
    let evidence = capture.host_policy.as_ref().ok_or(Error::SchemaMismatch)?;
    if evidence
        .digest(&target)
        .map_err(|_| Error::SchemaMismatch)?
        != proof.host_policy_digest
        || capture.started_at_unix_ms != proof.capture_started_at_unix_ms
        || capture.finished_at_unix_ms != proof.capture_finished_at_unix_ms
    {
        return Err(Error::SchemaMismatch);
    }
    let receipt_sequence = requirement["receipt_sequence"]
        .as_i64()
        .ok_or(Error::SchemaMismatch)?;
    let receipt = tx
        .query_opt(
            "SELECT reporter,token,report FROM th_github_check_publication_reports
         WHERE tenant=$1 AND repository=$2 AND publication=$3 AND sequence=$4",
            &[&s.tenant(), &s.repository(), &id, &receipt_sequence],
        )
        .await?
        .ok_or(Error::SchemaMismatch)?;
    let receipt_report: Value = receipt.get("report");
    let reason = if proof.configuration_digest != configuration {
        Some("dispatch_policy_configuration_changed")
    } else if proof.decided_at_unix_ms >= merged_at
        || proof.capture_finished_at_unix_ms >= merged_at
    {
        Some("dispatch_policy_before_merge_unproven")
    } else if receipt_report["token_revoked"] != true
        || !check_publications::retained_approval_current(tx, s, &publication, &receipt).await?
        || !host_policy::current::trusted_sources(tx, s, q, &[proof.host_policy_sequence]).await?
    {
        Some("dispatch_or_publication_source_untrusted")
    } else {
        None
    };
    result["dispatch_policy_digest"] = json!(digest.to_string());
    result["host_policy_sequence"] = json!(proof.host_policy_sequence);
    result["decided_at_unix_ms"] = json!(proof.decided_at_unix_ms);
    result["verified"] = json!(reason.is_none());
    result["reason"] = json!(reason);
    Ok(result)
}

pub(super) async fn evaluate(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
    group: &GroupCapture,
    evidence: &Value,
    configuration_digest: Option<&str>,
) -> Result<Value> {
    let mut reasons = BTreeSet::new();
    let published = &evidence["publication"];
    let timing = &published["timing"];
    let complete = evidence["complete"] == true;
    if published["exact_candidate_published"] != true {
        reasons.insert("exact_publication_unproven");
    }
    if !complete || timing["reported_order"] != "before_merge" {
        reasons.insert("checks_before_merge_unproven");
    }
    if configuration_digest.is_none() {
        reasons.insert("active_supported_policy_unproven");
    }
    let target = queue::target(q)?;
    if group.before.members.len() != 1
        || !group
            .before
            .selected(&target)
            .is_some_and(|m| m.position == 1 && m.entry.base.as_ref() == Some(&group.before.main))
    {
        reasons.insert("single_member_at_observed_base_unproven");
    }
    let queue_id: Uuid = q.get("queue");
    if !latest_captures(
        tx,
        s,
        queue_id,
        evidence["check_sequence"]
            .as_i64()
            .ok_or(Error::SchemaMismatch)?,
        published["sequence"]
            .as_i64()
            .ok_or(Error::SchemaMismatch)?,
    )
    .await?
    {
        reasons.insert("newer_or_ambiguous_capture");
    }
    let events = groups::matched_events(tx, s, q, &group.before).await?;
    if !matched_lifecycle(&events)? {
        reasons.insert("signed_merge_group_lifecycle_unproven");
    }
    let mut decisions = Vec::new();
    if let (true, Some(merged_at), Some(configuration)) = (
        complete,
        timing["merged_at_unix_ms"].as_i64(),
        configuration_digest,
    ) {
        let requirements = evidence["requirements"]
            .as_array()
            .ok_or(Error::SchemaMismatch)?;
        for check in requirements {
            decisions.push(check_dispatch(tx, s, q, check, configuration, merged_at).await?);
        }
    }
    if decisions.is_empty() || decisions.iter().any(|v| v["verified"] != true) {
        reasons.insert("required_check_dispatch_unproven");
    }
    Ok(
        json!({"version":1,"verified":reasons.is_empty(),"reasons":reasons,
        "configuration_digest":configuration_digest,"signed_events":events,"check_dispatches":decisions}),
    )
}
