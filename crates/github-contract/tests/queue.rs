use serde_json::{Value, json};

#[test]
fn graphql_receipt_v3_is_exact_and_cannot_be_mixed_with_other_effects() {
    let wire: Value = serde_json::from_str(include_str!("fixtures/queue-enqueue-v3.json")).unwrap();
    let r: QueueObservation = serde_json::from_value(wire.clone()).unwrap();
    r.validate(&target()).unwrap();
    assert_eq!(json!(r), wire);
    assert_eq!(r.state(&target()), "host_enqueued");
    for field in [
        "version",
        "head",
        "repository",
        "pull",
        "number",
        "request",
        "entry",
        "no-send",
        "mixed",
        "snapshot",
    ] {
        let mut v = r.clone();
        let e = v.enqueue_receipt.as_mut().unwrap();
        match field {
            "version" => v.version = 2,
            "head" => e.head_revision = "git:sha1:".to_owned() + &"22".repeat(20),
            "repository" => e.repository_id += 1,
            "pull" => e.pull_node.push('x'),
            "number" => e.pull_number += 1,
            "request" => e.request_id = uuid::Uuid::nil().to_string(),
            "entry" => e.entry_id = "../foreign".into(),
            "no-send" => v.mutation_not_sent = true,
            "snapshot" => v.snapshot = Some(snapshot()),
            _ => v.receipt = Some(pending()),
        }
        assert!(v.validate(&target()).is_err(), "{field}");
    }
    assert!(
        r.enqueue_receipt
            .unwrap()
            .validate(&target(), uuid::Uuid::new_v4())
            .is_err()
    );
}

#[test]
fn no_send_v2_is_positive_evidence_and_old_reports_remain_unknown() {
    let v: Value = serde_json::from_str(include_str!("fixtures/queue-no-send-v2.json")).unwrap();
    let mut r: QueueObservation = serde_json::from_value(v.clone()).unwrap();
    r.validate(&target()).unwrap();
    assert_eq!(json!(r), v);
    assert_eq!(r.state(&target()), "not_sent");
    r.receipt = Some(pending());
    assert!(r.validate(&target()).is_err());
    r.receipt = None;
    r.version = 1;
    assert!(r.validate(&target()).is_err());
    r.mutation_not_sent = false;
    let old = json!(r);
    assert!(old.get("mutation_not_sent").is_none());
    let old: QueueObservation = serde_json::from_value(old).unwrap();
    old.validate(&target()).unwrap();
    assert_eq!(old.state(&target()), "uncertain");
}
use treeharbor_github_contract::{InspectionReport, queue::*};
fn fixture() -> Value {
    serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap()
}
fn target() -> QueueTarget {
    let f = fixture();
    QueueTarget {
        pull: serde_json::from_value(f["target"].clone()).unwrap(),
        expected_base_revision: "git:sha1:".to_owned() + &"22".repeat(20),
        expected_rules: serde_json::from_value(f["report"]["rules_before"].clone()).unwrap(),
    }
}
fn capture() -> InspectionReport {
    serde_json::from_value(fixture()["report"].clone()).unwrap()
}
fn pending() -> QueueReceipt {
    QueueReceipt {
        http_status: 202,
        status: "pending".into(),
        request_uuid: Some("a15d30c6-3af7-4191-9a68-600000000001".into()),
        expected_head_revision: Some(target().pull.head_revision),
        merge_action: Some("merge_queue".into()),
        bypass_rules: Some(false),
        final_revision: None,
    }
}
fn configuration() -> QueueConfiguration {
    QueueConfiguration {
        id: "MQ_test".into(),
        merge_method: "SQUASH".into(),
        merging_strategy: "ALLGREEN".into(),
        maximum_entries_to_build: 5,
        maximum_entries_to_merge: 5,
        minimum_entries_to_merge: 1,
        minimum_entries_wait_minutes: 0,
        check_timeout_minutes: 60,
    }
}
fn snapshot() -> QueueSnapshot {
    QueueSnapshot {
        queue_id: None,
        pull: capture().before,
        queue_enabled: true,
        configuration: Some(configuration()),
        entry: None,
        final_commit: None,
    }
}
#[test]
fn queue_preflight_pins_open_head_base_and_rules_without_promoting_old_checks() {
    let t = target();
    assert!(t.admits_capture(&capture()));
    assert!(snapshot().can_request(&t));
    for field in ["base", "head", "rules", "draft", "closed", "failed"] {
        let mut r = capture();
        match field {
            "base" => {
                r.before.base_revision = "git:sha1:".to_owned() + &"33".repeat(20);
                r.after = r.before.clone();
            }
            "head" => {
                r.before.head_revision = "git:sha1:".to_owned() + &"33".repeat(20);
                r.after = r.before.clone();
            }
            "rules" => {
                r.rules_before.count += 1;
                r.rules_after = r.rules_before.clone();
            }
            "draft" => {
                r.before.draft = true;
                r.after = r.before.clone();
            }
            "closed" => {
                r.before.state = "closed".into();
                r.after = r.before.clone();
            }
            _ => r.checks[0].runs[0].conclusion = Some("failure".into()),
        }
        assert!(!t.admits_capture(&r), "{field}");
    }
    let mut s = snapshot();
    s.configuration = None;
    assert!(!s.can_request(&t));
    s = snapshot();
    s.queue_enabled = false;
    assert!(!s.can_request(&t));
}
#[test]
fn async_receipts_reject_direct_bypass_foreign_head_and_ambiguous_conflicts() {
    let t = target();
    pending().validate(&t).unwrap();
    for field in [
        "direct", "default", "bypass", "head", "uuid", "conflict", "status",
    ] {
        let mut r = pending();
        match field {
            "direct" => r.merge_action = Some("direct_merge".into()),
            "default" => r.merge_action = Some("default".into()),
            "bypass" => r.bypass_rules = Some(true),
            "head" => r.expected_head_revision = Some("git:sha1:".to_owned() + &"33".repeat(20)),
            "uuid" => r.request_uuid = Some("00000000-0000-0000-0000-000000000000".into()),
            "conflict" => {
                r.http_status = 409;
                r.bypass_rules = None;
            }
            _ => r.http_status = 201,
        }
        assert!(r.validate(&t).is_err(), "{field}");
    }
    let mut r = pending();
    r.http_status = 409;
    r.validate(&t).unwrap();
    let v = json!({"http_status":200,"status":"enqueued","request_uuid":null,"expected_head_revision":null,"merge_action":null,"bypass_rules":null,"final_revision":null,"merge_verified":true});
    assert!(serde_json::from_value::<QueueReceipt>(v).is_err());
}
#[test]
fn queue_and_final_host_facts_never_become_verified_merge() {
    let t = target();
    let mut r = QueueObservation {
        version: 1,
        started_at_unix_ms: 1,
        finished_at_unix_ms: 2,
        snapshot: None,
        receipt: Some(pending()),
        enqueue_receipt: None,
        group_capture: None,
        group_checks: None,
        group_attempt: None,
        workflow_definition: None,
        async_result: None,
        mutation_not_sent: false,
        token_revoked: true,
    };
    assert_eq!(r.state(&t), "host_pending");
    r.receipt = None;
    assert_eq!(r.state(&t), "uncertain");
    let mut s = snapshot();
    s.entry = Some(QueueEntryObservation {
        id: "MQE_one".into(),
        state: "QUEUED".into(),
        base: None,
        head: None,
    });
    r.snapshot = Some(s.clone());
    assert_eq!(r.state(&t), "host_enqueued");
    let commit = GitCommitObservation {
        revision: "git:sha1:".to_owned() + &"44".repeat(20),
        tree: "git:sha1:".to_owned() + &"55".repeat(20),
    };
    s.entry.as_mut().unwrap().head = Some(commit.clone());
    s.entry.as_mut().unwrap().base = Some(commit.clone());
    r.snapshot = Some(s.clone());
    assert_eq!(r.state(&t), "validating");
    s.entry = None;
    s.pull.state = "closed".into();
    s.pull.merged = true;
    s.final_commit = Some(commit);
    r.snapshot = Some(s.clone());
    assert_eq!(r.state(&t), "host_merged_unverified");
    s.pull.head_revision = "git:sha1:".to_owned() + &"66".repeat(20);
    r.snapshot = Some(s);
    assert_eq!(r.state(&t), "stale");
    r.finished_at_unix_ms = 200_000;
    assert!(r.validate(&t).is_err());
}
#[test]
fn queue_snapshot_rejects_foreign_id_bad_object_and_unknown_configuration() {
    let t = target();
    for field in [
        "repository",
        "node",
        "number",
        "method",
        "strategy",
        "limits",
        "final",
    ] {
        let mut s = snapshot();
        match field {
            "repository" => s.pull.repository_id += 1,
            "node" => s.pull.node_id.push('x'),
            "number" => s.pull.number += 1,
            "method" => s.configuration.as_mut().unwrap().merge_method = "DEFAULT".into(),
            "strategy" => s.configuration.as_mut().unwrap().merging_strategy = "UNKNOWN".into(),
            "limits" => s.configuration.as_mut().unwrap().minimum_entries_to_merge = 100,
            _ => {
                s.final_commit = Some(GitCommitObservation {
                    revision: t.pull.head_revision.clone(),
                    tree: t.pull.head_revision.clone(),
                })
            }
        }
        assert!(s.validate(&t).is_err(), "{field}");
    }
}

#[test]
fn retained_async_result_vectors_and_invalid_identity_never_authorize_merge() {
    let vectors: Value =
        serde_json::from_str(include_str!("fixtures/queue-result-v1.json")).unwrap();
    assert_eq!(vectors["version"], 1);
    for case in vectors["cases"].as_array().unwrap() {
        let wire = json!({"request_uuid":vectors["request_uuid"],"http_status":case["http_status"],"outcome":case["outcome"]});
        let r: QueueAsyncResult = serde_json::from_value(wire.clone()).unwrap();
        r.validate(&target()).unwrap();
        assert_eq!(json!(r), wire);
        assert_eq!(json!(r.state()), case["state"]);
        assert_eq!(json!(r.terminal()), case["terminal"]);
        let mut invalid = r.clone();
        invalid.http_status = 403;
        assert!(invalid.validate(&target()).is_err());
        invalid = r;
        invalid.request_uuid = "../other-request".into();
        assert!(invalid.validate(&target()).is_err());
    }
    for field in ["head", "action", "bypass", "uuid", "extra"] {
        let mut wire = json!({"request_uuid":vectors["request_uuid"],"http_status":200,"outcome":vectors["cases"][0]["outcome"]});
        match field {
            "head" => {
                wire["outcome"]["expected_head_revision"] =
                    json!("git:sha1:".to_owned() + &"22".repeat(20))
            }
            "action" => wire["outcome"]["merge_action"] = json!("default"),
            "bypass" => wire["outcome"]["bypass_rules"] = json!(true),
            "uuid" => wire["request_uuid"] = json!("00000000-0000-0000-0000-000000000000"),
            _ => wire["outcome"]["merge_verified"] = json!(true),
        }
        assert!(
            serde_json::from_value::<QueueAsyncResult>(wire)
                .map_or(true, |r| r.validate(&target()).is_err()),
            "{field}"
        );
    }
    let mut bad_target = target();
    bad_target.pull.repository_id = 0;
    assert!(pending().validate(&bad_target).is_err());
    let mut r = pending();
    r.http_status = 200;
    assert!(r.validate(&target()).is_err()); // only a result GET may be pending/200
}

#[test]
fn read_snapshot_v3_retains_queue_identity_without_admission_configuration() {
    let v: Value = serde_json::from_str(include_str!("fixtures/queue-read-v3.json")).unwrap();
    let mut r: QueueObservation = serde_json::from_value(v.clone()).unwrap();
    r.validate(&target()).unwrap();
    assert_eq!(json!(r), v);
    assert_eq!(r.state(&target()), "host_enqueued");
    assert!(!r.snapshot.as_ref().unwrap().can_request(&target()));
    r.version = 2;
    assert!(r.validate(&target()).is_err());
    r.version = 3;
    r.snapshot.as_mut().unwrap().queue_id = None;
    assert!(r.validate(&target()).is_err());
    r.snapshot.as_mut().unwrap().queue_id = Some("MQ_foreign".into());
    r.snapshot.as_mut().unwrap().configuration = Some(configuration());
    assert!(r.validate(&target()).is_err());
}
