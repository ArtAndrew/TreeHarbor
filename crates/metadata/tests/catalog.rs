mod common;
use common::{cleanup, connection, setup};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use treeharbor_core::*;
use treeharbor_metadata::catalog::ObjectLease;
use treeharbor_metadata::{Authority, Begin, Error as MetaError, Metadata, Request, Scope};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}
fn depot(root: &tempfile::TempDir, tenant: &str) -> ManagedStore {
    ManagedStore::open(root.path(), TenantId::new(tenant).unwrap(), "local-1").unwrap()
}
async fn upload(db: &mut Metadata, store: &ManagedStore, object: &Object) -> ObjectLease {
    let prepared = PreparedObject::new(object).unwrap();
    let lease = db
        .reserve_object(store, &prepared, Uuid::new_v4(), 60)
        .await
        .unwrap();
    if lease.requires_upload() {
        let receipt = store.put(lease.key().generation(), &prepared).unwrap();
        db.publish_object(&lease, &receipt).await.unwrap();
    }
    lease
}
fn request(scope: &Scope, key: &str) -> Request {
    Request::new(
        scope.clone(),
        key,
        "trusted-test",
        "pin-test",
        Authority::Standalone,
        1,
        key.as_bytes(),
    )
    .unwrap()
}
async fn pin(db: &mut Metadata, scope: &Scope, holder: &str, lease: &ObjectLease) {
    let Begin::Fresh(tx) = db
        .begin(request(scope, &Uuid::new_v4().to_string()))
        .await
        .unwrap()
    else {
        panic!()
    };
    tx.pin_object(holder, lease).await.unwrap();
    tx.complete(json!({"pinned":holder}), vec![]).await.unwrap();
}
async fn unpin(db: &mut Metadata, scope: &Scope, holder: &str) {
    let Begin::Fresh(tx) = db
        .begin(request(scope, &Uuid::new_v4().to_string()))
        .await
        .unwrap()
    else {
        panic!()
    };
    tx.unpin_object(holder).await.unwrap();
    tx.complete(json!({"unpinned":holder}), vec![])
        .await
        .unwrap();
}
fn directory(name: &str, id: ObjectId, kind: u8) -> Object {
    Object::Directory(
        Directory::new(vec![Entry {
            name: Name::new(name.as_bytes().to_vec()).unwrap(),
            value: EntryRef {
                target: id,
                mode: if kind == 2 {
                    EntryMode::Directory
                } else {
                    EntryMode::File
                },
            },
        }])
        .unwrap(),
    )
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn upload_before_publish_and_atomic_pin_survive_reconnect() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let prepared = PreparedObject::new(&Object::Blob(b"accepted bytes".to_vec())).unwrap();
    let lease = db
        .reserve_object(&store, &prepared, Uuid::new_v4(), 60)
        .await
        .unwrap();
    let Begin::Fresh(tx) = db.begin(request(&scope, "not-uploaded")).await.unwrap() else {
        panic!()
    };
    assert!(matches!(
        tx.pin_object("workspace/one", &lease).await,
        Err(MetaError::ObjectNotLive)
    ));
    drop(tx);
    let receipt = store.put(lease.key().generation(), &prepared).unwrap();
    db.publish_object(&lease, &receipt).await.unwrap();
    let Begin::Fresh(tx) = db.begin(request(&scope, "rolled-back")).await.unwrap() else {
        panic!()
    };
    tx.pin_object("workspace/aborted", &lease).await.unwrap();
    drop(tx);
    let Begin::Fresh(tx) = db.begin(request(&scope, "accepted")).await.unwrap() else {
        panic!()
    };
    tx.pin_object("workspace/one", &lease).await.unwrap();
    tx.complete(json!({"root":prepared.id().to_string()}), vec![])
        .await
        .unwrap();
    db.release_object_hold(&lease).await.unwrap();
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    assert!(matches!(
        db.begin(request(&scope, "accepted")).await,
        Ok(Begin::Replay(_))
    ));
    assert!(
        db.try_retire_object(lease.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    let read = db
        .lease_pinned_root(&scope, "workspace/one", Uuid::new_v4(), 60)
        .await
        .unwrap();
    assert_eq!(
        store.get(read.key().generation(), read.key().id()).unwrap(),
        Object::Blob(b"accepted bytes".to_vec())
    );
    assert!(matches!(
        db.lease_pinned_root(&scope, "workspace/aborted", Uuid::new_v4(), 60)
            .await,
        Err(MetaError::NotFound)
    ));
    let wrong = Scope::new("other-tenant", scope.repository()).unwrap();
    assert!(matches!(
        db.lease_pinned_root(&wrong, "workspace/one", Uuid::new_v4(), 60)
            .await,
        Err(MetaError::NotFound)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn wrong_receipts_missing_children_and_wrong_edge_types_fail_closed() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let expected = PreparedObject::new(&Object::Blob(b"expected".to_vec())).unwrap();
    let lease = db
        .reserve_object(&store, &expected, Uuid::new_v4(), 60)
        .await
        .unwrap();
    let wrong = store
        .put(
            1,
            &PreparedObject::new(&Object::Blob(b"wrong".to_vec())).unwrap(),
        )
        .unwrap();
    assert!(matches!(
        db.publish_object(&lease, &wrong).await,
        Err(MetaError::ReceiptMismatch)
    ));
    let wrong_generation = store.put(2, &expected).unwrap();
    assert!(matches!(
        db.publish_object(&lease, &wrong_generation).await,
        Err(MetaError::ReceiptMismatch)
    ));
    let other = depot(&root, "other");
    let wrong_tenant = other.put(1, &expected).unwrap();
    assert!(matches!(
        db.publish_object(&lease, &wrong_tenant).await,
        Err(MetaError::ReceiptMismatch)
    ));
    db.publish_object(&lease, &store.put(1, &expected).unwrap())
        .await
        .unwrap();
    let missing =
        PreparedObject::new(&directory("absent", ObjectId::from_bytes([9; 32]), 1)).unwrap();
    let missing_lease = db
        .reserve_object(&store, &missing, Uuid::new_v4(), 60)
        .await
        .unwrap();
    assert!(matches!(
        db.publish_object(&missing_lease, &store.put(1, &missing).unwrap())
            .await,
        Err(MetaError::ObjectNotLive)
    ));
    let wrong_kind =
        PreparedObject::new(&directory("not-a-directory", lease.key().id(), 2)).unwrap();
    let wrong_lease = db
        .reserve_object(&store, &wrong_kind, Uuid::new_v4(), 60)
        .await
        .unwrap();
    assert!(matches!(
        db.publish_object(&wrong_lease, &store.put(1, &wrong_kind).unwrap())
            .await,
        Err(MetaError::ObjectNotLive)
    ));
    let raw = connection(&schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_object_edges", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn retirement_recovery_and_late_old_generation_work_cannot_delete_new_bytes() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let prepared = PreparedObject::new(&Object::Blob(b"survivor".to_vec())).unwrap();
    let old = upload(&mut db, &store, &Object::Blob(b"survivor".to_vec())).await;
    db.release_object_hold(&old).await.unwrap();
    let retirement = db.try_retire_object(old.key(), 0).await.unwrap().unwrap();
    assert!(matches!(
        db.reserve_object(&store, &prepared, Uuid::new_v4(), 60)
            .await,
        Err(MetaError::ObjectBusy)
    ));
    // Crash after physical deletion but before recording its completion.
    store
        .remove_retired_generation(old.key().generation(), old.key().id())
        .unwrap();
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let pending = db.pending_retirements(scope.tenant(), 10).await.unwrap();
    assert_eq!(pending.len(), 1);
    db.finish_retirement(&store, &pending[0]).await.unwrap();
    let new = upload(&mut db, &store, &Object::Blob(b"survivor".to_vec())).await;
    assert_eq!(new.key().generation(), old.key().generation() + 1);
    pin(&mut db, &scope, "main", &new).await;
    // A delayed old uploader may leave orphan bytes, but cannot publish them.
    let old_receipt = store.put(old.key().generation(), &prepared).unwrap();
    assert!(db.publish_object(&old, &old_receipt).await.is_err());
    db.finish_retirement(&store, &retirement).await.unwrap();
    assert!(store.get(old.key().generation(), old.key().id()).is_err());
    assert!(store.get(new.key().generation(), new.key().id()).is_ok());
    assert!(db.try_retire_object(new.key(), 0).await.unwrap().is_none());
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn stale_retirement_permit_is_revalidated_before_destructive_io() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let lease = upload(&mut db, &store, &Object::Blob(b"restored".to_vec())).await;
    db.release_object_hold(&lease).await.unwrap();
    let permit = db.try_retire_object(lease.key(), 0).await.unwrap().unwrap();
    // Model an older restored catalog. Real restore must fence all old processes.
    let raw = connection(&schema).await;
    raw.batch_execute("DELETE FROM th_object_retirements; UPDATE th_objects SET state='live'")
        .await
        .unwrap();
    assert!(matches!(
        db.finish_retirement(&store, &permit).await,
        Err(MetaError::StaleLease)
    ));
    assert!(
        store
            .get(lease.key().generation(), lease.key().id())
            .is_ok()
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn expired_upload_cannot_publish_and_gc_observes_a_grace_period() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let prepared = PreparedObject::new(&Object::Blob(b"expired".to_vec())).unwrap();
    let lease = db
        .reserve_object(&store, &prepared, Uuid::new_v4(), 60)
        .await
        .unwrap();
    let receipt = store.put(1, &prepared).unwrap();
    let raw = connection(&schema).await;
    raw.batch_execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
    )
    .await
    .unwrap();
    assert!(matches!(
        db.publish_object(&lease, &receipt).await,
        Err(MetaError::StaleLease)
    ));
    assert!(
        db.try_retire_object(lease.key(), 30)
            .await
            .unwrap()
            .is_none()
    );
    raw.batch_execute("UPDATE th_objects SET orphaned_at=clock_timestamp()-interval '31 seconds'")
        .await
        .unwrap();
    let permit = db
        .try_retire_object(lease.key(), 30)
        .await
        .unwrap()
        .unwrap();
    db.finish_retirement(&store, &permit).await.unwrap();
    assert!(store.get(1, lease.key().id()).is_err());
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn persistent_pins_and_reader_holds_protect_the_entire_native_history() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let mut leases = Vec::new();
    let operations = upload(
        &mut db,
        &store,
        &Object::ChangeOperations(Changes::new(vec![]).unwrap()),
    )
    .await;
    let operations_id = operations.key().id();
    leases.push(operations);
    let mut snapshots = Vec::new();
    let mut second_parent_blob = None;
    for name in ["left", "right"] {
        let blob = upload(&mut db, &store, &Object::Blob(name.as_bytes().to_vec())).await;
        if name == "right" {
            second_parent_blob = Some(blob.key().clone());
        }
        let dir = upload(&mut db, &store, &directory(name, blob.key().id(), 1)).await;
        let snapshot = upload(
            &mut db,
            &store,
            &Object::Snapshot {
                root: dir.key().id(),
            },
        )
        .await;
        snapshots.push(snapshot.key().id());
        leases.extend([blob, dir, snapshot]);
    }
    let commit = |parents, snapshot, message: &str| {
        Object::NativeCommit(Commit {
            git_origin: None,
            parents,
            snapshot,
            changes: operations_id,
            author_name: "Test".into(),
            author_email: "test@example.invalid".into(),
            timestamp_seconds: 1,
            timezone_minutes: 0,
            message: message.into(),
        })
    };
    let left = upload(&mut db, &store, &commit(vec![], snapshots[0], "left")).await;
    let right = upload(&mut db, &store, &commit(vec![], snapshots[1], "right")).await;
    let merge = upload(
        &mut db,
        &store,
        &commit(
            vec![left.key().id(), right.key().id()],
            snapshots[0],
            "merge",
        ),
    )
    .await;
    pin(&mut db, &scope, "branch/main", &merge).await;
    leases.extend([left, right, merge.clone()]);
    for lease in &leases {
        db.release_object_hold(lease).await.unwrap();
    }
    for lease in &leases {
        assert!(
            db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    let reader = db
        .lease_pinned_root(&scope, "branch/main", Uuid::new_v4(), 60)
        .await
        .unwrap();
    unpin(&mut db, &scope, "branch/main").await;
    for lease in &leases {
        assert!(
            db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    let second = second_parent_blob.unwrap();
    assert!(store.get(second.generation(), second.id()).is_ok());
    db.release_object_hold(&reader).await.unwrap();
    for _ in 0..leases.len() + 1 {
        for lease in &leases {
            if let Some(retirement) = db.try_retire_object(lease.key(), 0).await.unwrap() {
                db.finish_retirement(&store, &retirement).await.unwrap();
            }
        }
    }
    assert_eq!(
        connection(&schema)
            .await
            .query_one(
                "SELECT count(*) FROM th_objects WHERE state<>'deleted'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn same_holder_cannot_be_retargeted_implicitly() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let first = upload(&mut db, &store, &Object::Blob(b"first".to_vec())).await;
    let second = upload(&mut db, &store, &Object::Blob(b"second".to_vec())).await;
    pin(&mut db, &scope, "workspace/one", &first).await;
    pin(&mut db, &scope, "workspace/one", &first).await;
    let Begin::Fresh(tx) = db.begin(request(&scope, "conflicting-pin")).await.unwrap() else {
        panic!()
    };
    assert!(matches!(
        tx.pin_object("workspace/one", &second).await,
        Err(MetaError::PinConflict)
    ));
    drop(tx);
    let read = db
        .lease_pinned_root(&scope, "workspace/one", Uuid::new_v4(), 60)
        .await
        .unwrap();
    assert_eq!(read.key().id(), first.key().id());
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn concurrent_reuse_and_gc_never_accept_a_dangling_pin() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    for round in 0..12 {
        let object = Object::Blob(format!("race-{round}").into_bytes());
        let old = upload(&mut db, &store, &object).await;
        db.release_object_hold(&old).await.unwrap();
        let mut writer = Metadata::new(connection(&schema).await).await.unwrap();
        let mut gc = Metadata::new(connection(&schema).await).await.unwrap();
        let write = async {
            let prepared = PreparedObject::new(&object).unwrap();
            match writer
                .reserve_object(&store, &prepared, Uuid::new_v4(), 60)
                .await
            {
                Err(MetaError::ObjectBusy) => None,
                Err(error) => panic!("unexpected reserve error: {error}"),
                Ok(lease) => {
                    if lease.requires_upload() {
                        writer
                            .publish_object(
                                &lease,
                                &store.put(lease.key().generation(), &prepared).unwrap(),
                            )
                            .await
                            .unwrap();
                    }
                    pin(&mut writer, &scope, &format!("race/{round}"), &lease).await;
                    Some(lease)
                }
            }
        };
        let sweep = async {
            if let Some(retirement) = gc.try_retire_object(old.key(), 0).await.unwrap() {
                gc.finish_retirement(&store, &retirement).await.unwrap();
            }
        };
        let (accepted, ()) = tokio::join!(write, sweep);
        if let Some(lease) = accepted {
            assert_eq!(
                store
                    .get(lease.key().generation(), lease.key().id())
                    .unwrap(),
                object
            );
        }
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn independent_mark_and_sweep_model_matches_catalog_after_cascade() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let mut leases: Vec<ObjectLease> = Vec::new();
    let mut edges: Vec<Vec<usize>> = Vec::new();
    let mut kinds = Vec::new();
    let mut seed = 17u64;
    for i in 0..32 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let (object, children) = if i == 0 || i % 3 == 0 {
            (Object::Blob(format!("blob-{i}").into_bytes()), vec![])
        } else {
            let child = (seed as usize) % i;
            (
                directory(&format!("node-{i}"), leases[child].key().id(), kinds[child]),
                vec![child],
            )
        };
        kinds.push(object.kind());
        edges.push(children);
        leases.push(upload(&mut db, &store, &object).await);
    }
    let roots = [29usize, 31];
    let mut expected = BTreeSet::new();
    let mut pending = roots.to_vec();
    while let Some(node) = pending.pop() {
        if expected.insert(node) {
            pending.extend(&edges[node]);
        }
    }
    for node in roots {
        pin(&mut db, &scope, &format!("model/{node}"), &leases[node]).await;
    }
    for lease in &leases {
        db.release_object_hold(lease).await.unwrap();
    }
    for _ in 0..leases.len() + 1 {
        for (index, lease) in leases.iter().enumerate() {
            if let Some(retirement) = db.try_retire_object(lease.key(), 0).await.unwrap() {
                assert!(
                    !expected.contains(&index),
                    "GC attempted to retire a model-reachable object"
                );
                db.finish_retirement(&store, &retirement).await.unwrap();
            }
        }
    }
    for (index, lease) in leases.iter().enumerate() {
        assert_eq!(
            store
                .get(lease.key().generation(), lease.key().id())
                .is_ok(),
            expected.contains(&index)
        );
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn migration_from_published_v1_preserves_existing_repository_and_is_repeatable() {
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    let raw = connection("public").await;
    raw.batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let sql = include_str!("../migrations/001_transactions.sql");
    raw.batch_execute(sql).await.unwrap();
    raw.execute(
        "INSERT INTO th_schema_version (version,migration_digest) VALUES (1,$1)",
        &[&Sha256::digest(sql.as_bytes()).to_vec()],
    )
    .await
    .unwrap();
    let scope = Scope::new("upgrading", Uuid::new_v4()).unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let Begin::Fresh(tx) = db.begin(request(&scope, "after-upgrade")).await.unwrap() else {
        panic!()
    };
    tx.complete(json!({"ok":true}), vec![]).await.unwrap();
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        26
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn concurrent_token_reuse_cannot_bind_one_hold_to_two_objects() {
    let (schema, scope, mut first) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let mut second = Metadata::new(connection(&schema).await).await.unwrap();
    let a = PreparedObject::new(&Object::Blob(b"a".to_vec())).unwrap();
    let b = PreparedObject::new(&Object::Blob(b"b".to_vec())).unwrap();
    let token = Uuid::new_v4();
    let (a, b) = tokio::join!(
        first.reserve_object(&store, &a, token, 60),
        second.reserve_object(&store, &b, token, 60)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    assert!(
        matches!(a, Err(MetaError::ReceiptMismatch))
            || matches!(b, Err(MetaError::ReceiptMismatch))
    );
    let raw = connection(&schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_objects", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_object_holds", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn concurrent_deletion_retries_finish_one_retirement() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let lease = upload(&mut db, &store, &Object::Blob(b"gc retry".to_vec())).await;
    db.release_object_hold(&lease).await.unwrap();
    let retirement = db.try_retire_object(lease.key(), 0).await.unwrap().unwrap();
    let mut other = Metadata::new(connection(&schema).await).await.unwrap();
    let (first, second) = tokio::join!(
        db.finish_retirement(&store, &retirement),
        other.finish_retirement(&store, &retirement)
    );
    first.unwrap();
    second.unwrap();
    assert!(
        db.pending_retirements(scope.tenant(), 10)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .get(lease.key().generation(), lease.key().id())
            .is_err()
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn subtree_operations_retain_both_directory_graphs_until_last_pin_is_removed() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let old_blob = upload(
        &mut db,
        &store,
        &Object::Blob(b"old retained bytes".to_vec()),
    )
    .await;
    let new_blob = upload(
        &mut db,
        &store,
        &Object::Blob(b"new retained bytes".to_vec()),
    )
    .await;
    let old_dir = upload(&mut db, &store, &directory("old", old_blob.key().id(), 1)).await;
    let new_dir = upload(&mut db, &store, &directory("new", new_blob.key().id(), 1)).await;
    let changes = Object::ChangeOperations(
        Changes::with_subtrees(vec![ChangeOperation {
            path: RepoPath::new(b"a".to_vec()).unwrap(),
            expected: Some(EntryRef {
                mode: EntryMode::Directory,
                target: old_dir.key().id(),
            }),
            replacement: Some(EntryRef {
                mode: EntryMode::Directory,
                target: new_dir.key().id(),
            }),
        }])
        .unwrap(),
    );
    let operations = upload(&mut db, &store, &changes).await;
    pin(&mut db, &scope, "retained-delta", &operations).await;
    let leases = [operations, old_dir, new_dir, old_blob, new_blob];
    for lease in &leases {
        db.release_object_hold(lease).await.unwrap();
    }
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    for lease in &leases {
        assert!(
            db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    let read = db
        .lease_pinned_root(&scope, "retained-delta", Uuid::new_v4(), 60)
        .await
        .unwrap();
    assert_eq!(
        store.get(read.key().generation(), read.key().id()).unwrap(),
        changes
    );
    db.release_object_hold(&read).await.unwrap();
    unpin(&mut db, &scope, "retained-delta").await;
    for lease in &leases {
        let retired = db
            .try_retire_object(lease.key(), 0)
            .await
            .unwrap()
            .expect("unreferenced graph should retire children after parent");
        db.finish_retirement(&store, &retired).await.unwrap();
        assert!(
            store
                .get(lease.key().generation(), lease.key().id())
                .is_err()
        );
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn original_git_metadata_survives_pin_reader_reconnect_and_is_eventually_collectable() {
    let (schema, scope, mut db) = setup().await;
    let root = root();
    let store = depot(&root, scope.tenant());
    let raw = upload(
        &mut db,
        &store,
        &Object::Blob(b"opaque original Git commit payload".to_vec()),
    )
    .await;
    let directory = upload(
        &mut db,
        &store,
        &Object::Directory(Directory::new(vec![]).unwrap()),
    )
    .await;
    let snapshot = upload(
        &mut db,
        &store,
        &Object::Snapshot {
            root: directory.key().id(),
        },
    )
    .await;
    let changes = upload(
        &mut db,
        &store,
        &Object::ChangeOperations(Changes::new(vec![]).unwrap()),
    )
    .await;
    // The catalog retains data; it deliberately does not endorse this claimed
    // Git identity. The export boundary must reject this invalid association.
    let head = upload(
        &mut db,
        &store,
        &Object::NativeCommit(Commit {
            git_origin: Some(Box::new(GitOrigin {
                oid: GitOid::Sha1([1; 20]),
                raw: raw.key().id(),
            })),
            parents: vec![],
            snapshot: snapshot.key().id(),
            changes: changes.key().id(),
            author_name: "Test".into(),
            author_email: "test@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: "source".into(),
        }),
    )
    .await;
    pin(&mut db, &scope, "branch/main", &head).await;
    let leases = [raw.clone(), directory, snapshot, changes, head];
    for lease in &leases {
        db.release_object_hold(lease).await.unwrap();
    }
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    assert!(db.try_retire_object(raw.key(), 0).await.unwrap().is_none());
    let read = db
        .lease_pinned_root(&scope, "branch/main", Uuid::new_v4(), 60)
        .await
        .unwrap();
    unpin(&mut db, &scope, "branch/main").await;
    assert!(db.try_retire_object(raw.key(), 0).await.unwrap().is_none());
    assert_eq!(
        store.get(raw.key().generation(), raw.key().id()).unwrap(),
        Object::Blob(b"opaque original Git commit payload".to_vec())
    );
    db.release_object_hold(&read).await.unwrap();
    for _ in 0..leases.len() + 1 {
        for lease in &leases {
            if let Some(retirement) = db.try_retire_object(lease.key(), 0).await.unwrap() {
                db.finish_retirement(&store, &retirement).await.unwrap();
            }
        }
    }
    assert!(store.get(raw.key().generation(), raw.key().id()).is_err());
    cleanup(&schema).await;
}
