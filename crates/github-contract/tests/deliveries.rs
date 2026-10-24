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
