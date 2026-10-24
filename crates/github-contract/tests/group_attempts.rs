use serde_json::{Value, json};
use treeharbor_github_contract::{group_checks::GroupCheckReport, queue::*};
fn fixture() -> (QueueTarget, GroupCheckReport, QueueObservation) {
    let v: Value = serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap();
    let t = QueueTarget {
        pull: serde_json::from_value(v["target"].clone()).unwrap(),
        expected_base_revision: format!("git:sha1:{}", "22".repeat(20)),
        expected_rules: serde_json::from_value(v["report"]["rules_before"].clone()).unwrap(),
    };
    let source: QueueObservation =
        serde_json::from_str(include_str!("fixtures/group-checks-v5.json")).unwrap();
    let r = serde_json::from_str(include_str!("fixtures/group-attempt-v6.json")).unwrap();
    (t, source.group_checks.unwrap(), r)
}
#[test]
fn golden_links_distinct_job_and_check_nodes_with_64_bit_provider_identifiers() {
    let (t, source, r) = fixture();
    r.validate(&t).unwrap();
    assert_eq!(
        json!(r),
        serde_json::from_str::<Value>(include_str!("fixtures/group-attempt-v6.json")).unwrap()
    );
    let a = r.group_attempt.unwrap();
    a.target.validate_source(&t, &source).unwrap();
    assert!(a.attempt_linked(&t, r.finished_at_unix_ms));
    assert!(a.current_attempt_matches());
    assert_ne!(a.before.jobs[0].node_id, a.before.check.run.id);
    assert!(a.resolved.run_id > u32::MAX as u64);
    assert!(a.resolved.check_id > u32::MAX as u64);
}
#[test]
fn rejects_foreign_attempt_identity_aliasing_bounds_and_mixed_observations() {
    let (t, _, r) = fixture();
    for case in [
        "version",
        "mixed",
        "sequence",
        "push",
        "check-id",
        "run-id",
        "suite-id",
        "repo",
        "head",
        "attempt",
        "job-attempt",
        "job-check",
        "alias",
        "time",
        "too-many",
        "conclusion",
    ] {
        let mut v = json!(r);
        match case {
            "version" => v["version"] = json!(5),
            "mixed" => v["mutation_not_sent"] = json!(true),
            "sequence" => v["group_attempt"]["target"]["check_sequence"] = json!(0),
            "push" => v["group_attempt"]["target"]["workflow"]["event"] = json!("push"),
            other => {
                let a = &mut v["group_attempt"]["after"];
                match other {
                    "check-id" => a["check"]["database_id"] = json!(7),
                    "run-id" => a["selected"]["run_id"] = json!(7),
                    "suite-id" => a["selected"]["suite_node"] = json!("CS_other"),
                    "repo" => a["current"]["repository_id"] = json!(12),
                    "head" => {
                        a["jobs"][0]["head_revision"] =
                            json!(format!("git:sha1:{}", "ab".repeat(20)))
                    }
                    "attempt" => a["selected"]["number"] = json!(2),
                    "job-attempt" => a["jobs"][0]["attempt"] = json!(2),
                    "job-check" => a["jobs"][0]["check_run_id"] = json!(7),
                    "alias" => a["jobs"][0]["id"] = json!(7),
                    "time" => {
                        a["jobs"][0]["completed_at_unix_ms"] = json!(r.finished_at_unix_ms + 15001)
                    }
                    "too-many" => a["jobs"] = json!(vec![a["jobs"][0].clone(); 101]),
                    "conclusion" => a["check"]["run"]["status"] = json!("QUEUED"),
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
fn historical_attempt_link_never_claims_that_a_later_attempt_is_currently_validated() {
    let (t, _, mut r) = fixture();
    let a = r.group_attempt.as_mut().unwrap();
    a.after.current.number = 2;
    a.after.current.status = "IN_PROGRESS".into();
    a.after.current.conclusion = None;
    assert!(!a.current_attempt_matches());
    assert!(!a.attempt_linked(&t, r.finished_at_unix_ms));
    a.before.current = a.after.current.clone();
    // Exact old attempt is historical evidence only, despite two agreeing reads.
    assert!(a.attempt_linked(&t, r.finished_at_unix_ms));
    assert!(!a.current_attempt_matches());
    assert_eq!(r.state(&t), "uncertain");
}
#[test]
fn missing_job_or_changed_check_does_not_inherit_the_retained_success() {
    let (t, source, mut r) = fixture();
    let a = r.group_attempt.as_mut().unwrap();
    a.before.jobs.clear();
    a.after.jobs.clear();
    a.validate(&t, r.finished_at_unix_ms).unwrap();
    assert!(!a.attempt_linked(&t, r.finished_at_unix_ms));
    for s in [&mut a.before, &mut a.after] {
        s.check.run.conclusion = Some("FAILURE".into());
    }
    assert!(!a.attempt_linked(&t, r.finished_at_unix_ms));
    a.target.check.conclusion = Some("FAILURE".into());
    assert!(a.target.validate_source(&t, &source).is_err());
}

#[test]
fn encoded_pair_bound_applies_before_the_individual_job_limit() {
    let (t, _, mut r) = fixture();
    let a = r.group_attempt.as_mut().unwrap();
    for view in [&mut a.before, &mut a.after] {
        let original = view.jobs[0].clone();
        for n in 1..50 {
            let mut j = original.clone();
            j.id += n;
            j.node_id = format!("JOB_{n}");
            j.check_run_id += n;
            view.jobs.push(j);
        }
    }
    r.validate(&t).unwrap();
    let a = r.group_attempt.as_mut().unwrap();
    for view in [&mut a.before, &mut a.after] {
        for j in view.jobs.iter_mut().skip(1) {
            j.name = "x".repeat(256);
        }
        assert!(view.jobs.len() < treeharbor_github_contract::group_attempts::MAX_JOBS);
    }
    assert!(serde_json::to_vec(a).unwrap().len() > 48 * 1024);
    assert!(r.validate(&t).is_err());
}
