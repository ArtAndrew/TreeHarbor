use serde_json::{Value, json};
use treeharbor_github_contract::{dequeue::*, queue::*};
use uuid::Uuid;
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap()
}
fn vectors() -> Value {
    serde_json::from_str(include_str!("fixtures/dequeue-v1.json")).unwrap()
}
fn request() -> Uuid {
    vectors()["request_id"].as_str().unwrap().parse().unwrap()
}
fn target() -> QueueTarget {
    let f = fixture();
    QueueTarget {
        pull: serde_json::from_value(f["target"].clone()).unwrap(),
        expected_base_revision: "git:sha1:".to_owned() + &"22".repeat(20),
        expected_rules: serde_json::from_value(f["report"]["rules_before"].clone()).unwrap(),
    }
}
fn snapshot() -> QueueSnapshot {
    QueueSnapshot {
        pull: serde_json::from_value(fixture()["report"]["before"].clone()).unwrap(),
        queue_enabled: true,
        configuration: Some(QueueConfiguration {
            id: "MQ_one".into(),
            merge_method: "SQUASH".into(),
            merging_strategy: "ALLGREEN".into(),
            maximum_entries_to_build: 5,
            maximum_entries_to_merge: 5,
            minimum_entries_to_merge: 1,
            minimum_entries_wait_minutes: 0,
            check_timeout_minutes: 60,
        }),
        entry: Some(QueueEntryObservation {
            id: "MQE_one".into(),
            state: "QUEUED".into(),
            base: None,
            head: None,
        }),
        final_commit: None,
    }
}
fn observation() -> DequeueObservation {
    let mut receipt: DequeueReceipt = serde_json::from_value(vectors()["receipt"].clone()).unwrap();
    receipt.pull_node = target().pull.pull_node;
    DequeueObservation {
        version: 1,
        started_at_unix_ms: 1,
        finished_at_unix_ms: 2,
        before: Some(snapshot()),
        receipt: Some(receipt),
        after: None,
        mutation_not_sent: false,
        token_revoked: true,
    }
}
#[test]
fn dequeue_requires_settled_enqueue_and_never_turns_ack_into_removal_or_merge() {
    let t = target();
    let mut o = observation();
    o.validate(&t, request()).unwrap();
    assert_eq!(o.state(&t, request(), true), "dequeue_acknowledged");
    assert!(!o.changed_during_dequeue());
    let mut after = snapshot();
    after.entry = None;
    o.after = Some(after.clone());
    assert_eq!(o.state(&t, request(), false), "enqueue_unresolved");
    assert_eq!(o.state(&t, request(), true), "removal_observed");
    after.pull.merged = true;
    after.pull.state = "closed".into();
    after.final_commit = Some(GitCommitObservation {
        revision: "git:sha1:".to_owned() + &"33".repeat(20),
        tree: "git:sha1:".to_owned() + &"44".repeat(20),
    });
    o.after = Some(after.clone());
    assert_eq!(o.state(&t, request(), true), "host_merged_unverified");
    after.pull.head_revision = "git:sha1:".to_owned() + &"55".repeat(20);
    o.after = Some(after);
    assert_eq!(o.state(&t, request(), true), "other_head_merged");
    o.after = Some(snapshot());
    assert_eq!(o.state(&t, request(), true), "host_queued");
}
#[test]
fn dequeue_retains_pr_scoped_head_and_entry_races_but_rejects_wrong_identity() {
    let t = target();
    let mut o = observation();
    o.receipt.as_mut().unwrap().head_revision = "git:sha1:".to_owned() + &"77".repeat(20);
    o.receipt.as_mut().unwrap().entry_id = "MQE_replaced".into();
    o.validate(&t, request()).unwrap();
    assert!(o.changed_during_dequeue());
    for field in [
        "request",
        "repository",
        "pull",
        "number",
        "entry",
        "time",
        "before",
        "not-sent",
    ] {
        let mut o = observation();
        match field {
            "request" => o.receipt.as_mut().unwrap().request_id = Uuid::nil().to_string(),
            "repository" => o.receipt.as_mut().unwrap().repository_id += 1,
            "pull" => o.receipt.as_mut().unwrap().pull_node.push('x'),
            "number" => o.receipt.as_mut().unwrap().pull_number += 1,
            "entry" => o.receipt.as_mut().unwrap().entry_id = "../bad".into(),
            "time" => o.finished_at_unix_ms = 190_000,
            "not-sent" => o.mutation_not_sent = true,
            _ => o.before = None,
        }
        assert!(o.validate(&t, request()).is_err(), "{field}");
        assert_eq!(o.state(&t, request(), true), "uncertain");
    }
    let mut v = json!(observation());
    v["enqueue_settled"] = json!(true);
    assert!(serde_json::from_value::<DequeueObservation>(v).is_err());
}
#[test]
fn dequeue_receipt_vector_roundtrips_without_implicit_idempotency_or_permission() {
    let v = vectors();
    assert_eq!(v["version"], 1);
    let r: DequeueReceipt = serde_json::from_value(v["receipt"].clone()).unwrap();
    assert_eq!(json!(r), v["receipt"]);
    let mut t = target();
    t.pull.pull_node = r.pull_node.clone();
    r.validate(&t, request()).unwrap();
    assert!(r.validate(&t, Uuid::new_v4()).is_err());
    let o: DequeueObservation = serde_json::from_value(v["observation"].clone()).unwrap();
    o.validate(&t, request()).unwrap();
    assert_eq!(json!(o), v["observation"]);
    assert_eq!(o.state(&t, request(), true), "not_sent");
}
