use super::super::restore::{journal, retained};
use super::*;
use treeharbor_github_contract::restore_queue::RestoreQueueReport;
use treeharbor_metadata::github::{
    DequeueSpec, RestoreQueueContext, RestoreQueueEvidence, RestoreQueueSubject,
};

#[tokio::test]
#[ignore = "requires PostgreSQL; retained queue/dequeue, fresh scope and immutable historical replay"]
async fn restore_queue_evidence_never_settles_retained_mutations() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let dequeue = Uuid::new_v4();
    f.p.f
        .db
        .request_github_dequeue(
            &f.owner,
            &cmd("remove"),
            dequeue,
            &DequeueSpec {
                queue: f.id.to_string(),
                ttl_seconds: 60,
                scope: "pull_request".into(),
            },
        )
        .await
        .unwrap();
    f.p.f
        .db
        .revoke_github(&f.p.owner, &cmd("revoke"), f.p.binding)
        .await
        .unwrap();
    let manifest = "a1".repeat(32);
    journal(&f.p, &manifest, None).await;
    let subject = RestoreQueueSubject::Dequeue { operation: dequeue };
    let before = retained(&f.p).await;
    let bundle: RestoreQueueContext = serde_json::from_value(
        f.p.f
            .db
            .github_restore_queue_context(&f.p.reader, 1, f.p.binding, &manifest, &subject)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(before, retained(&f.p).await);
    assert_eq!(bundle.queue, f.id);
    assert_eq!(bundle.target, f.target);
    let other = RestoreQueueSubject::Queue { operation: f.id };
    let queue_context =
        f.p.f
            .db
            .github_restore_queue_context(&f.p.reader, 1, f.p.binding, &manifest, &other)
            .await
            .unwrap();
    assert_eq!(queue_context["context_sha256"], bundle.context_sha256);
    for missing in [
        RestoreQueueSubject::Queue { operation: dequeue },
        RestoreQueueSubject::Dequeue { operation: f.id },
    ] {
        assert!(matches!(
            f.p.f
                .db
                .github_restore_queue_context(&f.p.reader, 1, f.p.binding, &manifest, &missing)
                .await,
            Err(MetaError::NotFound)
        ));
    }
    let now = f.report().await.finished_at_unix_ms;
    let snapshot = dequeues::snapshot(&f).await;
    let mut spec = RestoreQueueEvidence {
        version: 1,
        binding: f.p.binding,
        manifest_sha256: manifest,
        context_sha256: bundle.context_sha256,
        subject,
        report: RestoreQueueReport {
            version: 1,
            app_id: bundle.context.specification.app_id,
            installation_id: bundle.context.specification.upstream.installation_id,
            started_at_unix_ms: now,
            finished_at_unix_ms: now,
            before: snapshot.clone(),
            after: snapshot,
            token_revoked: true,
        },
    };
    // Read-only captures cannot carry privileged queue configuration.
    spec.report.before.configuration = None;
    spec.report.after.configuration = None;
    let historical_reader = actor(&mut f.p.f, "history-reader", K::Human, &[P::Read]).await;
    let wrong = actor(&mut f.p.f, "other-bridge", K::Bridge, &[P::HostRead]).await;
    for auth in [&f.owner, &wrong] {
        assert!(matches!(
            f.p.f
                .db
                .record_github_restore_queue(auth, &cmd("denied"), &spec)
                .await,
            Err(MetaError::Forbidden)
        ));
    }
    let before = retained(&f.p).await;
    let receipt =
        f.p.f
            .db
            .record_github_restore_queue(&f.p.reader, &cmd("observed-queue"), &spec)
            .await
            .unwrap();
    assert_eq!(receipt["classification"], "not_in_queue");
    assert_eq!(receipt["prior_mutation_settled"], false);
    assert_eq!(receipt["publication_authorized"], false);
    assert_eq!(before[1..], retained(&f.p).await[1..]);
    for case in [
        "app",
        "installation",
        "node",
        "repository",
        "old",
        "future",
        "duration",
        "digest",
        "subject",
        "binding",
    ] {
        let mut bad = spec.clone();
        match case {
            "app" => bad.report.app_id += 1,
            "installation" => bad.report.installation_id += 1,
            "node" => bad.report.after.pull.node_id = "PR_other".into(),
            "repository" => bad.report.after.pull.repository_id += 1,
            "old" => {
                bad.report.started_at_unix_ms -= 300000;
                bad.report.finished_at_unix_ms -= 300000;
            }
            "future" => {
                bad.report.started_at_unix_ms += 60000;
                bad.report.finished_at_unix_ms += 60000;
            }
            "duration" => bad.report.started_at_unix_ms -= 120001,
            "digest" => bad.context_sha256 = "a2".repeat(32),
            "subject" => bad.subject = RestoreQueueSubject::Queue { operation: dequeue },
            _ => bad.binding = Uuid::new_v4(),
        }
        let before = retained(&f.p).await;
        assert!(
            f.p.f
                .db
                .record_github_restore_queue(&f.p.reader, &cmd(case), &bad)
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(before, retained(&f.p).await);
    }
    let original = spec.clone();
    spec.subject = other;
    spec.report.after.pull.head_revision = "git:sha1:".to_owned() + &"aa".repeat(20);
    assert_eq!(
        f.p.f
            .db
            .record_github_restore_queue(&f.p.reader, &cmd("changed"), &spec)
            .await
            .unwrap()["classification"],
        "changed_during_capture"
    );
    spec.report.before = spec.report.after.clone();
    assert_eq!(
        f.p.f
            .db
            .record_github_restore_queue(&f.p.reader, &cmd("moved"), &spec)
            .await
            .unwrap()["classification"],
        "head_differs"
    );
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_queue(&f.p.reader, &cmd("observed-queue"), &spec)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    // Simulate additional retained attempt data arriving after capture. This is
    // a constrained fixture update, not authorization to dispatch after restore.
    connection(&f.p.f.schema).await.execute(
        "UPDATE th_github_dequeues SET attempt=$2,started_at=clock_timestamp() WHERE dequeue=$1",
        &[&dequeue,&Uuid::new_v4()],
    ).await.unwrap();
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_queue(&f.p.reader, &cmd("stale"), &spec)
            .await,
        Err(MetaError::RestoreContextChanged)
    ));
    let mut restarted = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    assert_eq!(
        restarted
            .record_github_restore_queue(&f.p.reader, &cmd("observed-queue"), &original)
            .await
            .unwrap(),
        receipt
    );
    assert_eq!(
        restarted
            .github_restore_queue(&historical_reader, "observed-queue")
            .await
            .unwrap(),
        receipt
    );
    restarted
        .set_repository_access(&f.p.f.scope, "bridge", &[], "operator")
        .await
        .unwrap();
    assert!(matches!(
        restarted
            .record_github_restore_queue(&f.p.reader, &cmd("observed-queue"), &original)
            .await,
        Err(MetaError::Forbidden)
    ));
    cleanup(&f.p.f.schema).await;
}
