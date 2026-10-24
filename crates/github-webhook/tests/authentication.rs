use serde_json::Value;
use treeharbor_github_webhook::{Error, MAX_BODY_BYTES, WebhookEvent, WebhookKey};
use uuid::Uuid;
fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/vectors-v1.json")).unwrap()
}
fn key() -> WebhookKey {
    WebhookKey::new(
        7,
        Uuid::from_bytes([1; 16]),
        vectors()["secret"].as_str().unwrap().as_bytes().to_vec(),
    )
    .unwrap()
}
#[test]
fn independent_python_hmac_vectors_raw_unicode_and_typed_duplicates() {
    let vectors = vectors();
    let key = key();
    for v in vectors["vectors"].as_array().unwrap() {
        let raw = v["body"].as_str().unwrap().as_bytes().to_vec();
        let result = key.authenticate(
            v["event_header"].as_str().unwrap(),
            v["signature"].as_str().unwrap(),
            raw.clone(),
        );
        match v["error"].as_str() {
            None => {
                let delivery = result.unwrap_or_else(|e| panic!("{}: {e}", v["name"]));
                assert_eq!(delivery.body(), raw);
            }
            Some(error) => {
                let e = match result {
                    Err(e) => e,
                    Ok(_) => panic!("unexpected acceptance: {}", v["name"]),
                };
                assert_eq!(format!("{e:?}"), error, "{}", v["name"]);
            }
        }
    }
    let v = &vectors["vectors"][0];
    let delivery = key
        .authenticate(
            "merge_group",
            v["signature"].as_str().unwrap(),
            v["body"].as_str().unwrap().as_bytes().to_vec(),
        )
        .unwrap();
    let WebhookEvent::MergeGroup(fact) = delivery.event() else {
        panic!("wrong event")
    };
    assert_eq!(
        serde_json::to_value(fact).unwrap(),
        serde_json::from_str::<Value>(include_str!("fixtures/checks-normalized.json")).unwrap()
    );
    assert_eq!(delivery.app_id(), 7);
    assert_eq!(delivery.key_id(), Uuid::from_bytes([1; 16]));
}
#[test]
fn altered_bytes_and_key_revocation_fail_before_any_payload_identity() {
    let vectors = vectors();
    let v = &vectors["vectors"][0];
    let key = key();
    let raw = v["body"].as_str().unwrap().as_bytes();
    let sig = v["signature"].as_str().unwrap();
    let mut changed = raw.to_vec();
    changed.push(b' ');
    assert!(matches!(
        key.authenticate("merge_group", sig, changed),
        Err(Error::Signature)
    ));
    assert!(matches!(
        key.authenticate("merge_group", sig, b"not JSON".to_vec()),
        Err(Error::Signature)
    ));
    let revoked = WebhookKey::new(7, Uuid::from_bytes([2; 16]), vec![b'X'; 48]).unwrap();
    assert!(matches!(
        revoked.authenticate("merge_group", sig, raw.to_vec()),
        Err(Error::Signature)
    ));
    assert!(matches!(
        key.authenticate("merge_group", sig, vec![b' '; MAX_BODY_BYTES + 1]),
        Err(Error::Size)
    ));
    assert!(matches!(
        key.authenticate("merge_group", sig, vec![]),
        Err(Error::Size)
    ));
}
#[test]
fn invalid_key_configuration_is_rejected() {
    for (app, id, secret) in [
        (0, Uuid::new_v4(), vec![b'x'; 32]),
        (7, Uuid::nil(), vec![b'x'; 32]),
        (7, Uuid::new_v4(), vec![b'x'; 31]),
        (7, Uuid::new_v4(), vec![b'x'; 257]),
        (7, Uuid::new_v4(), vec![0; 32]),
        (7, Uuid::new_v4(), vec![255; 32]),
    ] {
        assert!(WebhookKey::new(app, id, secret).is_err());
    }
}
