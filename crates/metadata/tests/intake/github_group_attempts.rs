use super::*;
async fn report(f: &QueueFixture, source: &QueueObservation) -> QueueObservation {
    let mut r: QueueObservation = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/group-attempt-v6.json"
    ))
    .unwrap();
    r.started_at_unix_ms = f.report().await.started_at_unix_ms;
    r.finished_at_unix_ms = r.started_at_unix_ms;
    let subject = source.group_checks.as_ref().unwrap().checks.subject.clone();
    let a = r.group_attempt.as_mut().unwrap();
    a.target.subject = subject.clone();
    for view in [&mut a.before, &mut a.after] {
        view.selected.head_revision = subject.revision.clone();
        view.current.head_revision = subject.revision.clone();
        view.check.head_revision = subject.revision.clone();
        for job in &mut view.jobs {
            job.head_revision = subject.revision.clone();
        }
    }
    r
}
#[tokio::test]
#[ignore = "requires PostgreSQL; exact retained check linkage, no stale rerun inheritance, replay and scoped history"]
async fn attempt_reports_bind_original_check_and_keep_history_separate_from_current_eligibility() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let dispatch = f.begin().await;
    let membership = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("membership"), f.id, dispatch, &membership)
        .await
        .unwrap();
    let checks = group_checks::report(&f, &membership).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("checks"), f.id, dispatch, &checks)
        .await
        .unwrap();
    let r = report(&f, &checks).await;
    let selection = r.group_attempt.as_ref().unwrap().target.selection();
    let context =
        f.p.f
            .db
            .github_queue_attempt_context(&f.bridge, 1, f.id, &selection)
            .await
            .unwrap();
    assert_eq!(
        context["group_attempt_target"],
        json!(r.group_attempt.as_ref().unwrap().target)
    );
    assert!(
        f.p.f
            .db
            .github_queue_attempt_context(&f.owner, 1, f.id, &selection)
            .await
            .is_err()
    );
    let mut push = selection.clone();
    push.suite_id = "CS_push".into();
    push.check_id = "CR_push".into();
    assert!(
        f.p.f
            .db
            .github_queue_attempt_context(&f.bridge, 1, f.id, &push)
            .await
            .is_err()
    );
    for case in [
        "membership-not-checks",
        "missing",
        "subject",
        "check-result",
        "workflow-attempt",
    ] {
        let mut bad = r.clone();
        let a = bad.group_attempt.as_mut().unwrap();
        match case {
            "membership-not-checks" => a.target.check_sequence = 1,
            "missing" => a.target.check_sequence = 4,
            "subject" => a.target.subject.tree = format!("git:sha1:{}", "ab".repeat(20)),
            "check-result" => a.target.check.conclusion = Some("FAILURE".into()),
            "workflow-attempt" => {
                a.target.workflow.observed_run_attempt = 2;
                for s in [&mut a.before, &mut a.after] {
                    s.selected.number = 2;
                    s.current.number = 2;
                    for j in &mut s.jobs {
                        j.attempt = 2;
                    }
                }
            }
            _ => unreachable!(),
        }
        bad.validate(&f.target).unwrap();
        assert!(
            f.p.f
                .db
                .report_github_queue(&f.bridge, &cmd(case), f.id, dispatch, &bad)
                .await
                .is_err(),
            "{case}"
        );
    }
    let key = cmd("attempt");
    let done =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, dispatch, &r)
            .await
            .unwrap();
    assert_eq!(done["recorded_observation"], json!(r));
    assert_eq!(done["observation"], json!(membership));
    assert_eq!(done["report_sequence"], 3);
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    assert_eq!(
        done,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, dispatch, &r)
            .await
            .unwrap()
    );
    let absent = f.report().await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("absent"), f.id, dispatch, &absent)
        .await
        .unwrap();
    assert_eq!(
        context,
        f.p.f
            .db
            .github_queue_attempt_context(&f.bridge, 1, f.id, &selection)
            .await
            .unwrap()
    );
    let mut newer = r.clone();
    let a = newer.group_attempt.as_mut().unwrap();
    for snapshot in [&mut a.before, &mut a.after] {
        snapshot.current.number = 2;
        snapshot.current.conclusion = Some("FAILURE".into());
    }
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, dispatch, &newer)
            .await
            .is_err()
    );
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("later-attempt"), f.id, dispatch, &newer)
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
            .github_queue_attempts(&read, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(page["captures"][0]["report"], json!(r));
    assert_eq!(page["next_after"], 3);
    assert_eq!(page["captures"][0]["attempt_linked"], true);
    assert_eq!(page["captures"][0]["current_attempt_matches"], true);
    assert_eq!(page["captures"][0]["definition_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert_eq!(page["host_policy_verified"], false);
    let next =
        f.p.f
            .db
            .github_queue_attempts(&read, 1, f.id, 3)
            .await
            .unwrap();
    assert_eq!(next["captures"][0]["sequence"], 5);
    assert_eq!(next["captures"][0]["attempt_linked"], true);
    assert_eq!(next["captures"][0]["current_attempt_matches"], false);
    assert!(next["next_after"].is_null());
    assert!(
        f.p.f
            .db
            .github_queue_attempts(&read, 2, f.id, 0)
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
            .github_queue_attempts(&read, 1, f.id, 0)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
