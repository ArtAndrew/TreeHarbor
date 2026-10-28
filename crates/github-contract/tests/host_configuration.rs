use serde_json::{Value, json};
use treeharbor_github_contract::host_configuration::*;
fn fixture() -> (Target, Inspection, Value) {
    let v: Value = serde_json::from_str(include_str!(
        "fixtures/host-configuration-inspection-v1.json"
    ))
    .unwrap();
    (
        serde_json::from_value(v["target"].clone()).unwrap(),
        serde_json::from_value(v["inspection"].clone()).unwrap(),
        v,
    )
}
fn protection(c: &mut Configuration) -> &mut Value {
    c.profile.protection.value.as_mut().unwrap()
}
fn legacy(c: &mut Configuration) -> &mut Legacy {
    c.visibility.legacy.as_mut().unwrap()
}
fn reviews(c: &mut Configuration) {
    protection(c)["required_pull_request_reviews"] = json!({"required_approving_review_count":0,"dismiss_stale_reviews":false,"require_code_owner_reviews":false,"require_last_push_approval":false});
    legacy(c)
        .flags
        .insert("requiresApprovingReviews".into(), true);
}
#[test]
fn independent_vector_preserves_existing_profile_and_binds_all_observations() {
    let (t, r, v) = fixture();
    assert_eq!(
        r.canonical(&t).unwrap(),
        v["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(r.digest(&t).unwrap(), v["digest"]);
    assert_eq!(
        r.before.canonical(&t).unwrap(),
        v["configuration_canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(r.before.digest(&t).unwrap(), v["configuration_digest"]);
    let old: Value =
        serde_json::from_str(include_str!("fixtures/host-policy-configuration-v1.json")).unwrap();
    assert_eq!(r.before.profile.digest().unwrap(), old["digest"]);
    assert!(r.reasons(&t).unwrap().is_empty());
    assert!(json!(r).get("approved").is_none());
    for field in ["started_at_unix_ms", "finished_at_unix_ms", "token_revoked"] {
        let mut v = json!(r);
        v[field] = if field == "token_revoked" {
            json!(false)
        } else {
            json!(v[field].as_i64().unwrap() + 1)
        };
        let changed: Inspection = serde_json::from_value(v).unwrap();
        assert_ne!(changed.digest(&t).unwrap(), r.digest(&t).unwrap());
    }
}
#[test]
fn explicit_absence_resolves_only_missing_legacy_fields() {
    let (t, mut r, _) = fixture();
    let c = &mut r.before;
    protection(c)
        .as_object_mut()
        .unwrap()
        .remove("restrictions");
    reviews(c);
    assert!(
        !c.profile
            .analyze()
            .unwrap()
            .configuration_reasons
            .is_empty()
    );
    assert!(c.analyze(&t).unwrap().configuration_reasons.is_empty());
    // Empty dismissal object is consistent with disabled restrictions.
    protection(c)["required_pull_request_reviews"]["dismissal_restrictions"] =
        json!({"users":[],"teams":[],"apps":[]});
    assert!(c.analyze(&t).unwrap().configuration_reasons.is_empty());
    protection(c)["restrictions"] = json!({"users":[],"teams":[],"apps":[]});
    assert!(c.analyze(&t).unwrap().configuration_reasons.is_empty());
    for bad in [json!(false), json!({"users":[],"teams":[]}), Value::Null] {
        protection(c)["required_pull_request_reviews"]["bypass_pull_request_allowances"] = bad;
        assert!(!c.analyze(&t).unwrap().configuration_reasons.is_empty());
    }
}
#[test]
fn conflicting_sources_do_not_discharge_missing_proof() {
    let (t, r, _) = fixture();
    for case in [
        "flag",
        "contexts",
        "strict",
        "review-count",
        "actors",
        "null-enabled",
    ] {
        let mut c = *r.before.clone();
        reviews(&mut c);
        protection(&mut c)
            .as_object_mut()
            .unwrap()
            .remove("restrictions");
        match case {
            "flag" => {
                legacy(&mut c).flags.insert("allowsDeletions".into(), true);
            }
            "contexts" => legacy(&mut c).required_contexts = Some(vec!["Other check".into()]),
            "strict" => {
                legacy(&mut c)
                    .flags
                    .insert("requiresStrictStatusChecks".into(), false);
            }
            "review-count" => legacy(&mut c).approving_reviews = Some(2),
            "actors" => {
                protection(&mut c)["restrictions"] =
                    json!({"users":[{"id":1}],"teams":[],"apps":[]})
            }
            _ => {
                protection(&mut c)["restrictions"] = Value::Null;
                legacy(&mut c).flags.insert("restrictsPushes".into(), true);
            }
        }
        let a = c.analyze(&t).unwrap();
        assert!(
            a.configuration_reasons
                .contains("legacy_rest_graphql_disagree"),
            "{case}: {a:?}"
        );
        assert!(
            a.configuration_reasons
                .contains("legacy/reviews/bypass/object_unproven")
        );
    }
}
#[test]
fn visibility_and_deployments_remain_explicit_and_bind_configuration_identity() {
    let (t, r, _) = fixture();
    for force in [false, true] {
        let mut c = *r.before.clone();
        let l = legacy(&mut c);
        let b = if force {
            &mut l.bypass_force_pushes
        } else {
            &mut l.bypass_pull_requests
        };
        b.total_count = 1;
        b.nodes.push("Allowance_one".into());
        assert!(
            c.analyze(&t)
                .unwrap()
                .configuration_reasons
                .contains(if force {
                    "legacy_force_push_bypass_present"
                } else {
                    "legacy_pr_bypass_present"
                })
        );
        assert_ne!(c.digest(&t).unwrap(), r.before.digest(&t).unwrap());
    }
    let mut c = *r.before.clone();
    legacy(&mut c)
        .flags
        .insert("requiresDeployments".into(), true);
    let a = c.analyze(&t).unwrap();
    assert!(a.candidate_requirements.contains("required_deployments"));
    assert!(
        a.configuration_reasons
            .contains("legacy_deployment_environments_unavailable")
    );
    legacy(&mut c).deployment_environments = Some(vec!["staging".into()]);
    assert!(
        c.analyze(&t)
            .unwrap()
            .candidate_requirements
            .contains("required_deployments")
    );
    c.visibility.owner_kind = "User".into();
    assert!(
        c.analyze(&t)
            .unwrap()
            .configuration_reasons
            .contains("personal_repository_queue_unsupported")
    );
    c.visibility.legacy = None;
    assert!(
        c.analyze(&t)
            .unwrap()
            .configuration_reasons
            .contains("legacy_visibility_unavailable")
    );
}
#[test]
fn incomplete_malformed_and_cross_repository_evidence_is_rejected() {
    let (t, r, _) = fixture();
    for case in [
        "flag",
        "extra-flag",
        "page",
        "count",
        "duplicate",
        "rule-repository",
        "target",
        "queue",
        "oversize",
        "version",
    ] {
        let mut c = *r.before.clone();
        match case {
            "flag" => {
                legacy(&mut c).flags.remove("requiresDeployments");
            }
            "extra-flag" => {
                legacy(&mut c).flags.insert("futureFlag".into(), false);
            }
            "page" => legacy(&mut c).bypass_pull_requests.has_next_page = true,
            "count" => legacy(&mut c).bypass_pull_requests.total_count = 1,
            "duplicate" => {
                let b = &mut legacy(&mut c).bypass_force_pushes;
                b.total_count = 2;
                b.nodes = vec!["A_one".into(); 2];
            }
            "rule-repository" => legacy(&mut c).repository_id = 12,
            "target" => c.visibility.target.base_ref = "other".into(),
            "queue" => c.visibility.queue_id = None,
            "oversize" => legacy(&mut c).deployment_environments = Some(vec!["x".repeat(257)]),
            _ => c.version = 2,
        }
        assert!(c.validate(&t).is_err(), "{case}");
    }
    for delta in [-1, 120001] {
        let mut changed = r.clone();
        changed.finished_at_unix_ms = changed.started_at_unix_ms + delta;
        assert!(changed.validate(&t).is_err());
    }
    let mut v = json!(r);
    v["approved"] = json!(true);
    assert!(serde_json::from_value::<Inspection>(v).is_err());
}
#[test]
fn drift_revocation_and_order_are_distinct() {
    let (t, mut r, _) = fixture();
    legacy(&mut r.before).deployment_environments = Some(vec!["staging".into(), "prod".into()]);
    r.after = r.before.clone();
    legacy(&mut r.after)
        .deployment_environments
        .as_mut()
        .unwrap()
        .reverse();
    assert!(!r.reasons(&t).unwrap().contains("configuration_changed"));
    legacy(&mut r.after).id = "BPR_replaced".into();
    r.token_revoked = false;
    let reasons = r.reasons(&t).unwrap();
    assert!(reasons.contains("configuration_changed"));
    assert!(reasons.contains("token_revocation_unproven"));
}

#[test]
fn combined_report_bound_is_not_two_independent_profile_budgets() {
    let (t, mut r, _) = fixture();
    protection(&mut r.before)["url"] = json!("x".repeat(26000));
    r.after = r.before.clone();
    r.before.validate(&t).unwrap();
    r.after.validate(&t).unwrap();
    assert!(r.validate(&t).is_err());
    protection(&mut r.before)["url"] = json!("x".repeat(MAX_BYTES));
    assert!(r.before.validate(&t).is_err());
}

fn missing_reviews_with_extra_rule() -> (Target, Configuration) {
    let (t, r, _) = fixture();
    let mut c = *r.before;
    protection(&mut c)
        .as_object_mut()
        .unwrap()
        .remove("required_pull_request_reviews");
    legacy(&mut c).approving_reviews = None;
    let rule = json!({"type":"pull_request","parameters":{
        "required_approving_review_count":0,"require_code_owner_review":false,
        "require_last_push_approval":false,"dismiss_stale_reviews_on_push":true,
        "required_review_thread_resolution":false,"required_reviewers":[],
        "require_extra_approval_for_unattributed_changes":true}});
    let mut effective = rule.clone();
    effective["ruleset_id"] = json!(42);
    effective["ruleset_source"] = json!("example/repo");
    effective["ruleset_source_type"] = json!("Repository");
    c.profile.rules.push(effective);
    c.profile.rulesets[0].document.value.as_mut().unwrap()["rules"]
        .as_array_mut()
        .unwrap()
        .push(rule);
    (t, c)
}

#[test]
fn explicit_supplementary_review_absence_preserves_raw_identity_and_thread_requirement() {
    let (t, mut c) = missing_reviews_with_extra_rule();
    protection(&mut c)["required_conversation_resolution"]["enabled"] = json!(true);
    legacy(&mut c)
        .flags
        .insert("requiresConversationResolution".into(), true);
    let bytes = c.canonical(&t).unwrap();
    let digest = c.digest(&t).unwrap();
    let profile = c.profile.analyze().unwrap();
    assert!(
        profile
            .configuration_reasons
            .contains("legacy/reviews/unavailable")
    );
    assert!(
        profile
            .candidate_requirements
            .iter()
            .any(|r| r.ends_with("/extra_approval_for_unattributed_changes"))
    );
    let a = c.analyze(&t).unwrap();
    assert!(a.configuration_reasons.is_empty(), "{a:?}");
    assert!(a.candidate_requirements.is_empty(), "{a:?}");
    assert!(
        a.reviews
            .iter()
            .any(|r| r.source == "legacy/reviews" && r.approvals == 0 && r.resolve_threads)
    );
    assert_eq!(c.canonical(&t).unwrap(), bytes);
    assert_eq!(c.digest(&t).unwrap(), digest);
    assert!(
        protection(&mut c)
            .get("required_pull_request_reviews")
            .is_none()
    );
}

#[test]
fn supplementary_review_absence_does_not_override_constraints_or_disagreement() {
    let (t, baseline) = missing_reviews_with_extra_rule();
    for case in [
        "approvals",
        "count",
        "code-owner",
        "last-push",
        "stale",
        "dismissal",
        "bypass",
        "other-field",
    ] {
        let mut c = baseline.clone();
        match case {
            "approvals" => {
                legacy(&mut c)
                    .flags
                    .insert("requiresApprovingReviews".into(), true);
            }
            "count" => legacy(&mut c).approving_reviews = Some(1),
            "code-owner" => {
                legacy(&mut c)
                    .flags
                    .insert("requiresCodeOwnerReviews".into(), true);
            }
            "last-push" => {
                legacy(&mut c)
                    .flags
                    .insert("requireLastPushApproval".into(), true);
            }
            "stale" => {
                legacy(&mut c)
                    .flags
                    .insert("dismissesStaleReviews".into(), true);
            }
            "dismissal" => {
                legacy(&mut c)
                    .flags
                    .insert("restrictsReviewDismissals".into(), true);
            }
            "bypass" => {
                legacy(&mut c).bypass_pull_requests.total_count = 1;
                legacy(&mut c).bypass_pull_requests.nodes = vec!["Allowance_one".into()];
            }
            _ => {
                legacy(&mut c).flags.insert("allowsDeletions".into(), true);
            }
        }
        let a = c.analyze(&t).unwrap();
        assert!(
            a.configuration_reasons
                .contains("legacy/reviews/unavailable"),
            "{case}: {a:?}"
        );
        assert!(
            a.candidate_requirements
                .iter()
                .any(|r| r.ends_with("/extra_approval_for_unattributed_changes")),
            "{case}: {a:?}"
        );
    }
}

#[test]
fn supplementary_absence_never_replaces_malformed_rest_or_unknown_source_approvals() {
    let (t, baseline) = missing_reviews_with_extra_rule();
    for value in [
        json!(false),
        json!({}),
        json!({"required_approving_review_count":null}),
    ] {
        let mut c = baseline.clone();
        protection(&mut c)["required_pull_request_reviews"] = value;
        let a = c.analyze(&t).unwrap();
        assert!(!a.configuration_reasons.is_empty());
        assert!(
            a.candidate_requirements
                .iter()
                .any(|r| r.ends_with("/extra_approval_for_unattributed_changes"))
        );
    }
    for case in ["positive", "missing", "source-unavailable"] {
        let mut c = baseline.clone();
        match case {
            "positive" => {
                c.profile.rules[1]["parameters"]["required_approving_review_count"] = json!(1);
                c.profile.rulesets[0].document.value.as_mut().unwrap()["rules"][1]["parameters"]
                    ["required_approving_review_count"] = json!(1);
            }
            "missing" => {
                c.profile.rules[1]["parameters"]
                    .as_object_mut()
                    .unwrap()
                    .remove("required_approving_review_count");
                c.profile.rulesets[0].document.value.as_mut().unwrap()["rules"][1]["parameters"]
                    .as_object_mut()
                    .unwrap()
                    .remove("required_approving_review_count");
            }
            _ => {
                c.profile.rulesets[0].document.status = 404;
                c.profile.rulesets[0].document.value = None;
            }
        }
        let a = c.analyze(&t).unwrap();
        assert!(
            !a.configuration_reasons
                .contains("legacy/reviews/unavailable")
        );
        assert!(
            a.candidate_requirements
                .iter()
                .any(|r| r.ends_with("/extra_approval_for_unattributed_changes")),
            "{case}: {a:?}"
        );
    }
}
