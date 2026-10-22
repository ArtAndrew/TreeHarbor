use super::*;
use treeharbor_metadata::review::{ReviewFence, ReviewTarget};
fn fence(changeset: Uuid) -> ReviewFence {
    ReviewFence {
        authority: Authority::Standalone,
        epoch: 1,
        target: ReviewTarget::Change {
            changeset,
            version: 1,
        },
    }
}
async fn input(f: &mut Fixture, a: &Actors) -> Uuid {
    let entry = proposal(f, a, "review", b"accepted bytes\n").await;
    connection(&f.schema)
        .await
        .query_one(
            "SELECT changeset FROM th_native_queue WHERE entry=$1",
            &[&entry],
        )
        .await
        .unwrap()
        .get(0)
}
#[tokio::test]
#[ignore = "requires PostgreSQL; exact accepted review versions and read-only permissions"]
async fn review_uses_immutable_version_pins_after_new_version_and_workspace_drain() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let change = input(&mut f, &a).await;
    let before = f.db.change_version(&f.scope, change, 1).await.unwrap();
    let read_auth = auth(&mut f, &a.reviewer, P::Read).await;
    let wrong = auth(&mut f, &a.owner, P::ChangeWrite).await;
    assert!(matches!(
        f.db.open_review(&wrong, fence(change)).await,
        Err(MetaError::Forbidden)
    ));
    let next = f.prepare(b"newer bytes\n").await;
    let mut s = submission(change, f.run, &next);
    s.previous_version = 1;
    f.db.accept_change(&context(&f.scope, "next-version"), &s)
        .await
        .unwrap();
    f.db.drain_workspace(&context(&f.scope, "drain-review"), f.workspace, 1)
        .await
        .unwrap();
    let read = f.db.open_review(&read_auth, fence(change)).await.unwrap();
    assert_eq!(read.identity()["run"], f.run.to_string());
    assert_eq!(read.identity()["version"], 1);
    assert_eq!(read.snapshots()[0].to_string(), before["base_snapshot"]);
    assert_eq!(read.snapshots()[1].to_string(), before["result_snapshot"]);
    let nodes = read.roots();
    for (i, node) in nodes.iter().enumerate() {
        let object =
            f.db.read_review(&read, i, &f.depot, std::slice::from_ref(node), 1_000_000)
                .await
                .unwrap()
                .read()
                .unwrap()
                .pop()
                .unwrap();
        assert_eq!(object.id(), node.id());
    }
    assert!(matches!(
        f.db.read_review(&read, 0, &f.depot, &[nodes[1].clone()], 1_000_000)
            .await,
        Err(MetaError::ReadScope)
    ));
    f.db.finish_review(read).await.unwrap();
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; review atomic acquisition, epoch/revocation fences and all-hold cleanup"]
async fn review_rejects_nonmembers_and_revoked_or_changed_authority_and_closes_all_holds() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let change = input(&mut f, &a).await;
    let reader = auth(&mut f, &a.reviewer, P::Read).await;
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    let RevisionRef::Native(base) = f.revision else {
        panic!()
    };
    assert!(matches!(
        f.db.open_review(
            &reader,
            ReviewFence {
                target: ReviewTarget::Revisions {
                    before: base,
                    after: ObjectId::from_bytes([77; 32])
                },
                ..fence(change)
            }
        )
        .await,
        Err(MetaError::NotFound)
    ));
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_holds WHERE expires_at>clock_timestamp()",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    let read = f.db.open_review(&reader, fence(change)).await.unwrap();
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_holds WHERE expires_at>clock_timestamp()",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        2
    );
    raw.execute("UPDATE th_repositories SET epoch=2", &[])
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
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_holds WHERE expires_at>clock_timestamp()",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    let read =
        f.db.open_review(
            &reader,
            ReviewFence {
                epoch: 2,
                ..fence(change)
            },
        )
        .await
        .unwrap();
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
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_holds WHERE expires_at>clock_timestamp()",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; scoped review roots, expired leases and durable pin GC"]
async fn review_does_not_cross_repository_and_retains_source_roots_during_gc() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let change = input(&mut f, &a).await;
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
    let other_auth =
        f.db.authorize(&token.secret, &other, P::Read)
            .await
            .unwrap();
    assert!(matches!(
        f.db.open_review(&other_auth, fence(change)).await,
        Err(MetaError::NotFound)
    ));
    let read = f.db.open_review(&reader, fence(change)).await.unwrap();
    let mut pending: Vec<_> = read.roots().into_iter().enumerate().collect();
    let mut seen = BTreeSet::new();
    while let Some((source, node)) = pending.pop() {
        if !seen.insert(node.id()) {
            continue;
        }
        let value =
            f.db.read_review(&read, source, &f.depot, &[node], 1_000_000)
                .await
                .unwrap()
                .read()
                .unwrap()
                .pop()
                .unwrap();
        pending.extend(value.children().map(|n| (source, n)));
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
    cleanup(&f.schema).await;
}
