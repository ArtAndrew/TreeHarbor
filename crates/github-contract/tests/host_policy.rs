use serde_json::{Value, json};
use treeharbor_github_contract::{host_policy::*, queue::QueueTarget};
fn fixture() -> (QueueTarget, Report, Value) {
    let v: Value = serde_json::from_str(include_str!("fixtures/host-policy-v1.json")).unwrap();
    (
        serde_json::from_value(v["queue_target"].clone()).unwrap(),
        serde_json::from_value(v["report"].clone()).unwrap(),
        v,
    )
}
#[test]
fn independent_vector_retains_raw_policy_and_exact_candidate_without_granting_authority() {
    let (t, r, v) = fixture();
    r.validate(&t).unwrap();
    assert!(r.capture_reasons(&t).unwrap().is_empty());
    assert_eq!(
        serde_json::to_vec(&json!(r)).unwrap(),
        v["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(r.digest(&t).unwrap(), v["digest"]);
    assert!(json!(r).get("host_policy_verified").is_none());
    assert!(json!(r).get("merge_verified").is_none());
    // A stable request for changes is a legitimate observation, not merge permission.
    let mut denied = r.clone();
    denied.before.review.decision = Some("CHANGES_REQUESTED".into());
    denied.after = denied.before.clone();
    assert!(denied.capture_reasons(&t).unwrap().is_empty());
    assert_ne!(denied.digest(&t).unwrap(), r.digest(&t).unwrap());
}
#[test]
fn hidden_missing_and_nonempty_bypass_are_distinct_from_an_observed_empty_list() {
    let (t, r, _) = fixture();
    for (case, reason) in [
        ("redacted", "ruleset_bypass_redacted"),
        ("present", "ruleset_bypass_present"),
        ("forbidden", "ruleset_unavailable"),
        ("missing", "ruleset_unavailable"),
        ("inactive", "ruleset_not_active"),
        ("rules", "ruleset_rules_changed"),
        ("legacy", "legacy_protection_unavailable"),
    ] {
        let mut p = r.clone();
        let rs = &mut p.before.rulesets[0].document;
        match case {
            "redacted" => {
                rs.value
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("bypass_actors");
            }
            "present" => {
                rs.value.as_mut().unwrap()["bypass_actors"] = json!([{"actor_id":9007199254740993u64,"actor_type":"Integration","bypass_mode":"always"}])
            }
            "forbidden" | "missing" => {
                rs.status = if case == "forbidden" { 403 } else { 404 };
                rs.value = None;
            }
            "inactive" => rs.value.as_mut().unwrap()["enforcement"] = json!("disabled"),
            "rules" => {
                rs.value.as_mut().unwrap()["rules"][0]["parameters"]["merge_method"] =
                    json!("SQUASH")
            }
            "legacy" => {
                p.before.protection.status = 404;
                p.before.protection.value = None;
            }
            _ => unreachable!(),
        }
        p.after = p.before.clone();
        assert!(p.capture_reasons(&t).unwrap().contains(reason), "{case}");
        assert_ne!(p.digest(&t).unwrap(), r.digest(&t).unwrap());
    }
}
#[test]
fn foreign_incomplete_duplicate_and_unbounded_documents_are_rejected() {
    let (t, r, _) = fixture();
    for case in [
        "foreign_repository",
        "foreign_source",
        "source_kind",
        "detail_id",
        "detail_source",
        "missing_detail",
        "duplicate_detail",
        "duplicate_rule",
        "duplicate_thread",
        "false_empty",
        "error_body",
        "unknown_decision",
        "wrong_target",
        "deep",
        "oversize",
    ] {
        let mut p = r.clone();
        match case {
            "foreign_repository" => p.repository_name = "other/repo".into(),
            "foreign_source" => p.before.rules[0]["ruleset_source"] = json!("other/repo"),
            "source_kind" => p.before.rulesets[0].source.kind = "Organization".into(),
            "detail_id" => p.before.rulesets[0].document.value.as_mut().unwrap()["id"] = json!(43),
            "detail_source" => {
                p.before.rulesets[0].document.value.as_mut().unwrap()["source"] =
                    json!("other/repo")
            }
            "missing_detail" => p.before.rulesets.clear(),
            "duplicate_detail" => p.before.rulesets.push(p.before.rulesets[0].clone()),
            "duplicate_rule" => p.before.rules.push(p.before.rules[0].clone()),
            "duplicate_thread" => p
                .before
                .review
                .threads
                .push(p.before.review.threads[0].clone()),
            "false_empty" => {
                p.before.rulesets[0].document.value.as_mut().unwrap()["bypass_actors"] = Value::Null
            }
            "error_body" => p.before.protection.status = 403,
            "unknown_decision" => p.before.review.decision = Some("GREEN".into()),
            "wrong_target" => p.target.member.pull_number += 1,
            "deep" => {
                let mut v = json!(0);
                for _ in 0..20 {
                    v = json!([v]);
                }
                p.before.protection.value.as_mut().unwrap()["nested"] = v;
            }
            "oversize" => {
                p.before.protection.value.as_mut().unwrap()["text"] = json!("x".repeat(65536))
            }
            _ => unreachable!(),
        }
        assert!(p.validate(&t).is_err(), "{case}");
    }
    let mut v = json!(r);
    v["host_policy_verified"] = json!(true);
    assert!(serde_json::from_value::<Report>(v).is_err());
}
#[test]
fn review_and_candidate_drift_are_retained_without_reusing_old_permission() {
    let (t, r, _) = fixture();
    for case in ["decision", "thread", "head", "candidate", "rule"] {
        let mut p = r.clone();
        match case {
            "decision" => p.after.review.decision = Some("REVIEW_REQUIRED".into()),
            "thread" => p.after.review.threads[0].resolved = false,
            "head" => p.after.review.pull.head_revision = format!("git:sha1:{}", "ab".repeat(20)),
            "candidate" => p.target.member.position += 1,
            "rule" => p.after.rules[0]["parameters"]["merge_method"] = json!("REBASE"),
            _ => unreachable!(),
        }
        p.validate(&t).unwrap();
        assert!(!p.capture_reasons(&t).unwrap().is_empty(), "{case}");
        assert_ne!(p.digest(&t).unwrap(), r.digest(&t).unwrap());
    }
}
#[test]
fn inherited_sources_and_unknown_rules_are_preserved_without_guessing_semantics() {
    let (mut t, mut r, _) = fixture();
    let s = &mut r.before;
    s.rules[0]["ruleset_source_type"] = json!("Organization");
    s.rules[0]["ruleset_source"] = json!("example");
    s.rulesets[0].source.kind = "Organization".into();
    s.rulesets[0].source.name = "example".into();
    let detail = s.rulesets[0].document.value.as_mut().unwrap();
    detail["source_type"] = json!("Organization");
    detail["source"] = json!("example");
    let mut extra = s.rules[0].clone();
    extra["type"] = json!("future_rule");
    extra["parameters"] = json!({"unknown":true});
    s.rules.push(extra);
    detail["rules"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"future_rule","parameters":{"unknown":true}}));
    r.after = r.before.clone();
    t.expected_rules = rules_fingerprint(&r.before.rules).unwrap();
    assert!(r.capture_reasons(&t).unwrap().is_empty());
    // Capture consistency intentionally does not assert semantic policy support.
    assert_eq!(r.before.rules[1]["type"], "future_rule");
    let mut missing = r.clone();
    missing.before.rulesets[0].document.value.as_mut().unwrap()["rules"]
        .as_array_mut()
        .unwrap()
        .pop();
    missing.after = missing.before.clone();
    assert!(
        missing
            .capture_reasons(&t)
            .unwrap()
            .contains("ruleset_rules_changed")
    );
}

#[test]
fn observation_v9_does_not_upgrade_old_receipts_or_allow_mixed_effects() {
    use treeharbor_github_contract::queue::QueueObservation;
    let (t, policy, _) = fixture();
    let mut wire = json!({"version":9,"started_at_unix_ms":1,"finished_at_unix_ms":2,
        "snapshot":null,"receipt":null,"async_result":null,"token_revoked":true,
        "host_policy":policy});
    let r: QueueObservation = serde_json::from_value(wire.clone()).unwrap();
    r.validate(&t).unwrap();
    assert_eq!(json!(r), wire);
    assert_eq!(r.state(&t), "uncertain");
    for version in 1..=8 {
        wire["version"] = json!(version);
        assert!(
            serde_json::from_value::<QueueObservation>(wire.clone())
                .unwrap()
                .validate(&t)
                .is_err()
        );
    }
    wire["version"] = json!(9);
    wire["mutation_not_sent"] = json!(true);
    assert!(
        serde_json::from_value::<QueueObservation>(wire.clone())
            .unwrap()
            .validate(&t)
            .is_err()
    );
    wire.as_object_mut().unwrap().remove("mutation_not_sent");
    wire["snapshot"] = json!(policy.after.group.target);
    assert!(
        serde_json::from_value::<QueueObservation>(wire.clone())
            .unwrap()
            .validate(&t)
            .is_err()
    );
    wire["snapshot"] = Value::Null;
    wire.as_object_mut().unwrap().remove("host_policy");
    assert!(
        serde_json::from_value::<QueueObservation>(wire)
            .unwrap()
            .validate(&t)
            .is_err()
    );
    for old in [
        include_str!("fixtures/queue-no-send-v2.json"),
        include_str!("fixtures/queue-enqueue-v3.json"),
        include_str!("fixtures/queue-groups-v4.json"),
        include_str!("fixtures/group-checks-v5.json"),
        include_str!("fixtures/group-attempt-v6.json"),
        include_str!("fixtures/workflow-definition-v7.json"),
        include_str!("fixtures/publication-v8.json"),
    ] {
        let wire: Value = serde_json::from_str(old).unwrap();
        let retained: QueueObservation = serde_json::from_value(wire.clone()).unwrap();
        assert!(retained.host_policy.is_none());
        assert_eq!(
            serde_json::to_vec(&json!(retained)).unwrap(),
            serde_json::to_vec(&wire).unwrap()
        );
    }
}
