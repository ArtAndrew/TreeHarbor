#[path = "../../metadata/tests/common/mod.rs"]
mod common;
use common::{cleanup, connection, setup};
use std::{
    collections::BTreeMap,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt, symlink},
    },
};
use treeharbor_core::*;
use treeharbor_materializer::{Error as BuildError, Limits, Materializer, Preparation, Selection};
use treeharbor_metadata::catalog::ObjectLease;
use treeharbor_metadata::{Authority, Begin, Metadata, Request, Scope};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

fn private_temp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
fn store(root: &tempfile::TempDir, scope: &Scope) -> ManagedStore {
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    ManagedStore::open(
        root.path(),
        TenantId::new(scope.tenant()).unwrap(),
        "local-1",
    )
    .unwrap()
}
async fn upload(db: &mut Metadata, store: &ManagedStore, object: Object) -> ObjectLease {
    let object = PreparedObject::new(&object).unwrap();
    let lease = db
        .reserve_object(store, &object, Uuid::new_v4(), 60)
        .await
        .unwrap();
    if lease.requires_upload() {
        db.publish_object(
            &lease,
            &store.put(lease.key().generation(), &object).unwrap(),
        )
        .await
        .unwrap();
    }
    lease
}
async fn fixture(
    db: &mut Metadata,
    scope: &Scope,
    store: &ManagedStore,
    files: &[(&[u8], EntryMode, &[u8])],
) -> (ObjectId, Vec<ObjectLease>) {
    let mut dirs: BTreeMap<Vec<u8>, Vec<Entry>> = BTreeMap::from([(vec![], vec![])]);
    let mut leases = Vec::new();
    for (path, mode, content) in files {
        let blob = upload(db, store, Object::Blob(content.to_vec())).await;
        let mut parent = Vec::new();
        let parts: Vec<_> = path.split(|b| *b == b'/').collect();
        for name in &parts[..parts.len() - 1] {
            if !parent.is_empty() {
                parent.push(b'/');
            }
            parent.extend_from_slice(name);
            dirs.entry(parent.clone()).or_default();
        }
        dirs.get_mut(&parent).unwrap().push(Entry {
            name: Name::new(parts.last().unwrap().to_vec()).unwrap(),
            value: EntryRef {
                mode: *mode,
                target: blob.key().id(),
            },
        });
        leases.push(blob);
    }
    let mut paths: Vec<_> = dirs.keys().cloned().collect();
    paths.sort_by_key(|p| std::cmp::Reverse(p.len()));
    let mut root_id = None;
    for path in paths {
        let object = Object::Directory(Directory::new(dirs.remove(&path).unwrap()).unwrap());
        let lease = upload(db, store, object).await;
        if path.is_empty() {
            root_id = Some(lease.key().id());
        } else {
            let (parent, name) = match path.iter().rposition(|b| *b == b'/') {
                Some(i) => (&path[..i], &path[i + 1..]),
                None => (&[][..], &path[..]),
            };
            dirs.get_mut(parent).unwrap().push(Entry {
                name: Name::new(name.to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Directory,
                    target: lease.key().id(),
                },
            });
        }
        leases.push(lease);
    }
    let snapshot = upload(
        db,
        store,
        Object::Snapshot {
            root: root_id.unwrap(),
        },
    )
    .await;
    let id = snapshot.key().id();
    let request = Request::new(
        scope.clone(),
        &Uuid::new_v4().to_string(),
        "fixture",
        "pin",
        Authority::Standalone,
        1,
        b"fixture",
    )
    .unwrap();
    let Begin::Fresh(tx) = db.begin(request).await.unwrap() else {
        panic!()
    };
    tx.pin_object("source", &snapshot).await.unwrap();
    tx.complete(serde_json::json!({}), vec![]).await.unwrap();
    leases.push(snapshot);
    for lease in &leases {
        db.release_object_hold(lease).await.unwrap();
    }
    (id, leases)
}
fn req(scope: &Scope, snapshot: ObjectId, selection: Selection) -> Preparation<'_> {
    Preparation {
        scope,
        holder: "source",
        snapshot,
        selection,
        limits: Limits::default(),
    }
}
fn sparse(paths: &[&[u8]]) -> Selection {
    Selection::sparse(
        paths
            .iter()
            .map(|p| RepoPath::new(p.to_vec()).unwrap())
            .collect(),
    )
    .unwrap()
}
fn remaining(root: &tempfile::TempDir, scope: &Scope) -> usize {
    fs::read_dir(root.path().join(scope.tenant()))
        .unwrap()
        .count()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn two_private_trees_preserve_bytes_modes_links_and_binary_names() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let files: Vec<(&[u8], EntryMode, &[u8])> = vec![
        (b"README", EntryMode::File, b"base"),
        (b"bin/run", EntryMode::Executable, b"#!/bin/sh\nexit 0\n"),
        (b"bin/readme", EntryMode::Symlink, b"../README"),
        (b"binary", EntryMode::File, b"\0\xff"),
    ];
    let (snapshot, leases) = fixture(&mut db, &scope, &store, &files).await;
    let operations = upload(
        &mut db,
        &store,
        Object::ChangeOperations(Changes::new(vec![]).unwrap()),
    )
    .await;
    let commit = upload(
        &mut db,
        &store,
        Object::NativeCommit(Commit {
            git_origin: None,
            parents: vec![],
            snapshot,
            changes: operations.key().id(),
            author_name: "Fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 1,
            timezone_minutes: 0,
            message: "native base".into(),
        }),
    )
    .await;
    let request = Request::new(
        scope.clone(),
        "native-root",
        "fixture",
        "pin",
        Authority::Standalone,
        1,
        b"native-root",
    )
    .unwrap();
    let Begin::Fresh(tx) = db.begin(request).await.unwrap() else {
        panic!()
    };
    tx.unpin_object("source").await.unwrap();
    tx.pin_object("source", &commit).await.unwrap();
    tx.complete(serde_json::json!({}), vec![]).await.unwrap();
    db.release_object_hold(&operations).await.unwrap();
    db.release_object_hold(&commit).await.unwrap();
    let output = private_temp();
    let builder = Materializer::open(output.path(), 2).unwrap();
    let mut other_db = Metadata::new(connection(&schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        builder.prepare(&mut db, &store, req(&scope, snapshot, Selection::full())),
        builder.prepare(
            &mut other_db,
            &store,
            req(&scope, snapshot, Selection::full())
        )
    );
    let a = a.unwrap();
    let b = b.unwrap();
    for tree in [&a, &b] {
        for (path, mode, contents) in &files {
            let path = tree.path().join(std::ffi::OsStr::from_bytes(path));
            if *mode == EntryMode::Symlink {
                assert_eq!(
                    fs::read_link(path).unwrap().as_os_str().as_bytes(),
                    *contents
                );
            } else {
                assert_eq!(fs::read(&path).unwrap(), *contents);
                assert_eq!(
                    fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0,
                    *mode == EntryMode::Executable
                );
            }
        }
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(tree.manifest_path()).unwrap()).unwrap();
        assert_eq!(manifest["header"]["snapshot"], snapshot.hex());
        assert_eq!(manifest["header"]["version"], 1);
        assert!(!tree.path().join(".git").exists());
    }
    assert_ne!(
        fs::metadata(a.path().join("README")).unwrap().ino(),
        fs::metadata(b.path().join("README")).unwrap().ino()
    );
    fs::write(a.path().join("README"), b"agent edit").unwrap();
    assert_eq!(fs::read(b.path().join("README")).unwrap(), b"base");
    assert_eq!(
        store
            .get(leases[0].key().generation(), leases[0].key().id())
            .unwrap(),
        Object::Blob(b"base".to_vec())
    );
    assert_eq!(
        connection(&schema)
            .await
            .query_one("SELECT count(*) FROM th_object_holds", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn expansion_preserves_edits_deletions_and_untracked_files_in_a_new_tree() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[
            (b"src/a", EntryMode::File, b"original"),
            (b"src/deleted", EntryMode::File, b"delete me"),
            (b"lib/needed", EntryMode::File, b"dependency"),
            (b"new", EntryMode::File, b"base new"),
        ],
    )
    .await;
    let output = private_temp();
    let builder = Materializer::open(output.path(), 1).unwrap();
    let a = builder
        .prepare(&mut db, &store, req(&scope, snapshot, sparse(&[b"src"])))
        .await
        .unwrap();
    assert!(!a.path().join("lib").exists());
    fs::write(a.path().join("src/a"), b"edited").unwrap();
    fs::remove_file(a.path().join("src/deleted")).unwrap();
    fs::write(a.path().join("new"), b"local untracked").unwrap();
    fs::write(a.path().join("generated"), b"build output").unwrap();
    // Discard all in-memory state and recover the independently observed exact
    // baseline. Reopening must not restore tracked deletions or erase edits.
    use sha2::{Digest, Sha256};
    use treeharbor_materializer::{BaselineExpectation, MaterializedTree};
    let root = a.manifest_path().parent().unwrap().to_owned();
    let digest = Sha256::digest(fs::read(a.manifest_path()).unwrap()).into();
    drop(a);
    drop(builder);
    let profile = SourceProfile::sparse(vec![RepoPath::new(b"src".to_vec()).unwrap()]).unwrap();
    let a = MaterializedTree::load_baseline(
        &root,
        BaselineExpectation {
            scope: &scope,
            snapshot,
            profile: &profile,
            manifest_digest: digest,
        },
        Limits::default(),
    )
    .unwrap();
    assert!(a.statistics().is_none());
    assert!(!a.preserved_local_state());
    let builder = Materializer::open(output.path(), 1).unwrap();
    let b = builder
        .expand_stopped(
            &mut db,
            &store,
            req(&scope, snapshot, Selection::full()),
            &a,
        )
        .await
        .unwrap();
    assert_eq!(fs::read(b.path().join("src/a")).unwrap(), b"edited");
    assert!(b.preserved_local_state());
    assert!(!b.path().join("src/deleted").exists());
    assert!(b.includes(&RepoPath::new(b"src/deleted".to_vec()).unwrap()));
    assert_eq!(
        fs::read(b.path().join("lib/needed")).unwrap(),
        b"dependency"
    );
    assert_eq!(fs::read(b.path().join("new")).unwrap(), b"local untracked");
    assert_eq!(
        fs::read(b.path().join("generated")).unwrap(),
        b"build output"
    );
    assert!(!a.path().join("lib").exists());
    fs::write(b.path().join("src/a"), b"second generation").unwrap();
    assert_eq!(fs::read(a.path().join("src/a")).unwrap(), b"edited");
    assert_eq!(
        b.statistics().unwrap().source_bytes_written,
        b"dependency".len() as u64
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn sparse_does_not_fetch_missing_unselected_blob_and_failed_expansion_keeps_old_tree() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, leases) = fixture(
        &mut db,
        &scope,
        &store,
        &[
            (b"a", EntryMode::File, b"available"),
            (b"b", EntryMode::File, b"missing"),
        ],
    )
    .await;
    store
        .remove_retired_generation(1, leases[1].key().id())
        .unwrap();
    let output = private_temp();
    let builder = Materializer::open(output.path(), 1).unwrap();
    let a = builder
        .prepare(&mut db, &store, req(&scope, snapshot, sparse(&[b"a"])))
        .await
        .unwrap();
    fs::write(a.path().join("a"), b"keep edit").unwrap();
    assert!(
        builder
            .expand_stopped(
                &mut db,
                &store,
                req(&scope, snapshot, Selection::full()),
                &a
            )
            .await
            .is_err()
    );
    assert_eq!(fs::read(a.path().join("a")).unwrap(), b"keep edit");
    assert_eq!(remaining(&output, &scope), 1);
    assert_eq!(
        connection(&schema)
            .await
            .query_one("SELECT count(*) FROM th_object_holds", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn missing_paths_wrong_snapshot_and_limits_never_publish_partial_trees() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[(b"src/a", EntryMode::File, b"payload")],
    )
    .await;
    let output = private_temp();
    let builder = Materializer::open(output.path(), 1).unwrap();
    assert!(matches!(
        builder
            .prepare(&mut db, &store, req(&scope, snapshot, sparse(&[b"absent"])))
            .await,
        Err(BuildError::MissingPath(_))
    ));
    assert!(matches!(
        builder
            .prepare(
                &mut db,
                &store,
                req(&scope, ObjectId::from_bytes([0; 32]), Selection::full())
            )
            .await,
        Err(BuildError::WrongSnapshot)
    ));
    for limits in [
        Limits {
            max_entries: 1,
            ..Limits::default()
        },
        Limits {
            max_workspace_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_source_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_path_bytes: 1,
            ..Limits::default()
        },
    ] {
        let mut r = req(&scope, snapshot, Selection::full());
        r.limits = limits;
        assert!(builder.prepare(&mut db, &store, r).await.is_err());
        assert_eq!(remaining(&output, &scope), 0);
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn unsafe_links_and_changed_parent_cannot_escape_or_overwrite_local_state() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[
            (b"src/a", EntryMode::File, b"a"),
            (b"src/new", EntryMode::File, b"must not escape"),
        ],
    )
    .await;
    let output = private_temp();
    let outside = private_temp();
    let builder = Materializer::open(output.path(), 1).unwrap();
    let a = builder
        .prepare(&mut db, &store, req(&scope, snapshot, sparse(&[b"src/a"])))
        .await
        .unwrap();
    fs::remove_dir_all(a.path().join("src")).unwrap();
    symlink(outside.path(), a.path().join("src")).unwrap();
    assert!(
        builder
            .expand_stopped(
                &mut db,
                &store,
                req(&scope, snapshot, Selection::full()),
                &a
            )
            .await
            .is_err()
    );
    assert!(!outside.path().join("new").exists());
    assert_eq!(remaining(&output, &scope), 1);
    // A regular-file obstruction is also preserved; no destructive repair.
    fs::remove_file(a.path().join("src")).unwrap();
    fs::write(a.path().join("src"), b"local replacement").unwrap();
    assert!(
        builder
            .expand_stopped(
                &mut db,
                &store,
                req(&scope, snapshot, Selection::full()),
                &a
            )
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(a.path().join("src")).unwrap(),
        b"local replacement"
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn case_collisions_are_rejected_when_the_target_filesystem_aliases_names() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[
            (b"File", EntryMode::File, b"upper"),
            (b"file", EntryMode::File, b"lower"),
        ],
    )
    .await;
    let output = private_temp();
    fs::write(output.path().join("CaseProbe"), b"probe").unwrap();
    let aliases = output.path().join("caseprobe").exists();
    fs::remove_file(output.path().join("CaseProbe")).unwrap();
    let builder = Materializer::open(output.path(), 1).unwrap();
    let a = builder
        .prepare(&mut db, &store, req(&scope, snapshot, sparse(&[b"File"])))
        .await
        .unwrap();
    let full = builder
        .expand_stopped(
            &mut db,
            &store,
            req(&scope, snapshot, Selection::full()),
            &a,
        )
        .await;
    if aliases {
        assert!(full.is_err());
        assert_eq!(remaining(&output, &scope), 1);
    } else {
        let full = full.unwrap();
        assert_eq!(fs::read(full.path().join("File")).unwrap(), b"upper");
        assert_eq!(fs::read(full.path().join("file")).unwrap(), b"lower");
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn cancelling_a_preparation_waiting_on_metadata_removes_staging_and_releases_slot() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[(b"a", EntryMode::File, b"retained")],
    )
    .await;
    let output = private_temp();
    let builder = Materializer::open(output.path(), 1).unwrap();
    let mut guard = connection(&schema).await;
    let tx = guard.transaction().await.unwrap();
    tx.query_one(
        "SELECT digest FROM th_objects WHERE digest=$1 FOR UPDATE",
        &[&snapshot.as_bytes().to_vec()],
    )
    .await
    .unwrap();
    let worker_client = connection(&schema).await;
    let pid: i32 = worker_client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let worker_builder = builder.clone();
    let worker_store = store.clone();
    let worker_scope = scope.clone();
    let worker = tokio::spawn(async move {
        let mut worker_db = Metadata::new(worker_client).await.unwrap();
        worker_builder
            .prepare(
                &mut worker_db,
                &worker_store,
                req(&worker_scope, snapshot, Selection::full()),
            )
            .await
    });
    let observer = connection(&schema).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: bool = observer.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')", &[&pid]).await.unwrap().get(0);
            if waiting { break; }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    }).await.unwrap();
    worker.abort();
    assert!(worker.await.unwrap_err().is_cancelled());
    tx.rollback().await.unwrap();
    assert_eq!(remaining(&output, &scope), 0);
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        builder.prepare(&mut db, &store, req(&scope, snapshot, Selection::full())),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(fs::read(result.path().join("a")).unwrap(), b"retained");
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn raw_names_are_preserved_or_rejected_without_silent_filename_conversion() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[(b"raw-\xff", EntryMode::File, b"bytes")],
    )
    .await;
    let output = private_temp();
    let probe = output
        .path()
        .join(std::ffi::OsStr::from_bytes(b"probe-\xff"));
    let supported = fs::write(&probe, b"probe").is_ok();
    if supported {
        fs::remove_file(&probe).unwrap();
    }
    let builder = Materializer::open(output.path(), 1).unwrap();
    let result = builder
        .prepare(&mut db, &store, req(&scope, snapshot, Selection::full()))
        .await;
    if supported {
        let tree = result.unwrap();
        assert_eq!(
            fs::read(tree.path().join(std::ffi::OsStr::from_bytes(b"raw-\xff"))).unwrap(),
            b"bytes"
        );
    } else {
        assert!(result.is_err());
        assert_eq!(remaining(&output, &scope), 0);
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn symlink_checks_use_actual_case_aliases_before_parent_traversal() {
    let (schema, scope, mut db) = setup().await;
    let objects = private_temp();
    let store = store(&objects, &scope);
    let (snapshot, _) = fixture(
        &mut db,
        &scope,
        &store,
        &[
            (b"Dir/Link", EntryMode::Symlink, b".."),
            (b"escape", EntryMode::Symlink, b"dir/link/../../outside"),
        ],
    )
    .await;
    let output = private_temp();
    fs::write(output.path().join("Probe"), b"probe").unwrap();
    let aliases = output.path().join("probe").exists();
    fs::remove_file(output.path().join("Probe")).unwrap();
    let builder = Materializer::open(output.path(), 1).unwrap();
    let result = builder
        .prepare(&mut db, &store, req(&scope, snapshot, Selection::full()))
        .await;
    if aliases {
        assert!(matches!(result, Err(BuildError::UnsafePath(_))));
        assert_eq!(remaining(&output, &scope), 0);
    } else {
        assert!(result.is_ok());
    } // This spelling is dangling on a case-sensitive filesystem.
    cleanup(&schema).await;
}
