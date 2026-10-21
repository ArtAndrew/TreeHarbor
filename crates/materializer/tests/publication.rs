#[path = "../../metadata/tests/common/mod.rs"]
mod common;
use rustix::fs::{FlockOperation, flock};
use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    sync::Arc,
    time::Duration,
};
use treeharbor_core::ObjectId;
use treeharbor_materializer::{Limits, Materializer, Preparation, Publication, Selection};
use treeharbor_storage::{ManagedStore, TenantId};

#[test]
#[ignore = "requires disposable PostgreSQL; uses a real blocked IO executor"]
fn cancelling_a_queued_stage_retains_its_external_lock_until_stage_cleanup() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let (schema, scope, mut db) = common::setup().await;
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let objects = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let store = ManagedStore::open(
            objects.path(),
            TenantId::new(scope.tenant()).unwrap(),
            "cancel-test",
        )
        .unwrap();
        let lock_path = root.path().join("node.lock");
        let lock = Arc::new(
            fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&lock_path)
                .unwrap(),
        );
        flock(&*lock, FlockOperation::NonBlockingLockExclusive).unwrap();
        let weak = Arc::downgrade(&lock);
        let publication = Publication::new(root.path().join("tree"), [1; 32], lock);
        let materializer = Materializer::open(root.path(), 1).unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, entered) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            started.send(()).unwrap();
            blocked.recv().unwrap();
        });
        entered.await.unwrap();
        let (polling, polled) = tokio::sync::oneshot::channel();
        let tenant = scope.tenant().to_owned();
        let request = tokio::spawn(async move {
            // On this current-thread runtime the receiver runs after this task
            // yields at its queued blocking stage, not between these statements.
            polling.send(()).unwrap();
            materializer
                .prepare_published(
                    &mut db,
                    &store,
                    Preparation {
                        scope: &scope,
                        holder: "unused",
                        snapshot: ObjectId::from_bytes([3; 32]),
                        selection: Selection::full(),
                        limits: Limits::default(),
                    },
                    publication,
                )
                .await
        });
        polled.await.unwrap();
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        assert!(weak.upgrade().is_some());
        let competitor = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&lock_path)
            .unwrap();
        assert!(flock(&competitor, FlockOperation::NonBlockingLockExclusive).is_err());
        release.send(()).unwrap();
        blocker.await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), async {
            while weak.upgrade().is_some() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
        flock(&competitor, FlockOperation::NonBlockingLockExclusive).unwrap();
        assert_eq!(fs::read_dir(root.path().join(tenant)).unwrap().count(), 0);
        assert!(!root.path().join("tree").exists());
        common::cleanup(&schema).await;
    });
}
