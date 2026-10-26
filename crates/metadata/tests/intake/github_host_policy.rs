use super::*;
use treeharbor_github_contract::host_policy as hp;

async fn capture(f: &mut QueueFixture, membership: &QueueObservation) -> QueueObservation {
    let context =
        f.p.f
            .db
            .github_queue_host_policy_context(&f.bridge, 1, f.id, 1)
            .await
            .unwrap();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/host-policy-v1.json"
    ))
    .unwrap();
    let mut policy: hp::Report = serde_json::from_value(fixture["report"].clone()).unwrap();
    policy.target = serde_json::from_value(context["host_policy_target"].clone()).unwrap();
    policy.repository_name = "example-org/demo".into();
    for s in [&mut policy.before, &mut policy.after] {
        s.group = membership.group_capture.as_ref().unwrap().before.clone();
        s.review.pull = s.group.target.pull.clone();
        s.rules[0]["ruleset_source"] = json!("example-org/demo");
        s.rulesets[0].source.name = "example-org/demo".into();
        s.rulesets[0].document.value.as_mut().unwrap()["source"] = json!("example-org/demo");
    }
    let mut r = f.report().await;
    r.version = 9;
    r.host_policy = Some(Box::new(policy));
    r.validate(&f.target).unwrap();
    r
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact host-policy membership, durable replay, scope and historical capture"]
async fn host_policy_is_immutable_scoped_paginated_and_never_replaces_queue_state() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let attempt = f.begin().await;
    let membership = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("membership"), f.id, attempt, &membership)
        .await
        .unwrap();
    let r = capture(&mut f, &membership).await;
    assert!(
        f.p.f
            .db
            .github_queue_host_policy_context(&f.owner, 1, f.id, 1)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_host_policy_context(&f.bridge, 2, f.id, 1)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_host_policy_context(&f.bridge, 1, Uuid::new_v4(), 1)
            .await
            .is_err()
    );
    for case in ["missing_membership", "wrong_member", "foreign_repository"] {
        let mut bad = r.clone();
        let p = bad.host_policy.as_mut().unwrap();
        match case {
            "missing_membership" => p.target.membership_sequence = 2,
            "wrong_member" => p.target.member.entry.id = "different-member".into(),
            _ => {
                p.repository_name = "foreign/repo".into();
                for s in [&mut p.before, &mut p.after] {
                    s.rules[0]["ruleset_source"] = json!("foreign/repo");
                    s.rulesets[0].source.name = "foreign/repo".into();
                    s.rulesets[0].document.value.as_mut().unwrap()["source"] =
                        json!("foreign/repo");
                }
            }
        }
        // These are structurally valid claims, but the persisted binding/membership rejects them.
        bad.validate(&f.target).unwrap();
        assert!(
            f.p.f
                .db
                .report_github_queue(&f.bridge, &cmd(case), f.id, attempt, &bad)
                .await
                .is_err(),
            "{case}"
        );
    }
    let key = cmd("host-policy");
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.owner, &key, f.id, attempt, &r)
            .await
            .is_err()
    );
    let receipt =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &r)
            .await
            .unwrap();
    assert_eq!(receipt["recorded_observation"], json!(r));
    assert_eq!(receipt["observation"], json!(membership));
    assert_eq!(receipt["report_sequence"], 2);
    assert_eq!(receipt["host_policy_verified"], false);
    assert_eq!(receipt["merge_verified"], false);
    let raw = connection(&f.p.f.schema).await;
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_outbox", &[])
        .await
        .unwrap()
        .get(0);
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    assert_eq!(
        receipt,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &r)
            .await
            .unwrap()
    );
    assert_eq!(
        count,
        raw.query_one("SELECT count(*) FROM th_outbox", &[])
            .await
            .unwrap()
            .get::<_, i64>(0)
    );
    assert_eq!(
        2,
        raw.query_one("SELECT count(*) FROM th_github_queue_reports", &[])
            .await
            .unwrap()
            .get::<_, i64>(0)
    );
    let mut changed = r.clone();
    changed.host_policy.as_mut().unwrap().after.review.decision = Some("CHANGES_REQUESTED".into());
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &changed)
            .await
            .is_err()
    );
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
    let first =
        f.p.f
            .db
            .github_queue_host_policies(&reader, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(first["captures"][0]["report"], json!(r));
    assert_eq!(
        first["captures"][0]["capture_digest"],
        r.host_policy.as_ref().unwrap().digest(&f.target).unwrap()
    );
    assert!(first["next_after"].is_null());
    for flag in [
        "host_policy_verified",
        "admission_verified",
        "merge_verified",
    ] {
        assert_eq!(first[flag], false);
    }
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("changed"), f.id, attempt, &changed)
        .await
        .unwrap();
    let page =
        f.p.f
            .db
            .github_queue_host_policies(&reader, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(page["next_after"], 2);
    let page =
        f.p.f
            .db
            .github_queue_host_policies(&reader, 1, f.id, 2)
            .await
            .unwrap();
    assert_eq!(page["captures"][0]["sequence"], 3);
    assert!(
        page["captures"][0]["capture_reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("capture_changed"))
    );
    assert!(page["next_after"].is_null());
    assert!(
        f.p.f
            .db
            .github_queue_host_policy_context(&f.bridge, 1, f.id, 2)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_host_policies(&f.bridge, 1, f.id, 0)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_host_policies(&reader, 2, f.id, 0)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_host_policies(&reader, 1, f.id, -1)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_host_policies(&reader, 1, Uuid::new_v4(), 0)
            .await
            .is_err()
    );
    // Retirement fences new effects, but cannot erase historical evidence from the assigned bridge.
    f.p.f
        .db
        .revoke_github(&f.p.owner, &cmd("retire"), f.p.binding)
        .await
        .unwrap();
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("late-capture"), f.id, attempt, &r)
        .await
        .unwrap();
    assert_eq!(
        f.p.f
            .db
            .github_queue_host_policies(&reader, 1, f.id, 0)
            .await
            .unwrap()["captures"][0]["report"],
        json!(r)
    );
    f.p.f
        .db
        .set_repository_access(&f.p.f.scope, f.bridge.principal(), &[], "operator")
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("revoked"), f.id, attempt, &r)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
