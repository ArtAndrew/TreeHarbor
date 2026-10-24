use treeharbor_github_contract::deliveries::DeliveryAttempt;

fn fixture() -> DeliveryAttempt {
    serde_json::from_slice(include_bytes!("fixtures/delivery-attempt-v1.json")).unwrap()
}
#[test]
fn delivery_attempt_wire_identity_and_status_are_separate() {
    let a = fixture();
    a.validate(9, 11, 1609459201000).unwrap();
    let v: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/delivery-attempt-v1.json")).unwrap();
    assert_eq!(serde_json::to_value(&a).unwrap(), v);
    let mut status = a.clone();
    status.status_code = Some(200);
    status.throttled_at_unix_ms = Some(1609459201000);
    assert!(a.same_attempt(&status)); // Provider status never establishes inbox receipt.
    for change in 0..5 {
        let mut b = a.clone();
        match change {
            0 => b.id += 1,
            1 => b.guid = uuid::Uuid::new_v4(),
            2 => b.repository_id += 1,
            3 => b.action = "destroyed".into(),
            _ => b.delivered_at_unix_ms += 1,
        }
        assert!(!a.same_attempt(&b));
    }
    assert!(a.validate(10, 11, 1609459201000).is_err());
    assert!(a.validate(9, 12, 1609459201000).is_err());
    assert!(a.validate(9, 11, 1609459185000).is_err());
    assert!(a.validate(9, 11, 0).is_err());
    assert!(a.validate(0, 11, 1609459201000).is_err());
    let mut b = a.clone();
    b.guid = uuid::Uuid::nil();
    assert!(b.validate(9, 11, 1609459201000).is_err());
    let mut b = a.clone();
    b.status_code = Some(600);
    assert!(b.validate(9, 11, 1609459201000).is_err());
    let mut b = a.clone();
    b.throttled_at_unix_ms = Some(0);
    assert!(b.validate(9, 11, 1609459201000).is_err());
    let mut extra = v.clone();
    extra["verified_signature"] = true.into();
    assert!(serde_json::from_value::<DeliveryAttempt>(extra).is_err());
    let duplicate = serde_json::to_string(&a)
        .unwrap()
        .replacen('{', "{\"id\":12345678,", 1);
    assert!(serde_json::from_str::<DeliveryAttempt>(&duplicate).is_err());
}

#[test]
fn parsed_payload_digest_matches_independent_python_vector_and_preserves_array_order() {
    use treeharbor_github_contract::deliveries::payload_digest;
    let v: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/payload-json-v1.json")).unwrap();
    assert_eq!(
        payload_digest(&v["payload"]).unwrap(),
        v["sha256"].as_str().unwrap()
    );
    let same: serde_json::Value =
        serde_json::from_str(v["canonical_utf8"].as_str().unwrap()).unwrap();
    assert_eq!(
        payload_digest(&same).unwrap(),
        v["sha256"].as_str().unwrap()
    );
    let mut changed = same.clone();
    changed["z"].as_array_mut().unwrap().reverse();
    assert_ne!(
        payload_digest(&changed).unwrap(),
        v["sha256"].as_str().unwrap()
    );
    assert!(payload_digest(&serde_json::json!([])).is_err());
    let mut deep = serde_json::json!(null);
    for _ in 0..66 {
        deep = serde_json::json!({"nested":deep});
    }
    assert!(payload_digest(&deep).is_err());
}

#[test]
fn recovery_report_vectors_reject_scope_version_and_ambiguous_progress() {
    use treeharbor_github_contract::deliveries::{DeliveryDetails, DeliveryPageReport};
    let v: serde_json::Value =
        serde_json::from_slice(include_bytes!("fixtures/delivery-recovery-v1.json")).unwrap();
    let page: DeliveryPageReport = serde_json::from_value(v["page"].clone()).unwrap();
    let details: DeliveryDetails = serde_json::from_value(v["details"].clone()).unwrap();
    page.validate(9, 11).unwrap();
    details.validate(9, 11).unwrap();
    assert_eq!(serde_json::to_value(&page).unwrap(), v["page"]);
    assert_eq!(serde_json::to_value(&details).unwrap(), v["details"]);
    assert!(page.validate(9, 12).is_err());
    assert!(details.validate(10, 11).is_err());
    for (field, bad) in [
        ("version", serde_json::json!(2)),
        ("scanned", serde_json::json!(0)),
        ("scanned", serde_json::json!(101)),
        ("cursor", v["page"]["next_cursor"].clone()),
        ("next_cursor", serde_json::json!("\n")),
    ] {
        let mut bad_page = v["page"].clone();
        bad_page[field] = bad;
        assert!(
            serde_json::from_value::<DeliveryPageReport>(bad_page)
                .unwrap()
                .validate(9, 11)
                .is_err()
        );
    }
    let mut duplicate = page.clone();
    duplicate.attempts.push(duplicate.attempts[0].clone());
    assert!(duplicate.validate(9, 11).is_err());
    for digest in [
        details.payload_sha256.to_uppercase(),
        "a".repeat(63),
        "g".repeat(64),
    ] {
        let mut bad = details.clone();
        bad.payload_sha256 = digest;
        assert!(bad.validate(9, 11).is_err());
    }
    let mut extra = v["details"].clone();
    extra["signature_verified"] = serde_json::json!(true);
    assert!(serde_json::from_value::<DeliveryDetails>(extra).is_err());
}
