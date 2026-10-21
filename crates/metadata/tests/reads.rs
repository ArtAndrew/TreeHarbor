mod common;
use common::{cleanup, connection, setup};
use serde_json::json;
use treeharbor_core::*;
use treeharbor_metadata::catalog::ObjectLease;
use treeharbor_metadata::reads::{MAX_READ_BYTES, MAX_READ_OBJECTS, ReadNode, ReadSession};
use treeharbor_metadata::{Authority, Begin, Error as MetaError, Metadata, Request, Scope};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

fn depot(root: &tempfile::TempDir, tenant: &str, backend: &str) -> ManagedStore {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    ManagedStore::open(root.path(), TenantId::new(tenant).unwrap(), backend).unwrap()
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
async fn pin(db: &mut Metadata, scope: &Scope, lease: Option<&ObjectLease>) {
    let request = Request::new(
        scope.clone(),
        &Uuid::new_v4().to_string(),
        "test",
        "test-pin",
        Authority::Standalone,
        1,
        b"test",
    )
    .unwrap();
    let Begin::Fresh(tx) = db.begin(request).await.unwrap() else {
        panic!()
    };
    if let Some(lease) = lease {
        tx.pin_object("source", lease).await.unwrap();
    } else {
        tx.unpin_object("source").await.unwrap();
    }
    tx.complete(json!({}), vec![]).await.unwrap();
}
async fn tree(db: &mut Metadata, scope: &Scope, store: &ManagedStore) -> Vec<ObjectLease> {
    let mut leases = Vec::new();
    let mut entries = Vec::new();
    for (name, contents) in [("first", "useful source"), ("second", "not requested")] {
        let blob = upload(db, store, &Object::Blob(contents.as_bytes().to_vec())).await;
        entries.push(Entry {
            name: Name::new(name.as_bytes().to_vec()).unwrap(),
            value: EntryRef {
                target: blob.key().id(),
                mode: EntryMode::File,
            },
        });
        leases.push(blob);
    }
    let dir = upload(
        db,
        store,
        &Object::Directory(Directory::new(entries).unwrap()),
    )
    .await;
    let snapshot = upload(
        db,
        store,
        &Object::Snapshot {
            root: dir.key().id(),
        },
    )
    .await;
    leases.extend([dir, snapshot]);
    pin(db, scope, leases.last()).await;
    for lease in &leases {
        db.release_object_hold(lease).await.unwrap();
    }
    leases
}
async fn children(
    db: &mut Metadata,
    session: &ReadSession,
    store: &ManagedStore,
    node: ReadNode,
) -> Vec<ReadNode> {
    db.open_read_batch(session, store, &[node], MAX_READ_BYTES)
        .await
        .unwrap()
        .read()
        .unwrap()[0]
        .children()
        .collect()
}
async fn files(db: &mut Metadata, session: &ReadSession, store: &ManagedStore) -> Vec<ReadNode> {
    let dirs = children(db, session, store, session.root()).await;
    children(db, session, store, dirs[0].clone()).await
}
async fn expire(schema: &str) {
    connection(schema)
        .await
        .batch_execute(
            "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
        )
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn verified_frontiers_allow_selected_files_without_reading_unrequested_content() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    let leases = tree(&mut db, &scope, &store).await;
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    let nodes = files(&mut db, &session, &store).await;
    assert_eq!(nodes.len(), 2);
    // Simulate storage loss outside the selected sparse frontier. Reading one
    // file must not eagerly traverse/download/verify the other file's bytes.
    store
        .remove_retired_generation(1, leases[1].key().id())
        .unwrap();
    let selected = nodes
        .iter()
        .find(|n| n.id() == leases[0].key().id())
        .unwrap();
    let read = db
        .open_read_batch(
            &session,
            &store,
            std::slice::from_ref(selected),
            MAX_READ_BYTES,
        )
        .await
        .unwrap()
        .read()
        .unwrap();
    assert_eq!(read[0].object(), &Object::Blob(b"useful source".to_vec()));
    let absent = nodes
        .iter()
        .find(|n| n.id() == leases[1].key().id())
        .unwrap();
    assert!(matches!(
        db.open_read_batch(
            &session,
            &store,
            std::slice::from_ref(absent),
            MAX_READ_BYTES
        )
        .await,
        Err(MetaError::Objects(Error::MissingObject(_)))
    ));
    db.close_read_session(session).await.unwrap();
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn opened_batch_survives_expiry_full_gc_and_generation_reuse() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    let leases = tree(&mut db, &scope, &store).await;
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    let nodes = files(&mut db, &session, &store).await;
    let batch = db
        .open_read_batch(&session, &store, &nodes, MAX_READ_BYTES)
        .await
        .unwrap();
    pin(&mut db, &scope, None).await;
    expire(&schema).await;
    assert!(matches!(
        db.renew_read_session(&session, 60).await,
        Err(MetaError::StaleLease)
    ));
    for lease in leases.iter().rev() {
        let retirement = db.try_retire_object(lease.key(), 0).await.unwrap().unwrap();
        db.finish_retirement(&store, &retirement).await.unwrap();
        assert!(
            store
                .get(lease.key().generation(), lease.key().id())
                .is_err()
        );
    }
    // No object path exists now. These bytes must come from already held inodes.
    let mut results: Vec<_> = batch
        .read()
        .unwrap()
        .into_iter()
        .map(|o| match o.into_object() {
            Object::Blob(b) => b,
            _ => panic!(),
        })
        .collect();
    results.sort();
    assert_eq!(
        results,
        vec![b"not requested".to_vec(), b"useful source".to_vec()]
    );
    let new = tree(&mut db, &scope, &store).await;
    assert_eq!(new[0].key().generation(), 2);
    assert!(matches!(
        db.open_read_batch(&session, &store, &nodes, MAX_READ_BYTES)
            .await,
        Err(MetaError::ObjectNotLive)
    ));
    let fresh = db.open_read_session(&scope, "source", 60).await.unwrap();
    assert!(matches!(
        db.open_read_batch(&fresh, &store, &nodes, MAX_READ_BYTES)
            .await,
        Err(MetaError::ReadScope)
    ));
    let fresh_nodes = files(&mut db, &fresh, &store).await;
    assert_eq!(
        db.open_read_batch(&fresh, &store, &fresh_nodes, MAX_READ_BYTES)
            .await
            .unwrap()
            .read()
            .unwrap()
            .len(),
        2
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn scope_and_backend_mismatch_never_grant_cross_session_reads() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    tree(&mut db, &scope, &store).await;
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    let other_scope = Scope::new(scope.tenant(), Uuid::new_v4()).unwrap();
    db.create_repository(&other_scope, Authority::Standalone)
        .await
        .unwrap();
    assert!(matches!(
        db.open_read_session(&other_scope, "source", 60).await,
        Err(MetaError::NotFound)
    ));
    let foreign_scope = Scope::new("foreign", scope.repository()).unwrap();
    assert!(matches!(
        db.open_read_session(&foreign_scope, "source", 60).await,
        Err(MetaError::NotFound)
    ));
    let other = db.open_read_session(&scope, "source", 60).await.unwrap();
    assert!(matches!(
        db.open_read_batch(&session, &store, &[other.root()], MAX_READ_BYTES)
            .await,
        Err(MetaError::ReadScope)
    ));
    let foreign = depot(&root, "foreign", "local-1");
    assert!(matches!(
        db.open_read_batch(&session, &foreign, &[session.root()], MAX_READ_BYTES)
            .await,
        Err(MetaError::ReadScope)
    ));
    let wrong_backend = depot(&root, scope.tenant(), "wrong-backend");
    assert!(matches!(
        db.open_read_batch(&session, &wrong_backend, &[session.root()], MAX_READ_BYTES)
            .await,
        Err(MetaError::ReceiptMismatch)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn read_budgets_are_enforced_before_opening_files() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    let leases = tree(&mut db, &scope, &store).await;
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    let nodes = files(&mut db, &session, &store).await;
    store
        .remove_retired_generation(1, leases[0].key().id())
        .unwrap();
    assert!(matches!(
        db.open_read_batch(&session, &store, &nodes, 1).await,
        Err(MetaError::ReadLimit)
    ));
    assert!(matches!(
        db.open_read_batch(&session, &store, &nodes, MAX_READ_BYTES + 1)
            .await,
        Err(MetaError::ReadLimit)
    ));
    assert!(matches!(
        db.open_read_batch(&session, &store, &[], MAX_READ_BYTES)
            .await,
        Err(MetaError::ReadLimit)
    ));
    assert!(matches!(
        db.open_read_batch(
            &session,
            &store,
            &vec![session.root(); MAX_READ_OBJECTS + 1],
            MAX_READ_BYTES
        )
        .await,
        Err(MetaError::ReadLimit)
    ));
    assert!(matches!(
        db.open_read_batch(
            &session,
            &store,
            &[session.root(), session.root()],
            MAX_READ_BYTES
        )
        .await,
        Err(MetaError::Invalid(_))
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn corrupt_bytes_or_broken_catalog_edge_cannot_produce_verified_children() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    let leases = tree(&mut db, &scope, &store).await;
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    let nodes = files(&mut db, &session, &store).await;
    let batch = db
        .open_read_batch(&session, &store, &nodes, MAX_READ_BYTES)
        .await
        .unwrap();
    let hex = leases[0].key().id().hex();
    let path = root
        .path()
        .join("managed-v1/1/objects")
        .join(scope.tenant())
        .join(&hex[..2])
        .join(&hex[2..]);
    let mut contents = std::fs::read(&path).unwrap();
    *contents.last_mut().unwrap() ^= 1;
    std::fs::write(&path, &contents).unwrap();
    assert!(matches!(batch.read(), Err(MetaError::Objects(_))));
    let raw = connection(&schema).await;
    raw.execute(
        "DELETE FROM th_object_edges WHERE child=$1",
        &[&leases[1].key().id().as_bytes().to_vec()],
    )
    .await
    .unwrap();
    let node = nodes
        .iter()
        .find(|n| n.id() == leases[1].key().id())
        .unwrap();
    assert!(matches!(
        db.open_read_batch(&session, &store, std::slice::from_ref(node), MAX_READ_BYTES)
            .await,
        Err(MetaError::ReadScope)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn renewal_preserves_reader_after_unpin_but_close_releases_retention() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    let leases = tree(&mut db, &scope, &store).await;
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    let node = session.root();
    pin(&mut db, &scope, None).await;
    let raw = connection(&schema).await;
    raw.batch_execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()+interval '30 seconds'",
    )
    .await
    .unwrap();
    db.renew_read_session(&session, 60).await.unwrap();
    assert!(raw.query_one("SELECT bool_and(expires_at>clock_timestamp()+interval '50 seconds') FROM th_object_holds", &[]).await.unwrap().get::<_, bool>(0));
    for lease in &leases {
        assert!(
            db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
    }
    let batch = db
        .open_read_batch(&session, &store, &[node], MAX_READ_BYTES)
        .await
        .unwrap();
    db.close_read_session(session).await.unwrap();
    let root_lease = leases.last().unwrap();
    let retirement = db
        .try_retire_object(root_lease.key(), 0)
        .await
        .unwrap()
        .unwrap();
    db.finish_retirement(&store, &retirement).await.unwrap();
    assert!(matches!(
        batch.read().unwrap()[0].object(),
        Object::Snapshot { .. }
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn current_snapshot_can_be_read_without_loading_either_merge_parent() {
    let (schema, scope, mut db) = setup().await;
    let root = tempfile::tempdir().unwrap();
    let store = depot(&root, scope.tenant(), "local-1");
    let leases = tree(&mut db, &scope, &store).await;
    let snapshot = leases.last().unwrap().key().id();
    let ops = upload(
        &mut db,
        &store,
        &Object::ChangeOperations(Changes::new(vec![]).unwrap()),
    )
    .await;
    let commit = |parents, message: &str| {
        Object::NativeCommit(Commit {
            parents,
            snapshot,
            changes: ops.key().id(),
            author_name: "Test".into(),
            author_email: "test@example.invalid".into(),
            timestamp_seconds: 1,
            timezone_minutes: 0,
            message: message.into(),
        })
    };
    let left = upload(&mut db, &store, &commit(vec![], "left")).await;
    let right = upload(&mut db, &store, &commit(vec![], "right")).await;
    let head = upload(
        &mut db,
        &store,
        &commit(vec![left.key().id(), right.key().id()], "head"),
    )
    .await;
    pin(&mut db, &scope, None).await;
    pin(&mut db, &scope, Some(&head)).await;
    for lease in [&ops, &left, &right, &head] {
        db.release_object_hold(lease).await.unwrap();
    }
    let session = db.open_read_session(&scope, "source", 60).await.unwrap();
    // Inject missing historical storage to prove the current-tree reader does
    // not traverse it. This is not a claim that the repository is healthy.
    for lease in [&left, &right] {
        store
            .remove_retired_generation(1, lease.key().id())
            .unwrap();
    }
    let frontier = children(&mut db, &session, &store, session.root()).await;
    let snapshot_node = frontier
        .iter()
        .find(|n| n.id() == snapshot)
        .unwrap()
        .clone();
    let dirs = children(&mut db, &session, &store, snapshot_node).await;
    let nodes = children(&mut db, &session, &store, dirs[0].clone()).await;
    assert_eq!(
        db.open_read_batch(&session, &store, &nodes, MAX_READ_BYTES)
            .await
            .unwrap()
            .read()
            .unwrap()
            .len(),
        2
    );
    let parent = frontier
        .iter()
        .find(|n| n.id() == right.key().id())
        .unwrap();
    assert!(matches!(
        db.open_read_batch(
            &session,
            &store,
            std::slice::from_ref(parent),
            MAX_READ_BYTES
        )
        .await,
        Err(MetaError::Objects(Error::MissingObject(_)))
    ));
    assert!(
        db.try_retire_object(right.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    cleanup(&schema).await;
}
