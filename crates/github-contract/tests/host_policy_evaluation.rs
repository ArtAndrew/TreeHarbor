use serde_json::{Value, json};
use treeharbor_github_contract::{
    host_policy as hp, host_policy_evaluation::*, queue::QueueTarget,
};

fn fixture() -> (Profile, Value) {
    let v: Value =
        serde_json::from_str(include_str!("fixtures/host-policy-configuration-v1.json")).unwrap();
    (serde_json::from_value(v["profile"].clone()).unwrap(), v)
}
fn report(p: &Profile) -> (QueueTarget, hp::Report) {
    let v: Value = serde_json::from_str(include_str!("fixtures/host-policy-v1.json")).unwrap();
    let t = serde_json::from_value(v["queue_target"].clone()).unwrap();
    let mut r: hp::Report = serde_json::from_value(v["report"].clone()).unwrap();
    for s in [&mut r.before, &mut r.after] {
        s.protection = p.protection.clone();
        s.rules = p.rules.clone();
        s.rulesets = p.rulesets.clone();
        s.group.target.configuration = p.queue.clone();
    }
    (t, r)
}
fn legacy(p: &mut Profile) -> &mut Value {
    p.protection.value.as_mut().unwrap()
}
fn replace_queue(p: &mut Profile, key: &str, value: Value) {
    p.rules[0]["parameters"][key] = value.clone();
    p.rulesets[0].document.value.as_mut().unwrap()["rules"][0]["parameters"][key] = value;
}
fn add_rule(p: &mut Profile, kind: &str, parameters: Option<Value>) {
    let mut r = json!({"type":kind});
    if let Some(parameters) = parameters {
        r["parameters"] = parameters;
    }
    p.rulesets[0].document.value.as_mut().unwrap()["rules"]
        .as_array_mut()
        .unwrap()
        .push(r.clone());
    r["ruleset_id"] = json!(42);
    r["ruleset_source_type"] = json!("Repository");
    r["ruleset_source"] = json!("example/repo");
    p.rules.push(r);
}
fn review_params() -> Value {
    json!({"required_approving_review_count":0,"require_code_owner_review":false,
        "require_last_push_approval":false,"dismiss_stale_reviews_on_push":false,
        "required_review_thread_resolution":true})
}

#[test]
fn independent_python_hashlib_vector_and_pr_independent_identity() {
    let (p, v) = fixture();
    assert_eq!(
        p.canonical().unwrap(),
        v["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(p.digest().unwrap(), v["digest"]);
    let a = p.analyze().unwrap();
    assert!(a.configuration_reasons.is_empty(), "{a:?}");
    assert!(a.candidate_requirements.is_empty());
    assert_eq!(json!(a.required), json!([{"name":"CI / build","app_id":7}]));
    let (t, mut r) = report(&p);
    assert!(evaluate(&r, &t).unwrap().reasons.is_empty());
    r.after.review.decision = Some("CHANGES_REQUESTED".into());
    r.after.review.threads[0].resolved = false;
    let e = evaluate(&r, &t).unwrap();
    assert_eq!(e.before_digest, e.after_digest);
    assert!(e.reasons.contains("review_not_approved"));
    assert!(e.reasons.contains("capture_changed"));
    assert!(json!(e).get("admission_verified").is_none());
    // No owner identity or PR/entry is required to inspect configuration.
    assert!(json!(p).get("target").is_none());
    for field in ["repository_id", "repository_name", "base_ref"] {
        let mut changed = json!(p);
        changed[field] = match field {
            "repository_id" => json!(12),
            "repository_name" => json!("Example/repo"),
            _ => json!("release"),
        };
        let changed: Profile = serde_json::from_value(changed).unwrap();
        assert_ne!(changed.digest().unwrap(), p.digest().unwrap(), "{field}");
    }
}

#[test]
fn configuration_order_is_canonical_but_nested_metadata_is_not_discarded() {
    let (mut p, _) = fixture();
    add_rule(&mut p, "deletion", None);
    add_rule(&mut p, "non_fast_forward", None);
    let id = p.digest().unwrap();
    let a = p.analyze().unwrap();
    p.rules.reverse();
    assert_eq!(p.digest().unwrap(), id);
    assert_eq!(p.analyze().unwrap(), a);
    p.rulesets[0].document.value.as_mut().unwrap()["name"] = json!("renamed");
    assert_ne!(p.digest().unwrap(), id);
    assert!(p.analyze().unwrap().configuration_reasons.is_empty());
    let id = p.digest().unwrap();
    p.rulesets[0].document.value.as_mut().unwrap()["rules"]
        .as_array_mut()
        .unwrap()
        .reverse();
    assert_ne!(p.digest().unwrap(), id);
}

#[test]
fn every_queue_setting_is_compared_and_unknown_parameters_never_pass() {
    let (p, _) = fixture();
    for (key, value) in [
        ("check_response_timeout_minutes", json!(21)),
        ("grouping_strategy", json!("HEADGREEN")),
        ("max_entries_to_build", json!(6)),
        ("max_entries_to_merge", json!(6)),
        ("merge_method", json!("SQUASH")),
        ("min_entries_to_merge", json!(2)),
        ("min_entries_to_merge_wait_minutes", json!(1)),
        ("future", json!(true)),
    ] {
        let mut bad = p.clone();
        replace_queue(&mut bad, key, value);
        let a = bad.analyze().unwrap();
        assert!(!a.configuration_reasons.is_empty(), "{key}");
        assert_ne!(bad.digest().unwrap(), p.digest().unwrap());
    }
    for method in ["SQUASH", "REBASE"] {
        let mut changed = p.clone();
        replace_queue(&mut changed, "merge_method", json!(method));
        changed.queue.as_mut().unwrap().merge_method = method.into();
        let a = changed.analyze().unwrap();
        assert!(a.configuration_reasons.is_empty());
        assert!(a.candidate_requirements.contains("non_merge_publication"));
    }
    let mut changed = p.clone();
    replace_queue(&mut changed, "grouping_strategy", json!("HEADGREEN"));
    changed.queue.as_mut().unwrap().merging_strategy = "HEADGREEN".into();
    assert!(
        changed
            .analyze()
            .unwrap()
            .candidate_requirements
            .contains("headgreen_group_checks")
    );
    changed.queue = None;
    assert!(
        changed
            .analyze()
            .unwrap()
            .configuration_reasons
            .contains("queue_configuration_unproven")
    );
}

#[test]
fn checks_union_all_sources_reject_any_app_duplicates_and_mismatched_sets() {
    let (p, _) = fixture();
    for issuer in [
        Value::Null,
        json!(-1),
        json!(0),
        json!(u64::MAX),
        json!("7"),
    ] {
        let mut bad = p.clone();
        legacy(&mut bad)["required_status_checks"]["checks"][0]["app_id"] = issuer;
        assert!(!bad.analyze().unwrap().configuration_reasons.is_empty());
    }
    let mut extra = p.clone();
    add_rule(
        &mut extra,
        "required_status_checks",
        Some(json!({"strict_required_status_checks_policy":true,
        "required_status_checks":[{"context":"CI / more","integration_id":8}]})),
    );
    let a = extra.analyze().unwrap();
    assert!(a.configuration_reasons.is_empty(), "{a:?}");
    assert_eq!(a.required.len(), 2);
    let (mut t, r) = report(&extra);
    t.expected_rules = hp::rules_fingerprint(&extra.rules).unwrap();
    assert!(
        evaluate(&r, &t)
            .unwrap()
            .reasons
            .contains("required_check_set_changed")
    );
    t.pull.required = a.required.into_iter().collect();
    assert!(evaluate(&r, &t).unwrap().reasons.is_empty());
    for case in [
        "duplicate",
        "hidden_context",
        "same_name",
        "unknown_check",
        "missing_issuer",
    ] {
        let mut bad = p.clone();
        match case {
            "duplicate" => legacy(&mut bad)["required_status_checks"]["checks"]
                .as_array_mut()
                .unwrap()
                .push(json!({"context":"CI / build","app_id":7})),
            "hidden_context" => legacy(&mut bad)["required_status_checks"]["contexts"]
                .as_array_mut()
                .unwrap()
                .push(json!("hidden")),
            "same_name" => add_rule(
                &mut bad,
                "required_status_checks",
                Some(
                    json!({"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"CI / build","integration_id":8}]}),
                ),
            ),
            "unknown_check" => {
                legacy(&mut bad)["required_status_checks"]["checks"][0]["future"] = json!(false)
            }
            _ => add_rule(
                &mut bad,
                "required_status_checks",
                Some(
                    json!({"strict_required_status_checks_policy":true,"required_status_checks":[{"context":"CI / more"}]}),
                ),
            ),
        }
        assert!(
            !bad.analyze().unwrap().configuration_reasons.is_empty(),
            "{case}"
        );
    }
}

#[test]
fn all_legacy_flags_are_interpreted_and_absent_null_unknown_fields_fail_closed() {
    let (p, _) = fixture();
    for key in [
        "enforce_admins",
        "allow_force_pushes",
        "allow_deletions",
        "lock_branch",
        "required_linear_history",
        "required_signatures",
        "block_creations",
        "allow_fork_syncing",
        "required_conversation_resolution",
    ] {
        for case in ["absent", "null", "unknown"] {
            let mut bad = p.clone();
            match case {
                "absent" => {
                    legacy(&mut bad).as_object_mut().unwrap().remove(key);
                }
                "null" => legacy(&mut bad)[key] = Value::Null,
                _ => legacy(&mut bad)[key]["future"] = json!(true),
            }
            assert!(
                !bad.analyze().unwrap().configuration_reasons.is_empty(),
                "{key}/{case}"
            );
        }
        let mut changed = p.clone();
        legacy(&mut changed)[key]["enabled"] = json!(key != "enforce_admins");
        assert_ne!(changed.digest().unwrap(), p.digest().unwrap());
        let a = changed.analyze().unwrap();
        match key {
            "required_linear_history" | "required_signatures" => {
                assert!(a.candidate_requirements.contains(key))
            }
            "block_creations" | "allow_fork_syncing" => assert!(a.configuration_reasons.is_empty()),
            "required_conversation_resolution" => {
                assert!(a.reviews.iter().any(|r| r.resolve_threads))
            }
            _ => assert!(!a.configuration_reasons.is_empty()),
        }
    }
}

#[test]
fn bypass_hidden_sources_and_future_rules_never_become_supported_configuration() {
    let (p, _) = fixture();
    for case in [
        "hidden",
        "bypass",
        "unavailable",
        "disabled",
        "missing_rule",
        "unknown_rule",
        "unknown_top",
        "unknown_detail",
        "unknown_parameter",
        "restrictions",
    ] {
        let mut bad = p.clone();
        match case {
            "hidden" => {
                bad.rulesets[0]
                    .document
                    .value
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("bypass_actors");
            }
            "bypass" => {
                bad.rulesets[0].document.value.as_mut().unwrap()["bypass_actors"] =
                    json!([{"actor_id":7,"actor_type":"Integration","bypass_mode":"always"}])
            }
            "unavailable" => {
                bad.rulesets[0].document = hp::Document {
                    status: 403,
                    value: None,
                }
            }
            "disabled" => {
                bad.rulesets[0].document.value.as_mut().unwrap()["enforcement"] = json!("disabled")
            }
            "missing_rule" => bad.rulesets[0].document.value.as_mut().unwrap()["rules"] = json!([]),
            "unknown_rule" => add_rule(&mut bad, "future_requirement", None),
            "unknown_top" => legacy(&mut bad)["future_policy"] = json!(false),
            "unknown_detail" => {
                bad.rulesets[0].document.value.as_mut().unwrap()["future_policy"] = json!(false)
            }
            "unknown_parameter" => add_rule(&mut bad, "deletion", Some(json!({"future":false}))),
            _ => {
                legacy(&mut bad)["restrictions"] = json!({"users":[{"id":1}],"teams":[],"apps":[]})
            }
        }
        assert!(
            !bad.analyze().unwrap().configuration_reasons.is_empty(),
            "{case}"
        );
    }
}

#[test]
fn review_requirements_never_infer_counts_code_owners_or_teams_from_approved() {
    let (p, _) = fixture();
    for (key, value) in [
        ("required_approving_review_count", json!(2)),
        ("require_code_owner_review", json!(true)),
        ("require_last_push_approval", json!(true)),
        (
            "required_reviewers",
            json!([{"reviewer":{"type":"Team","id":10},"minimum_approvals":1,"file_patterns":["**"]}]),
        ),
    ] {
        let mut changed = p.clone();
        let mut params = review_params();
        params[key] = value;
        add_rule(&mut changed, "pull_request", Some(params));
        let (mut t, mut r) = report(&changed);
        t.expected_rules = hp::rules_fingerprint(&changed.rules).unwrap();
        r.before.review.decision = Some("APPROVED".into());
        r.after = r.before.clone();
        let e = evaluate(&r, &t).unwrap();
        assert!(
            e.reasons
                .contains("ruleset/42/pull_request/review_approvals_unproven"),
            "{key}"
        );
        assert_eq!(e.before_digest, e.after_digest);
    }
    let mut changed = p.clone();
    add_rule(&mut changed, "pull_request", Some(review_params()));
    let (mut t, mut r) = report(&changed);
    t.expected_rules = hp::rules_fingerprint(&changed.rules).unwrap();
    assert!(evaluate(&r, &t).unwrap().reasons.is_empty());
    r.before.review.threads[0].resolved = false;
    r.before.review.threads[0].outdated = true;
    r.after = r.before.clone();
    assert!(
        evaluate(&r, &t)
            .unwrap()
            .reasons
            .contains("review_threads_unresolved")
    );
    for key in [
        "required_approving_review_count",
        "require_code_owner_review",
        "require_last_push_approval",
        "dismiss_stale_reviews_on_push",
        "required_review_thread_resolution",
    ] {
        let mut bad = p.clone();
        let mut params = review_params();
        params.as_object_mut().unwrap().remove(key);
        add_rule(&mut bad, "pull_request", Some(params));
        assert!(
            !bad.analyze().unwrap().configuration_reasons.is_empty(),
            "{key}"
        );
    }
}

#[test]
fn old_observation_fixture_is_valid_but_not_semantically_complete() {
    let v: Value = serde_json::from_str(include_str!("fixtures/host-policy-v1.json")).unwrap();
    let t: QueueTarget = serde_json::from_value(v["queue_target"].clone()).unwrap();
    let r: hp::Report = serde_json::from_value(v["report"].clone()).unwrap();
    assert!(r.capture_reasons(&t).unwrap().is_empty());
    let e = evaluate(&r, &t).unwrap();
    assert!(e.reasons.contains("queue_configuration_unproven"));
    assert!(
        e.reasons
            .contains("legacy/required_signatures/flag_unproven")
    );
    assert_eq!(r.digest(&t).unwrap(), v["digest"]);
}

#[test]
fn legacy_reviews_retain_every_constraint_and_do_not_turn_bypass_into_empty_policy() {
    let (mut p, _) = fixture();
    legacy(&mut p)["required_pull_request_reviews"] = json!({
        "dismiss_stale_reviews":true,"require_code_owner_reviews":true,
        "require_last_push_approval":true,"required_approving_review_count":2,
        "bypass_pull_request_allowances":{"users":[],"teams":[],"apps":[]},
        "dismissal_restrictions":{"users":[],"teams":[],"apps":[]}});
    let a = p.analyze().unwrap();
    assert!(a.configuration_reasons.is_empty(), "{a:?}");
    assert_eq!(
        a.reviews[0],
        ReviewRequirement {
            source: "legacy/reviews".into(),
            approvals: 2,
            code_owners: true,
            last_push: true,
            dismiss_stale: true,
            resolve_threads: false,
            required_teams: false
        }
    );
    let (t, mut r) = report(&p);
    r.before.review.decision = Some("APPROVED".into());
    r.after = r.before.clone();
    assert!(
        evaluate(&r, &t)
            .unwrap()
            .reasons
            .contains("legacy/reviews/review_approvals_unproven")
    );
    for key in [
        "bypass_pull_request_allowances",
        "dismiss_stale_reviews",
        "require_code_owner_reviews",
        "require_last_push_approval",
        "required_approving_review_count",
    ] {
        let mut bad = p.clone();
        legacy(&mut bad)["required_pull_request_reviews"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            !bad.analyze().unwrap().configuration_reasons.is_empty(),
            "{key}"
        );
    }
    for actors in ["users", "teams", "apps"] {
        let mut bad = p.clone();
        legacy(&mut bad)["required_pull_request_reviews"]["bypass_pull_request_allowances"]
            [actors] = json!([{"id":7}]);
        assert!(
            !bad.analyze().unwrap().configuration_reasons.is_empty(),
            "{actors}"
        );
    }
}

#[test]
fn malformed_profiles_and_unknown_nested_conditions_cannot_be_pinned_as_supported() {
    let (p, _) = fixture();
    for (key, value) in [
        ("version", json!(2)),
        ("repository_id", json!(0)),
        ("repository_name", json!("example")),
        ("base_ref", json!("../main")),
    ] {
        let mut v = json!(p);
        v[key] = value;
        let bad: Profile = serde_json::from_value(v).unwrap();
        assert!(bad.canonical().is_err(), "{key}");
    }
    let mut unknown = json!(p);
    unknown["approved"] = json!(true);
    assert!(serde_json::from_value::<Profile>(unknown).is_err());
    for value in [
        json!({}),
        json!({"include":[],"exclude":[],"future":true}),
        json!({"include":[null],"exclude":[]}),
        json!({"include":[],"exclude":null}),
    ] {
        let mut bad = p.clone();
        bad.rulesets[0].document.value.as_mut().unwrap()["conditions"]["ref_name"] = value;
        assert!(!bad.analyze().unwrap().configuration_reasons.is_empty());
    }
    let mut large = p.clone();
    legacy(&mut large)["padding"] = json!("x".repeat(MAX_PROFILE_BYTES));
    assert!(large.canonical().is_err());
    let mut bad = p.clone();
    bad.rulesets.clear();
    assert!(bad.canonical().is_err());
    let mut bad = p.clone();
    bad.rulesets.push(bad.rulesets[0].clone());
    assert!(bad.canonical().is_err());
}

#[test]
fn later_policy_and_head_changes_invalidate_candidate_diagnostics() {
    let (p, _) = fixture();
    let (t, mut r) = report(&p);
    r.after.protection.value.as_mut().unwrap()["required_conversation_resolution"]["enabled"] =
        json!(true);
    r.after.review.threads[0].resolved = false;
    let e = evaluate(&r, &t).unwrap();
    assert_ne!(e.before_digest, e.after_digest);
    assert!(e.reasons.contains("configuration_changed"));
    assert!(e.reasons.contains("review_threads_unresolved"));
    let (_, mut r) = report(&p);
    r.before.review.pull.head_revision = "git:sha1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
    r.after = r.before.clone();
    let e = evaluate(&r, &t).unwrap();
    assert!(e.reasons.contains("review_source_changed"));
    assert!(e.reasons.contains("review_head_not_current_open_pr"));
    for required in ["required_signatures", "required_linear_history"] {
        let mut changed = p.clone();
        add_rule(&mut changed, required, None);
        let (mut t, r) = report(&changed);
        t.expected_rules = hp::rules_fingerprint(&changed.rules).unwrap();
        assert!(
            evaluate(&r, &t)
                .unwrap()
                .reasons
                .contains(&format!("{required}_unproven"))
        );
    }
}

#[test]
fn selected_pr_review_cannot_certify_other_queue_members() {
    let (p, _) = fixture();
    let (t, mut r) = report(&p);
    let mut other = r.before.group.members[0].clone();
    other.position = 2;
    other.pull_node = "PR_other".into();
    other.pull_number = 18;
    other.entry.id = "MQE_other".into();
    other.entry.state = "QUEUED".into();
    other.entry.head = None;
    other.entry.base = None;
    other.candidate = None;
    other.source_revision = "git:sha1:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".into();
    r.before.group.members.push(other);
    r.after = r.before.clone();
    r.validate(&t).unwrap();
    assert!(
        evaluate(&r, &t)
            .unwrap()
            .reasons
            .contains("multi_member_policy_evidence_unproven")
    );
}

#[test]
fn inherited_check_sources_are_unioned_and_ruleset_order_is_not_identity() {
    let (mut p, _) = fixture();
    let rule = json!({"type":"required_status_checks","parameters":{
        "strict_required_status_checks_policy":false,"do_not_enforce_on_create":true,
        "required_status_checks":[{"context":"Organization / audit","integration_id":13}]}});
    let mut effective = rule.clone();
    effective["ruleset_id"] = json!(43);
    effective["ruleset_source_type"] = json!("Organization");
    effective["ruleset_source"] = json!("example");
    p.rules.push(effective);
    let mut source = p.rulesets[0].clone();
    source.source = hp::RuleSource {
        id: 43,
        kind: "Organization".into(),
        name: "example".into(),
    };
    let v = source.document.value.as_mut().unwrap();
    v["id"] = json!(43);
    v["source_type"] = json!("Organization");
    v["source"] = json!("example");
    v["rules"] = json!([rule]);
    p.rulesets.push(source);
    let a = p.analyze().unwrap();
    assert!(a.configuration_reasons.is_empty(), "{a:?}");
    assert_eq!(a.required.len(), 2);
    let id = p.digest().unwrap();
    p.rulesets.reverse();
    p.rules.reverse();
    assert_eq!(p.digest().unwrap(), id);
    assert_eq!(p.analyze().unwrap(), a);
}
