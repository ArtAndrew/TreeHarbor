use serde_json::{Value, json};
use treeharbor_github_contract::{check_dispatch::PolicyProof, check_publication as cp};
use uuid::Uuid;

fn fixture() -> (PolicyProof, Value) {
    // Generated with Python stdlib sorted compact JSON + hashlib, independently
    // of the Rust implementation. No database or provider trust is simulated here.
    let v: Value =
        serde_json::from_str(include_str!("fixtures/check-dispatch-policy-v1.json")).unwrap();
    (serde_json::from_value(v["proof"].clone()).unwrap(), v)
}

#[test]
fn independent_policy_proof_has_stable_bytes_and_no_merge_claim() {
    let (p, v) = fixture();
    assert_eq!(
        p.canonical().unwrap(),
        v["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(p.digest().unwrap(), v["digest"]);
    for field in ["admission_verified", "merge_verified", "dispatch_granted"] {
        assert!(v["proof"].get(field).is_none());
        let mut bad = v["proof"].clone();
        bad[field] = json!(true);
        assert!(serde_json::from_value::<PolicyProof>(bad).is_err());
    }
}

#[test]
fn every_decision_identity_changes_the_independent_digest() {
    let (p, v) = fixture();
    let changes = [
        ("tenant", json!("another-team")),
        ("repository", json!(Uuid::from_u128(1))),
        ("queue", json!(Uuid::from_u128(2))),
        ("check_run", json!(Uuid::from_u128(3))),
        ("execution_run", json!(Uuid::from_u128(4))),
        ("collection_key", json!("another-collection")),
        ("collected_at_unix_ms", json!(1000001)),
        ("decided_at_unix_ms", json!(1003001)),
        ("host_policy_sequence", json!(10)),
        (
            "host_policy_digest",
            json!(format!("sha256:{}", "aa".repeat(32))),
        ),
        (
            "configuration_digest",
            json!(format!("sha256:{}", "bb".repeat(32))),
        ),
        (
            "preflight_digest",
            json!(format!("sha256:{}", "cc".repeat(32))),
        ),
        ("capture_started_at_unix_ms", json!(1001001)),
        ("capture_finished_at_unix_ms", json!(1002001)),
    ];
    for (field, value) in changes {
        let mut changed = v["proof"].clone();
        changed[field] = value;
        let changed: PolicyProof = serde_json::from_value(changed).unwrap();
        assert_ne!(changed.digest().unwrap(), p.digest().unwrap(), "{field}");
    }
    let mut changed = p.clone();
    changed.target.admission.as_mut().unwrap().generation += 1;
    assert_ne!(changed.digest().unwrap(), p.digest().unwrap());
    changed = p.clone();
    changed.target.evidence = format!("sha256:{}", "dd".repeat(32));
    assert_ne!(changed.digest().unwrap(), p.digest().unwrap());
}

#[test]
fn legacy_pull_failure_and_ill_ordered_or_stale_captures_are_not_policy_proofs() {
    let (p, _) = fixture();
    for case in [
        "version",
        "legacy",
        "pull",
        "failure",
        "nil",
        "tenant",
        "key",
        "key-long",
        "sequence",
        "digest",
        "before-collection",
        "future-collection",
        "negative",
        "reversed",
        "old",
        "future",
        "long",
    ] {
        let mut bad = p.clone();
        match case {
            "version" => bad.version = 2,
            "legacy" => {
                bad.target.version = 1;
                bad.target.admission = None;
            }
            "pull" => {
                bad.target.source = cp::Source::Pull {
                    target: p.target.source.pull().clone(),
                }
            }
            "failure" => bad.target.conclusion = cp::Conclusion::Failure,
            "nil" => bad.execution_run = Uuid::nil(),
            "tenant" => bad.tenant = "../other".into(),
            "key" => bad.collection_key = "secret\n".into(),
            "key-long" => bad.collection_key = "x".repeat(129),
            "sequence" => bad.host_policy_sequence = 4097,
            "digest" => bad.configuration_digest = "sha256:abc".into(),
            "before-collection" => bad.capture_started_at_unix_ms = 999999,
            "future-collection" => bad.collected_at_unix_ms = bad.decided_at_unix_ms + 1,
            "negative" => bad.collected_at_unix_ms = 0,
            "reversed" => bad.capture_finished_at_unix_ms = bad.capture_started_at_unix_ms - 1,
            "old" => bad.decided_at_unix_ms = bad.capture_started_at_unix_ms + 120001,
            "future" => bad.capture_finished_at_unix_ms = bad.decided_at_unix_ms + 15001,
            _ => {
                bad.capture_finished_at_unix_ms = bad.capture_started_at_unix_ms + 120001;
                bad.decided_at_unix_ms = bad.capture_finished_at_unix_ms;
            }
        }
        assert!(bad.validate().is_err(), "{case}");
        assert!(bad.digest().is_err(), "{case}");
    }
}

#[test]
fn capture_window_boundaries_are_inclusive_without_overflow() {
    let (mut p, _) = fixture();
    p.capture_started_at_unix_ms = p.collected_at_unix_ms;
    p.decided_at_unix_ms = p.capture_started_at_unix_ms + 120_000;
    p.capture_finished_at_unix_ms = p.decided_at_unix_ms;
    p.validate().unwrap();
    p.decided_at_unix_ms += 1;
    assert!(p.validate().is_err());

    let (mut p, _) = fixture();
    p.capture_finished_at_unix_ms = p.decided_at_unix_ms + 15_000;
    p.validate().unwrap();
    p.capture_finished_at_unix_ms += 1;
    assert!(p.validate().is_err());

    let (mut p, _) = fixture();
    p.collected_at_unix_ms = i64::MAX - 1;
    p.capture_started_at_unix_ms = i64::MAX - 1;
    p.capture_finished_at_unix_ms = i64::MAX;
    p.decided_at_unix_ms = i64::MAX;
    p.validate().unwrap();
}
