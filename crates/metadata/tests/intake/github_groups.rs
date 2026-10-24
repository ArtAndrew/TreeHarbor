use super::*;
use treeharbor_github_webhook::{VerifiedDelivery, WebhookKey};
use treeharbor_metadata::github::{EventChannelSpec, Observation};
async fn group_report(f: &QueueFixture) -> QueueObservation {
    let mut v: Value = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/queue-groups-v4.json"
    ))
    .unwrap();
    let at = f.report().await.started_at_unix_ms;
    v["started_at_unix_ms"] = json!(at);
    v["finished_at_unix_ms"] = json!(at);
    for side in ["before", "after"] {
        let g = &mut v["group_capture"][side];
        let p = &mut g["target"]["pull"];
        p["node_id"] = json!(f.target.pull.pull_node);
        p["number"] = json!(f.target.pull.pull_number);
        p["head_ref"] = json!(f.target.pull.head_ref);
        p["head_revision"] = json!(f.target.pull.head_revision);
        p["base_revision"] = json!(f.target.expected_base_revision);
        let m = &mut g["members"][0];
        m["pull_node"] = json!(f.target.pull.pull_node);
        m["pull_number"] = json!(f.target.pull.pull_number);
        m["source_revision"] = json!(f.target.pull.head_revision);
        m["entry"]["base"]["revision"] = json!(format!("git:sha1:{}", "bb".repeat(20)));
        m["entry"]["head"] = json!({"revision":format!("git:sha1:{}","aa".repeat(20)),"tree":format!("git:sha1:{}","cc".repeat(20))});
        m["candidate"]["commit"] = m["entry"]["head"].clone();
        m["candidate"]["parents"][0] = m["entry"]["base"].clone();
        m["candidate"]["parents"][1]["revision"] = json!(f.target.pull.head_revision);
        g["target"]["entry"] = m["entry"].clone();
    }
    v["snapshot"] = v["group_capture"]["after"]["target"].clone();
    let r: QueueObservation = serde_json::from_value(v).unwrap();
    r.validate(&f.target).unwrap();
    r
}
fn signed(name: &str) -> VerifiedDelivery {
    let v: Value = serde_json::from_str(include_str!(
        "../../../github-webhook/tests/fixtures/vectors-v1.json"
    ))
    .unwrap();
    let f = v["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["name"] == name)
        .unwrap();
    WebhookKey::new(
        7,
        Uuid::new_v4(),
        v["secret"].as_str().unwrap().as_bytes().to_vec(),
    )
    .unwrap()
    .authenticate(
        "merge_group",
        f["signature"].as_str().unwrap(),
        f["body"].as_str().unwrap().as_bytes().to_vec(),
    )
    .unwrap()
}
async fn channel(f: &mut QueueFixture) -> Uuid {
    let probe = Uuid::new_v4();
    f.p.f
        .db
        .begin_github_probe(
            &f.p.reader,
            &cmd(&format!("probe-begin-{}", f.p.binding)),
            f.p.binding,
            probe,
        )
        .await
        .unwrap();
    let o: Observation = serde_json::from_str(include_str!(
        "../../../integrations-github/tests/fixtures/probe-v1.json"
    ))
    .unwrap();
    f.p.f
        .db
        .finish_github_probe(
            &f.p.reader,
            &cmd(&format!("probe-done-{}", f.p.binding)),
            f.p.binding,
            probe,
            &o,
        )
        .await
        .unwrap();
    let id = Uuid::new_v4();
    f.p.f
        .db
        .create_github_event_channel(
            &f.p.owner,
            &cmd(&format!("events-{}", f.p.binding)),
            id,
            &EventChannelSpec {
                binding: f.p.binding.to_string(),
                max_events: 16,
                max_bytes: 65536,
                max_aliases: 2,
            },
        )
        .await
        .unwrap();
    id
}
#[tokio::test]
#[ignore = "requires PostgreSQL; late/reordered signed facts, exact group linkage, immutable capture pagination and restart"]
async fn queue_groups_survive_later_missing_membership_and_join_only_exact_signed_facts() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let attempt = f.begin().await;
    let ch = channel(&mut f).await;
    let token =
        f.p.f
            .db
            .issue_repository_token(&f.p.f.scope, "human", &[P::Read], 3600, "operator")
            .await
            .unwrap();
    let reader =
        f.p.f
            .db
            .authorize(&token.secret, &f.p.f.scope, P::Read)
            .await
            .unwrap();
    let r = group_report(&f).await;
    let c = cmd("group-report");
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &c, f.id, attempt, &r)
            .await
            .unwrap();
    assert_eq!(
        v,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &c, f.id, attempt, &r)
            .await
            .unwrap()
    );
    let p =
        f.p.f
            .db
            .github_queue_groups(&reader, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(p["captures"][0]["consistent"], true);
    assert_eq!(p["captures"][0]["after_exact_merge_parents"], true);
    assert_eq!(p["captures"][0]["after_signed_events"]["events"], json!([]));
    // Signed destruction arrives first. Arrival order must not imply lifecycle order.
    for name in ["destroyed-invalidated", "checks"] {
        f.p.f
            .db
            .accept_github_event(Uuid::new_v4(), &signed(name))
            .await
            .unwrap();
    }
    f.p.f
        .db
        .revoke_github_event_channel(&f.p.owner, &cmd("revoke-events"), ch)
        .await
        .unwrap();
    let mut changed = r.clone();
    // A different base is still a valid capture, but cannot match this signed group.
    for side in [true, false] {
        let g = if side {
            &mut changed.group_capture.as_mut().unwrap().before
        } else {
            &mut changed.group_capture.as_mut().unwrap().after
        };
        g.members[0].entry.base.as_mut().unwrap().revision =
            format!("git:sha1:{}", "dd".repeat(20));
        // Queue metadata can drift before its candidate is rebuilt. Keep the
        // immutable candidate's original parents instead of forging a new DAG.
        g.target.entry = Some(g.members[0].entry.clone());
    }
    changed.snapshot = Some(changed.group_capture.as_ref().unwrap().after.target.clone());
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("second-group"), f.id, attempt, &changed)
        .await
        .unwrap();
    let unknown = f.report().await;
    f.p.f
        .db
        .report_github_queue(
            &f.bridge,
            &cmd("missing-membership"),
            f.id,
            attempt,
            &unknown,
        )
        .await
        .unwrap();
    f.p.f
        .db
        .cancel_github_queue(&f.owner, &cmd("cancel"), f.id)
        .await
        .unwrap();
    // Reconnecting the same upstream cannot mix a new channel into the old queue.
    f.p.f
        .db
        .revoke_github(&f.p.owner, &cmd("rebind-revoke"), f.p.binding)
        .await
        .unwrap();
    let spec: BindingSpec =
        serde_json::from_value(
            serde_json::from_str::<Value>(include_str!("../fixtures/github-bind-v1.json")).unwrap()
                ["specification"]
                .clone(),
        )
        .unwrap();
    f.p.binding = Uuid::new_v4();
    f.p.f
        .db
        .bind_github(&f.p.owner, &cmd("rebind"), f.p.binding, &spec)
        .await
        .unwrap();
    let new_channel = channel(&mut f).await;
    f.p.f
        .db
        .accept_github_event(Uuid::new_v4(), &signed("checks"))
        .await
        .unwrap();
    assert_ne!(ch, new_channel);
    assert_eq!(
        connection(&f.p.f.schema)
            .await
            .query_one("SELECT count(*) FROM th_github_events", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        3
    );
    let mut restarted = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    let p = restarted
        .github_queue_groups(&reader, 1, f.id, 0)
        .await
        .unwrap();
    assert_eq!(p["next_after"], 1);
    assert_eq!(p["captures"][0]["report"], json!(r));
    let events = p["captures"][0]["after_signed_events"]["events"]
        .as_array()
        .unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["fact"]["reason"], "invalidated");
    assert_eq!(events[1]["fact"]["action"], "checks_requested");
    assert_eq!(p["merge_verified"], false);
    assert_eq!(p["host_policy_verified"], false);
    let next = restarted
        .github_queue_groups(&reader, 1, f.id, 1)
        .await
        .unwrap();
    assert_eq!(next["captures"][0]["sequence"], 2);
    assert!(next["next_after"].is_null());
    assert_eq!(next["captures"][0]["after_exact_merge_parents"], false);
    assert_eq!(
        next["captures"][0]["after_signed_events"]["events"],
        json!([])
    );
    assert!(
        restarted
            .github_queue_groups(&reader, 2, f.id, 0)
            .await
            .is_err()
    );
    assert!(
        restarted
            .github_queue_groups(&reader, 1, f.id, -1)
            .await
            .is_err()
    );
    f.p.f
        .db
        .set_repository_access(&f.p.f.scope, "human", &[], "operator")
        .await
        .unwrap();
    assert!(
        restarted
            .github_queue_groups(&reader, 1, f.id, 0)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
