use serde_json::{Value, json};
use treeharbor_github_contract::{group_checks::*, queue::*};
fn fixture() -> (QueueTarget, QueueObservation, QueueObservation) {
    let f: Value = serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap();
    let t = QueueTarget {
        pull: serde_json::from_value(f["target"].clone()).unwrap(),
        expected_base_revision: format!("git:sha1:{}", "22".repeat(20)),
        expected_rules: serde_json::from_value(f["report"]["rules_before"].clone()).unwrap(),
    };
    (
        t,
        serde_json::from_str(include_str!("fixtures/queue-groups-v4.json")).unwrap(),
        serde_json::from_str(include_str!("fixtures/group-checks-v5.json")).unwrap(),
    )
}
#[test]
fn golden_preserves_distinct_group_and_push_checks_on_identical_sha_name_and_app() {
    let (t, m, r) = fixture();
    r.validate(&t).unwrap();
    assert_eq!(
        json!(r),
        serde_json::from_str::<Value>(include_str!("fixtures/group-checks-v5.json")).unwrap()
    );
    let c = r.group_checks.unwrap();
    c.target
        .validate_membership(&t, m.group_capture.as_ref().unwrap())
        .unwrap();
    assert!(c.checks.consistent(t.pull.repository_id));
    let suites = &c.checks.before.suites;
    assert_eq!(suites[0].runs[0].name, suites[1].runs[0].name);
    assert_ne!(suites[0].runs[0].id, suites[1].runs[0].id);
    assert_eq!(suites[0].workflow.as_ref().unwrap().event, "merge_group");
    assert_eq!(suites[1].workflow.as_ref().unwrap().event, "push");
}
#[test]
fn rejects_aliased_foreign_future_and_mixed_check_evidence() {
    let (t, _, original) = fixture();
    for case in [
        "version",
        "mixed",
        "subject",
        "repo",
        "sha",
        "suite-alias",
        "run-alias",
        "name-drift",
        "app-drift",
        "workflow-drift",
        "time",
        "url",
        "sequence",
        "conclusion",
    ] {
        let mut v = json!(original);
        match case {
            "version" => v["version"] = json!(4),
            "mixed" => v["mutation_not_sent"] = json!(true),
            "subject" => {
                v["group_checks"]["target"]["subject"]["tree"] =
                    json!(format!("git:sha1:{}", "ab".repeat(20)))
            }
            "sequence" => v["group_checks"]["target"]["membership_sequence"] = json!(0),
            other => {
                let suites = &mut v["group_checks"]["checks"]["after"]["suites"];
                match other {
                    "repo" => suites[0]["repository_id"] = json!(12),
                    "sha" => {
                        suites[0]["head_revision"] = json!(format!("git:sha1:{}", "ab".repeat(20)))
                    }
                    "suite-alias" => suites[1]["id"] = suites[0]["id"].clone(),
                    "run-alias" => suites[1]["runs"][0]["id"] = suites[0]["runs"][0]["id"].clone(),
                    "name-drift" => suites[0]["runs"][0]["name"] = json!("other"),
                    "app-drift" => suites[0]["app_id"] = json!(8),
                    "workflow-drift" => suites[0]["workflow"]["event"] = json!("push"),
                    "time" => {
                        suites[0]["runs"][0]["completed_at_unix_ms"] =
                            json!(original.finished_at_unix_ms + 15001)
                    }
                    "url" => {
                        suites[0]["workflow"]["file"]["repository_file_url"] =
                            json!("https://example.com/private")
                    }
                    "conclusion" => suites[0]["runs"][0]["status"] = json!("QUEUED"),
                    _ => unreachable!(),
                }
            }
        }
        assert!(
            serde_json::from_value::<QueueObservation>(v)
                .unwrap()
                .validate(&t)
                .is_err(),
            "{case}"
        );
    }
}
#[test]
fn workflow_rerun_and_check_completion_are_observed_drift_not_inherited_success() {
    let (t, _, mut r) = fixture();
    let c = r.group_checks.as_mut().unwrap();
    c.checks.after.suites[0]
        .workflow
        .as_mut()
        .unwrap()
        .observed_run_attempt = 2;
    assert!(!c.checks.consistent(t.pull.repository_id));
    r.validate(&t).unwrap();
    let c = r.group_checks.as_mut().unwrap();
    let old = &mut c.checks.before.suites[0].runs[0];
    old.status = "IN_PROGRESS".into();
    old.conclusion = None;
    old.completed_at_unix_ms = None;
    assert!(!c.checks.consistent(t.pull.repository_id));
    r.validate(&t).unwrap();
}
#[test]
fn missing_workflow_is_retained_and_wrong_membership_side_or_source_is_refused() {
    let (t, mut m, mut r) = fixture();
    let c = r.group_checks.as_mut().unwrap();
    c.checks.before.suites[0].workflow = None;
    c.checks.after.suites[0].workflow = None;
    r.validate(&t).unwrap();
    let c = r.group_checks.as_ref().unwrap();
    let g = m.group_capture.as_mut().unwrap();
    g.after.members.clear();
    g.after.target.entry = None;
    assert!(c.target.validate_membership(&t, g).is_err());
    let mut selected = c.target.clone();
    selected.side = Side::Before;
    selected.validate_membership(&t, g).unwrap();
    g.before.target.pull.head_revision = format!("git:sha1:{}", "ac".repeat(20));
    g.before.members[0].source_revision = g.before.target.pull.head_revision.clone();
    assert!(selected.validate_membership(&t, g).is_err());
}

#[test]
fn total_run_and_encoded_byte_limits_apply_across_suites_and_both_reads() {
    let (t, _, mut r) = fixture();
    let c = r.group_checks.as_mut().unwrap();
    for snapshot in [&mut c.checks.before, &mut c.checks.after] {
        for (i, suite) in snapshot.suites.iter_mut().enumerate() {
            let original = suite.runs[0].clone();
            suite.runs = (0..50)
                .map(|n| Run {
                    id: format!("CR_{i}_{n}"),
                    ..original.clone()
                })
                .collect();
        }
    }
    r.validate(&t).unwrap();
    let mut too_many = r.clone();
    let view = &mut too_many.group_checks.as_mut().unwrap().checks.after;
    let mut extra = view.suites[0].runs[0].clone();
    extra.id = "CR_overflow".into();
    view.suites[1].runs.push(extra);
    assert!(too_many.validate(&t).is_err());
    let checks = &mut r.group_checks.as_mut().unwrap().checks;
    for snapshot in [&mut checks.before, &mut checks.after] {
        for suite in &mut snapshot.suites {
            for run in &mut suite.runs {
                run.name = "x".repeat(256);
            }
        }
    }
    checks.validate_snapshots(t.pull.repository_id).unwrap();
    assert!(r.validate(&t).is_err());
}
