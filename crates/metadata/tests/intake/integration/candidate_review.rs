use super::*;
use treeharbor_metadata::review::{ReviewFence, ReviewRead, ReviewTarget};

fn fence(candidate: Uuid) -> ReviewFence {
    ReviewFence {
        authority: Authority::Standalone,
        epoch: 1,
        target: ReviewTarget::Candidate { candidate },
    }
}
async fn holds(f: &Fixture) -> i64 {
    connection(&f.schema)
        .await
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0)
}
async fn read_roots(f: &mut Fixture, read: &ReviewRead) {
    for (i, node) in read.roots().into_iter().enumerate() {
        let value =
            f.db.read_review(read, i, &f.depot, &[node], 1_000_000)
                .await
                .unwrap()
                .read()
                .unwrap()
                .pop()
                .unwrap();
        let Object::NativeCommit(commit) = value.object() else {
            panic!("candidate roots must be commits")
        };
        assert_eq!(commit.snapshot, read.snapshots()[i]);
        let prepared = PreparedObject::new(value.object()).unwrap();
        let lease =
            f.db.reserve_object(&f.depot, &prepared, Uuid::new_v4(), 60)
                .await
                .unwrap();
        f.db.release_object_hold(&lease).await.unwrap();
        assert!(
            f.db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact candidate identity, readonly inspection and retained published result"]
async fn candidate_review_binds_prepared_result_and_does_not_grant_publication() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let entry = proposal(&mut f, &a, "review", b"candidate bytes\n").await;
    let (candidate, worker) = start(&mut f, &a, entry).await;
    let reader = auth(&mut f, &a.reviewer, P::Read).await;
    let initial = holds(&f).await;
    assert!(matches!(
        f.db.open_review(&reader, fence(candidate)).await,
        Err(MetaError::IntegrationState)
    ));
    assert_eq!(holds(&f).await, initial);
    let prepared = prepare(&mut f, &worker, candidate).await;
    let wrong = auth(&mut f, &a.reviewer, P::Review).await;
    assert!(matches!(
        f.db.open_review(&wrong, fence(candidate)).await,
        Err(MetaError::Forbidden)
    ));
    let initial = holds(&f).await;
    let read = f.db.open_review(&reader, fence(candidate)).await.unwrap();
    assert_eq!(holds(&f).await, initial + 2);
    let change: Uuid = connection(&f.schema)
        .await
        .query_one(
            "SELECT changeset FROM th_native_queue WHERE entry=$1",
            &[&entry],
        )
        .await
        .unwrap()
        .get(0);
    let mut expected: Value =
        serde_json::from_str(include_str!("../../fixtures/review-candidate-v1.json")).unwrap();
    let e = &mut expected["identity"];
    e["candidate"] = json!(candidate.to_string());
    e["entry"] = json!(entry.to_string());
    e["changeset"] = json!(change.to_string());
    e["run"] = json!(f.run.to_string());
    e["version"] = json!(1);
    e["candidate_epoch"] = json!(1);
    e["expected_generation"] = json!(1);
    e["proposal_base_revision"] = json!(f.revision.to_string());
    e["expected_head"] = json!(f.revision.to_string());
    e["revision"] = json!(RevisionRef::Native(prepared.revision()).to_string());
    e["policy"] = json!(ObjectId::from_bytes(policy().digest().unwrap()).to_string());
    assert_eq!(read.identity(), e);
    assert_eq!(read.snapshots(), [f.base, prepared.commit().snapshot]);
    read_roots(&mut f, &read).await;
    assert!(matches!(
        f.db.land_native_candidate(&worker, &cmd("no-auto-review"), candidate)
            .await,
        Err(MetaError::AdmissionBlocked)
    ));
    evidence(&mut f, &a, &worker, candidate).await;
    f.db.land_native_candidate(&worker, &cmd("land-reviewed"), candidate)
        .await
        .unwrap();
    // Publication changes mutable state and main, not the captured diff identity.
    read_roots(&mut f, &read).await;
    f.db.finish_review(read).await.unwrap();
    assert_eq!(holds(&f).await, initial);
    let mut restarted = Metadata::new(connection(&f.schema).await).await.unwrap();
    let read = restarted
        .open_review(&reader, fence(candidate))
        .await
        .unwrap();
    assert_eq!(read.identity(), e);
    restarted.finish_review(read).await.unwrap();
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; stale/cancelled candidate inspection does not refresh approvals"]
async fn candidate_review_preserves_old_context_after_policy_version_and_cancel_changes() {
    for action in ["policy", "version", "cancel"] {
        let mut f = Fixture::new().await;
        let a = controls(&mut f).await;
        let entry = proposal(&mut f, &a, "old", b"original\n").await;
        let (candidate, worker) = start(&mut f, &a, entry).await;
        prepare(&mut f, &worker, candidate).await;
        evidence(&mut f, &a, &worker, candidate).await;
        let reader = auth(&mut f, &a.reviewer, P::Read).await;
        let read = f.db.open_review(&reader, fence(candidate)).await.unwrap();
        let identity = read.identity().clone();
        match action {
            "policy" => {
                let admin = auth(&mut f, &a.admin, P::PolicyWrite).await;
                f.db.change_native_policy(
                    &admin,
                    &cmd("policy-change"),
                    "main",
                    1,
                    ObjectId::from_bytes(policy().digest().unwrap()),
                )
                .await
                .unwrap();
            }
            "version" => {
                let change = identity["changeset"].as_str().unwrap().parse().unwrap();
                let prepared = f.prepare(b"new proposal bytes\n").await;
                let mut s = submission(change, f.run, &prepared);
                assert!(matches!(
                    f.db.accept_change(&context(&f.scope, "wrong-version"), &s)
                        .await,
                    Err(MetaError::VersionConflict)
                ));
                assert_eq!(
                    f.db.native_candidate_status(&reader, candidate)
                        .await
                        .unwrap()["state"],
                    "checking"
                );
                s.previous_version = 1;
                let accepted =
                    f.db.accept_change(&context(&f.scope, "next-version"), &s)
                        .await
                        .unwrap();
                assert_eq!(
                    f.db.accept_change(&context(&f.scope, "next-version"), &s)
                        .await
                        .unwrap(),
                    accepted
                );
                let status = f.db.native_queue_status(&reader, entry).await.unwrap();
                assert_eq!(status["state"], "stale");
                assert_eq!(status["reason"], "change_updated");
                f.db.drain_workspace(&context(&f.scope, "drain"), f.workspace, 1)
                    .await
                    .unwrap();
            }
            "cancel" => {
                let owner = auth(&mut f, &a.owner, P::QueueRequest).await;
                f.db.cancel_native_entry(&owner, &cmd("cancel"), entry)
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        read_roots(&mut f, &read).await;
        f.db.finish_review(read).await.unwrap();
        let read = f.db.open_review(&reader, fence(candidate)).await.unwrap();
        assert_eq!(read.identity(), &identity);
        f.db.finish_review(read).await.unwrap();
        assert!(matches!(
            f.db.land_native_candidate(&worker, &cmd("stale-land"), candidate)
                .await,
            Err(MetaError::IntegrationState)
        ));
        let reviewer = auth(&mut f, &a.reviewer, P::Review).await;
        assert!(matches!(
            f.db.record_native_review(
                &reviewer,
                &cmd("stale-approve"),
                candidate,
                Uuid::new_v4(),
                "approve"
            )
            .await,
            Err(MetaError::IntegrationState)
        ));
        if action == "version" {
            let change = identity["changeset"].as_str().unwrap().parse().unwrap();
            let owner = auth(&mut f, &a.owner, P::QueueRequest).await;
            let entry = Uuid::new_v4();
            f.db.enqueue_native_change(&owner, &cmd("new-queue"), entry, "main", change, 2)
                .await
                .unwrap();
            let (fresh, worker) = start(&mut f, &a, entry).await;
            prepare(&mut f, &worker, fresh).await;
            let next = f.db.open_review(&reader, fence(fresh)).await.unwrap();
            assert_eq!(next.identity()["version"], 2);
            assert_ne!(next.identity()["revision"], identity["revision"]);
            f.db.finish_review(next).await.unwrap();
            assert!(matches!(
                f.db.land_native_candidate(&worker, &cmd("old-evidence"), fresh)
                    .await,
                Err(MetaError::AdmissionBlocked)
            ));
            evidence(&mut f, &a, &worker, fresh).await;
            f.db.land_native_candidate(&worker, &cmd("fresh-land"), fresh)
                .await
                .unwrap();
            let old = f.db.open_review(&reader, fence(candidate)).await.unwrap();
            assert_eq!(old.identity(), &identity);
            read_roots(&mut f, &old).await;
            f.db.finish_review(old).await.unwrap();
        }
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; candidate review scope, revocation, epoch and lease fences"]
async fn candidate_review_fences_current_access_but_retains_captured_epoch() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let entry = proposal(&mut f, &a, "read", b"kept\n").await;
    let (candidate, worker) = start(&mut f, &a, entry).await;
    prepare(&mut f, &worker, candidate).await;
    let reader = auth(&mut f, &a.reviewer, P::Read).await;
    let other = Scope::new(f.scope.tenant(), Uuid::new_v4()).unwrap();
    f.db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    f.db.set_repository_access(&other, "reviewer", &[P::Read], "operator")
        .await
        .unwrap();
    let token =
        f.db.issue_repository_token(&other, "reviewer", &[P::Read], 3600, "operator")
            .await
            .unwrap();
    let foreign =
        f.db.authorize(&token.secret, &other, P::Read)
            .await
            .unwrap();
    let initial = holds(&f).await;
    assert!(matches!(
        f.db.open_review(&foreign, fence(candidate)).await,
        Err(MetaError::NotFound)
    ));
    assert!(matches!(
        f.db.open_review(&reader, fence(Uuid::new_v4())).await,
        Err(MetaError::NotFound)
    ));
    assert_eq!(holds(&f).await, initial);
    let read = f.db.open_review(&reader, fence(candidate)).await.unwrap();
    connection(&f.schema)
        .await
        .execute(
            "UPDATE th_repositories SET epoch=2 WHERE repository=$1",
            &[&f.scope.repository()],
        )
        .await
        .unwrap();
    assert!(matches!(
        f.db.read_review(&read, 0, &f.depot, &[read.roots()[0].clone()], 1_000_000)
            .await,
        Err(MetaError::Fenced)
    ));
    assert!(matches!(
        f.db.finish_review(read).await,
        Err(MetaError::Fenced)
    ));
    assert_eq!(holds(&f).await, initial);
    let current = ReviewFence {
        epoch: 2,
        ..fence(candidate)
    };
    let read = f.db.open_review(&reader, current).await.unwrap();
    assert_eq!(read.identity()["candidate_epoch"], 1);
    connection(&f.schema)
        .await
        .execute(
            "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    assert!(matches!(
        f.db.read_review(&read, 0, &f.depot, &[read.roots()[0].clone()], 1_000_000)
            .await,
        Err(MetaError::StaleLease)
    ));
    f.db.finish_review(read).await.unwrap();
    let initial = holds(&f).await;
    let read = f.db.open_review(&reader, current).await.unwrap();
    f.db.revoke_repository_token(&f.scope, a.reviewer.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.read_review(&read, 0, &f.depot, &[read.roots()[0].clone()], 1_000_000)
            .await,
        Err(MetaError::Unauthenticated)
    ));
    assert!(matches!(
        f.db.finish_review(read).await,
        Err(MetaError::Unauthenticated)
    ));
    assert_eq!(holds(&f).await, initial);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; failed/conflicted/cancelled preparations have no reviewable result"]
async fn candidate_review_never_substitutes_proposal_when_preparation_has_no_result() {
    for (reason, conflict) in [
        ("failed", false),
        ("conflicted", true),
        ("cancelled", false),
    ] {
        let mut f = Fixture::new().await;
        let a = controls(&mut f).await;
        let entry = proposal(&mut f, &a, "unprepared", b"proposal\n").await;
        let (candidate, worker) = start(&mut f, &a, entry).await;
        if reason == "cancelled" {
            let owner = auth(&mut f, &a.owner, P::QueueRequest).await;
            f.db.cancel_native_entry(&owner, &cmd("cancel"), entry)
                .await
                .unwrap();
        } else {
            f.db.fail_native_candidate(&worker, &cmd("fail"), candidate, reason, conflict)
                .await
                .unwrap();
        }
        let reader = auth(&mut f, &a.reviewer, P::Read).await;
        let initial = holds(&f).await;
        assert!(matches!(
            f.db.open_review(&reader, fence(candidate)).await,
            Err(MetaError::IntegrationState)
        ));
        assert_eq!(holds(&f).await, initial);
        cleanup(&f.schema).await;
    }
}
