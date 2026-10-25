use super::*;
use treeharbor_github_contract::publication as p;
#[tokio::test]
#[ignore = "requires PostgreSQL; exact publication source, replay, pages and authority fences"]
async fn publication_is_bound_to_retained_membership_and_survives_restart() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let attempt = f.begin().await;
    let m = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("membership"), f.id, attempt, &m)
        .await
        .unwrap();
    let selected =
        f.p.f
            .db
            .github_queue_publication_context(&f.bridge, 1, f.id, 1)
            .await
            .unwrap();
    assert!(
        f.p.f
            .db
            .github_queue_publication_context(&f.owner, 1, f.id, 1)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_publication_context(&f.bridge, 2, f.id, 1)
            .await
            .is_err()
    );
    let target: p::Target = serde_json::from_value(selected["publication_target"].clone()).unwrap();
    let c = target.candidate().unwrap().clone();
    let mut pull = m.snapshot.as_ref().unwrap().pull.clone();
    pull.merged = true;
    pull.state = "closed".into();
    let snapshot = p::Snapshot {
        pull,
        main: c.commit.clone(),
        final_commit: Some(c.clone()),
    };
    let mut r = f.report().await;
    r.version = 8;
    r.snapshot = None;
    r.publication = Some(p::Report {
        target,
        before: snapshot.clone(),
        after: snapshot,
        comparison: p::Comparison {
            base: c.commit.clone(),
            head_revision: c.commit.revision.clone(),
            merge_base: c.commit,
            status: "identical".into(),
            ahead_by: 0,
            behind_by: 0,
            total_commits: 0,
        },
    });
    r.validate(&f.target).unwrap();
    for (key, sequence) in [("missing", 2), ("bounds", 4096)] {
        let mut bad = r.clone();
        bad.publication.as_mut().unwrap().target.membership_sequence = sequence;
        assert!(
            f.p.f
                .db
                .report_github_queue(&f.bridge, &cmd(key), f.id, attempt, &bad)
                .await
                .is_err()
        );
    }
    let mut wrong = r.clone();
    wrong.publication.as_mut().unwrap().target.member.entry.id = "different-member".into();
    wrong.validate(&f.target).unwrap();
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("wrong-member"), f.id, attempt, &wrong)
            .await
            .is_err()
    );
    let key = cmd("publication");
    let receipt =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &r)
            .await
            .unwrap();
    assert_eq!(receipt["recorded_observation"], json!(r));
    assert_eq!(receipt["observation"], json!(m));
    assert_eq!(receipt["report_sequence"], 2);
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
    let page =
        f.p.f
            .db
            .github_queue_publications(&reader, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(page["captures"][0]["exact_candidate_published"], true);
    assert_eq!(page["captures"][0]["checks_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert_eq!(page["host_policy_verified"], false);
    assert!(page["next_after"].is_null());
    assert_eq!(page["captures"][0]["signed_events"]["events"], json!([]));
    let mut drift = r.clone();
    let d = drift.publication.as_mut().unwrap();
    d.after.main = treeharbor_github_contract::queue::GitCommitObservation {
        revision: format!("git:sha1:{}", "de".repeat(20)),
        tree: format!("git:sha1:{}", "ef".repeat(20)),
    };
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("drift"), f.id, attempt, &drift)
        .await
        .unwrap();
    let page =
        f.p.f
            .db
            .github_queue_publications(&reader, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(page["next_after"], 2);
    let page =
        f.p.f
            .db
            .github_queue_publications(&reader, 1, f.id, 2)
            .await
            .unwrap();
    assert_eq!(page["captures"][0]["exact_candidate_published"], false);
    assert!(page["next_after"].is_null());
    assert!(
        f.p.f
            .db
            .github_queue_publication_context(&f.bridge, 1, f.id, 2)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_publications(&reader, 2, f.id, 0)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .github_queue_publications(&reader, 1, f.id, -1)
            .await
            .is_err()
    );
    f.p.f
        .db
        .revoke_github(&f.p.owner, &cmd("revoke-binding"), f.p.binding)
        .await
        .unwrap();
    // Retiring an upstream fences new effects, not historical evidence from an
    // assigned bridge whose repository permission remains current.
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("retired-binding"), f.id, attempt, &r)
        .await
        .unwrap();
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
    assert!(
        f.p.f
            .db
            .github_queue_publication_context(&f.bridge, 1, f.id, 1)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
