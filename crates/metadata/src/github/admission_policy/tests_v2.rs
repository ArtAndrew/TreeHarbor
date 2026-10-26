use super::*;
use treeharbor_github_contract::host_configuration as hc;
fn fixture() -> (AdmissionPolicy, Value) {
    let v: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/connected-admission-policy-v2.json"
    ))
    .unwrap();
    (serde_json::from_value(v["policy"].clone()).unwrap(), v)
}
fn inspection(c: &hc::Configuration) -> hc::Inspection {
    hc::Inspection {
        version: 1,
        started_at_unix_ms: 1792990000000,
        finished_at_unix_ms: 1792990001000,
        before: Box::new(c.clone()),
        after: Box::new(c.clone()),
        token_revoked: true,
    }
}
#[test]
fn independent_v2_vector_and_repository_required_set_are_exact() {
    let (p, v) = fixture();
    assert_eq!(
        p.canonical().unwrap(),
        v["canonical"].as_str().unwrap().as_bytes()
    );
    assert_eq!(p.digest().unwrap().to_string(), v["digest"]);
    AdmissionPolicy::decode(&p.canonical().unwrap(), p.digest().unwrap()).unwrap();
    let required = p
        .required(7, 1, Uuid::from_bytes([1; 16]), "main", 1)
        .unwrap();
    assert_eq!(json!(required), json!([{"name":"CI / build","app_id":7}]));
    let mut b = Upstream {
        installation_id: 9,
        repository_id: 11,
        owner_id: 13,
        owner: "example-org".into(),
        name: "demo".into(),
    };
    p.bound(&b, "main").unwrap();
    assert!(p.bound(&b, "release").is_err());
    b.repository_id = 12;
    assert!(p.bound(&b, "main").is_err());
    b.repository_id = 11;
    b.name = "foreign".into();
    assert!(p.bound(&b, "main").is_err());
    for key in ["name", "app_id"] {
        let mut changed = p.clone();
        if key == "name" {
            changed.workflows[0].required.name = "other".into();
        } else {
            changed.workflows[0].required.app_id = 8;
        }
        assert!(
            changed
                .required(7, 1, Uuid::from_bytes([1; 16]), "main", 1)
                .is_err()
        );
    }
}
#[test]
fn v2_rejects_mismatched_rules_unsupported_configuration_and_implicit_upgrade() {
    let (p, _) = fixture();
    let mut bad = p.clone();
    bad.version = 1;
    assert!(bad.canonical().is_err());
    let mut bad = p.clone();
    bad.host_configuration = None;
    assert!(bad.canonical().is_err());
    let mut bad = p.clone();
    bad.rules.sha256 = "ab".repeat(32);
    assert!(bad.canonical().is_err());
    let mut bad = p.clone();
    bad.host_configuration.as_mut().unwrap().profile.queue = None;
    assert!(bad.canonical().is_err());
    let mut bad = p.clone();
    bad.host_configuration
        .as_mut()
        .unwrap()
        .profile
        .protection
        .value
        .as_mut()
        .unwrap()["new_policy_clause"] = json!(true);
    assert!(bad.canonical().is_err());
    let mut bad = p.clone();
    bad.host_configuration.as_mut().unwrap().visibility.legacy = None;
    assert!(bad.canonical().is_err());
    let mut changed = p.clone();
    changed
        .host_configuration
        .as_mut()
        .unwrap()
        .visibility
        .legacy
        .as_mut()
        .unwrap()
        .id = "BPR_replaced".into();
    assert_ne!(p.digest().unwrap(), changed.digest().unwrap());
}
#[test]
fn draft_recomputes_rules_requires_agreed_evidence_and_does_not_claim_approval() {
    let (p, _) = fixture();
    let r = inspection(p.host_configuration.as_ref().unwrap());
    let mut proposal = p.clone();
    proposal.version = 1;
    proposal.host_configuration = None;
    proposal.rules.sha256 = "ab".repeat(32);
    let draft = proposal.with_inspection(&r).unwrap();
    assert_eq!(draft.digest().unwrap(), p.digest().unwrap());
    assert!(json!(draft).get("approved").is_none());
    for case in ["drift", "revocation", "time", "identity"] {
        let mut r = r.clone();
        match case {
            "drift" => r.after.visibility.legacy.as_mut().unwrap().id = "BPR_other".into(),
            "revocation" => r.token_revoked = false,
            "time" => r.finished_at_unix_ms += 120001,
            _ => r.after.visibility.target.repository_id = 12,
        }
        assert!(proposal.with_inspection(&r).is_err(), "{case}");
    }
}
#[test]
fn managed_names_do_not_recurse_through_configuration_hash_but_require_exact_generation() {
    let old: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/connected-admission-policy-v1.json"
    ))
    .unwrap();
    let proposal: AdmissionPolicy = serde_json::from_value(old["policy"].clone()).unwrap();
    let binding = Uuid::from_bytes([1; 16]);
    let expected = proposal.required(7, 1, binding, "main", 1).unwrap();
    let (p, _) = fixture();
    let mut r = inspection(p.host_configuration.as_ref().unwrap());
    for c in [&mut r.before, &mut r.after] {
        let contexts: Vec<_> = expected.iter().map(|r| r.name.clone()).collect();
        let checks: Vec<_> = expected
            .iter()
            .map(|r| json!({"context":r.name,"app_id":r.app_id}))
            .collect();
        let status = &mut c.profile.protection.value.as_mut().unwrap()["required_status_checks"];
        status["contexts"] = json!(contexts);
        status["checks"] = json!(checks);
        c.visibility.legacy.as_mut().unwrap().required_contexts = Some(contexts);
    }
    let mut draft = proposal.with_inspection(&r).unwrap();
    assert_eq!(draft.required(7, 1, binding, "main", 1).unwrap(), expected);
    let id = draft.digest().unwrap();
    draft.managed[0]
        .validators
        .insert("another-validator".into());
    assert_ne!(draft.digest().unwrap(), id);
    assert_eq!(draft.required(7, 1, binding, "main", 1).unwrap(), expected);
    assert!(draft.required(7, 1, binding, "main", 2).is_err());
    assert!(draft.required(8, 1, binding, "main", 1).is_err());
    assert!(
        draft
            .required(7, 1, Uuid::from_bytes([2; 16]), "main", 1)
            .is_err()
    );
    assert!(draft.required(7, 1, binding, "other", 1).is_err());
}
