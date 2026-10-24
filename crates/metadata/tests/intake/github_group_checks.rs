use super::*;
use treeharbor_github_contract::group_checks::Side;
async fn report(f: &QueueFixture, m: &QueueObservation) -> QueueObservation {
    let mut v: QueueObservation = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/group-checks-v5.json"
    ))
    .unwrap();
    v.started_at_unix_ms = f.report().await.started_at_unix_ms;
    v.finished_at_unix_ms = v.started_at_unix_ms;
    let c = v.group_checks.as_mut().unwrap();
    let subject = m.group_capture.as_ref().unwrap().after.members[0]
        .candidate
        .as_ref()
        .unwrap()
        .commit
        .clone();
    c.target.subject = subject.clone();
    c.checks.subject = subject.clone();
    for view in [&mut c.checks.before, &mut c.checks.after] {
        for s in &mut view.suites {
            s.head_revision = subject.revision.clone();
        }
    }
    v
}
#[tokio::test]
#[ignore = "requires PostgreSQL; late candidate checks, original membership linkage, replay and authority"]
async fn group_checks_bind_retained_membership_and_do_not_replace_live_queue_or_certify_attempts() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let membership = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("membership"), f.id, a, &membership)
        .await
        .unwrap();
    let c =
        f.p.f
            .db
            .github_queue_check_context(&f.bridge, 1, f.id, 1, Side::After)
            .await
            .unwrap();
    let r = report(&f, &membership).await;
    assert_eq!(
        c["group_check_target"],
        json!(r.group_checks.as_ref().unwrap().target)
    );
    assert!(
        f.p.f
            .db
            .github_queue_check_context(&f.owner, 1, f.id, 1, Side::After)
            .await
            .is_err()
    );
    for case in ["missing-report", "wrong-subject", "invalid-sequence"] {
        let mut bad = r.clone();
        let c = bad.group_checks.as_mut().unwrap();
        match case {
            "missing-report" => c.target.membership_sequence = 3,
            "wrong-subject" => {
                c.target.subject.tree = format!("git:sha1:{}", "ef".repeat(20));
                c.checks.subject = c.target.subject.clone();
            }
            "invalid-sequence" => {
                c.target.membership_sequence = 0;
            }
            _ => unreachable!(),
        }
        assert!(
            f.p.f
                .db
                .report_github_queue(&f.bridge, &cmd(case), f.id, a, &bad)
                .await
                .is_err()
        );
    }
    let key = cmd("checks");
    let done =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(done["observation"], json!(membership));
    assert_eq!(done["recorded_observation"], json!(r));
    assert_eq!(done["report_sequence"], 2);
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    assert_eq!(
        done,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, a, &r)
            .await
            .unwrap()
    );
    assert!(
        f.p.f
            .db
            .github_queue_check_context(&f.bridge, 1, f.id, 2, Side::After)
            .await
            .is_err()
    );
    // Current membership disappears; an explicit read of the original candidate remains possible.
    let absent = f.report().await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("absent"), f.id, a, &absent)
        .await
        .unwrap();
    assert_eq!(
        c,
        f.p.f
            .db
            .github_queue_check_context(&f.bridge, 1, f.id, 1, Side::After)
            .await
            .unwrap()
    );
    let mut rerun = r.clone();
    rerun.group_checks.as_mut().unwrap().checks.after.suites[0]
        .workflow
        .as_mut()
        .unwrap()
        .observed_run_attempt = 2;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("rerun"), f.id, a, &rerun)
        .await
        .unwrap();
    let token =
        f.p.f
            .db
            .issue_repository_token(&f.p.f.scope, "human", &[P::Read], 3600, "operator")
            .await
            .unwrap();
    let read =
        f.p.f
            .db
            .authorize(&token.secret, &f.p.f.scope, P::Read)
            .await
            .unwrap();
    let page =
        f.p.f
            .db
            .github_queue_checks(&read, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(page["next_after"], 2);
    assert_eq!(page["captures"][0]["checks_consistent"], true);
    assert_eq!(page["captures"][0]["report"], json!(r));
    let matches = page["captures"][0]["selected_checks"][0]["after_matches"]
        .as_array()
        .unwrap();
    assert_eq!(matches.len(), 2);
    assert_eq!(matches[0]["merge_group_event"], true);
    assert_eq!(matches[1]["merge_group_event"], false);
    assert_eq!(page["captures"][0]["attempt_verified"], false);
    assert_eq!(page["captures"][0]["definition_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert_eq!(page["host_policy_verified"], false);
    let next =
        f.p.f
            .db
            .github_queue_checks(&read, 1, f.id, 2)
            .await
            .unwrap();
    assert_eq!(next["captures"][0]["sequence"], 4);
    assert_eq!(next["captures"][0]["checks_consistent"], false);
    assert!(next["next_after"].is_null());
    assert!(
        f.p.f
            .db
            .github_queue_checks(&read, 2, f.id, 0)
            .await
            .is_err()
    );
    f.p.f
        .db
        .set_repository_access(&f.p.f.scope, "human", &[], "operator")
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .github_queue_checks(&read, 1, f.id, 0)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
