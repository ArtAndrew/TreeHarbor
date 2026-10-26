use serde_json::{Value, json};
use treeharbor_github_contract::{
    host_policy::Report,
    host_policy_evaluation::evaluate,
    queue::{QueueObservation, QueueTarget},
};
fn fixture() -> (QueueTarget, Report, Value) {
    let v: Value = serde_json::from_str(include_str!("fixtures/host-policy-v2.json")).unwrap();
    (
        serde_json::from_value(v["queue_target"].clone()).unwrap(),
        serde_json::from_value(v["report"].clone()).unwrap(),
        v,
    )
}
#[test]
fn independent_v2_vector_binds_candidate_and_full_configuration_in_each_round() {
    let (t, r, v) = fixture();
    r.validate(&t).unwrap();
    assert_eq!(
        serde_json::to_vec(&json!(r)).unwrap(),
        v["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(r.digest(&t).unwrap(), v["digest"]);
    assert!(r.capture_reasons(&t).unwrap().is_empty());
    let e = evaluate(&r, &t).unwrap();
    assert_eq!(e.version, 2);
    assert_eq!(e.before_digest, v["configuration_digest"]);
    assert_eq!(e.after_digest, e.before_digest);
    assert!(e.reasons.is_empty(), "{:?}", e.reasons);
    assert!(json!(e).get("host_policy_verified").is_none());
}
#[test]
fn visibility_cannot_cross_versions_repositories_branches_or_queues() {
    let (t, r, _) = fixture();
    for case in [
        "v1",
        "missing",
        "repository",
        "name",
        "base",
        "queue",
        "flag",
        "partial",
    ] {
        let mut bad = r.clone();
        match case {
            "v1" => bad.version = 1,
            "missing" => bad.after.visibility = None,
            "repository" => bad.before.visibility.as_mut().unwrap().target.repository_id = 12,
            "name" => {
                bad.before
                    .visibility
                    .as_mut()
                    .unwrap()
                    .target
                    .repository_name = "other/repo".into()
            }
            "base" => bad.before.visibility.as_mut().unwrap().target.base_ref = "release".into(),
            "queue" => bad.before.visibility.as_mut().unwrap().queue_id = Some("MQ_changed".into()),
            "flag" => {
                bad.before
                    .visibility
                    .as_mut()
                    .unwrap()
                    .legacy
                    .as_mut()
                    .unwrap()
                    .flags
                    .remove("restrictsPushes");
            }
            _ => {
                bad.before
                    .visibility
                    .as_mut()
                    .unwrap()
                    .legacy
                    .as_mut()
                    .unwrap()
                    .bypass_pull_requests
                    .has_next_page = true
            }
        }
        assert!(bad.validate(&t).is_err(), "{case}");
    }
    let envelope = json!({"version":10,"started_at_unix_ms":1000,"finished_at_unix_ms":2000,
        "snapshot":null,"receipt":null,"async_result":null,"host_policy":r,"token_revoked":true});
    let p: QueueObservation = serde_json::from_value(envelope.clone()).unwrap();
    p.validate(&t).unwrap();
    let mut bad = envelope;
    bad["version"] = json!(9);
    assert!(
        serde_json::from_value::<QueueObservation>(bad)
            .unwrap()
            .validate(&t)
            .is_err()
    );
}
#[test]
fn changed_visibility_and_review_head_never_reuse_the_approved_configuration() {
    let (t, r, _) = fixture();
    for case in ["replacement", "bypass", "settings", "review", "head"] {
        let mut changed = r.clone();
        match case {
            "replacement" => {
                changed
                    .after
                    .visibility
                    .as_mut()
                    .unwrap()
                    .legacy
                    .as_mut()
                    .unwrap()
                    .id = "BPR_replaced".into()
            }
            "bypass" => {
                let b = &mut changed
                    .after
                    .visibility
                    .as_mut()
                    .unwrap()
                    .legacy
                    .as_mut()
                    .unwrap()
                    .bypass_pull_requests;
                b.total_count = 1;
                b.nodes.push("BPA_unexpected".into());
            }
            "settings" => {
                changed
                    .after
                    .group
                    .target
                    .configuration
                    .as_mut()
                    .unwrap()
                    .check_timeout_minutes += 1
            }
            "review" => changed.after.review.decision = Some("CHANGES_REQUESTED".into()),
            _ => changed.after.review.pull.head_revision = format!("git:sha1:{}", "aa".repeat(20)),
        }
        assert_ne!(changed.digest(&t).unwrap(), r.digest(&t).unwrap());
        let e = evaluate(&changed, &t).unwrap();
        assert!(!e.reasons.is_empty(), "{case}");
        assert!(e.reasons.contains("capture_changed"));
    }
}
