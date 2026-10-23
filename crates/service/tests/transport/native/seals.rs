//! Real CLI/server/object durability with explicitly trusted managed observations.
//! Physical capture and writer removal require the separate gVisor acceptance job.
use super::*;
use std::collections::BTreeMap;
use treeharbor_core::{ChangeOperation, Entry, EntryMode, EntryRef, Name, RepoPath, SourceProfile};
use treeharbor_metadata::{
    access::Permission,
    preparations::{PreparedFiles, StorageSlot},
    runs::RuntimeVolume,
    seals::SealPublication,
};
use treeharbor_runtime::{Definition, Profile};

async fn managed_fixture(f: &mut Fixture, base: RevisionRef) -> (Uuid, Uuid, Uuid, Vec<Object>) {
    let node_path = role(f, "capture-node", Some("node"), &["runtime.control"]);
    let credential = Credential::load(&node_path).unwrap();
    let mut connection = Database::new(f.config.clone())
        .unwrap()
        .connect(true)
        .await
        .unwrap();
    let db = &mut connection.metadata;
    let node = db
        .authorize(&credential.token, &f.scope, Permission::RuntimeControl)
        .await
        .unwrap();
    let storage = Uuid::new_v4();
    db.register_node_storage(f.scope.tenant(), "capture-node", storage, "test-operator")
        .await
        .unwrap();
    let slot = StorageSlot {
        storage,
        slot: Uuid::new_v4(),
    };
    db.register_runtime_volume(
        f.scope.tenant(),
        "capture-node",
        &RuntimeVolume {
            slot,
            digest: [71; 32],
            limit_bytes: 134217728,
        },
        "test-operator",
    )
    .await
    .unwrap();
    let runtime = Profile::new(Definition {
        version: 1,
        image: format!("sha256:{}", "11".repeat(32)),
        uid: 1001,
        gid: 1001,
        memory_bytes: 268435456,
        nano_cpus: 1_000_000_000,
        max_processes: 64,
        timeout_ms: 10000,
        workspace_bytes: 134217728,
    })
    .unwrap();
    db.register_runtime_profile(&f.scope, &runtime, "test-operator")
        .await
        .unwrap();
    let profile = ok(f.cli(&[
        "profile-put",
        "--key",
        "seal-full",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--full",
    ]));
    let workspace = Uuid::new_v4();
    let run = Uuid::new_v4();
    let seal = Uuid::new_v4();
    ok(f.cli(&[
        "workspace-create",
        "--key",
        "seal-workspace",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &workspace.to_string(),
        "--base",
        &base.to_string(),
        "--profile-digest",
        profile["profile_digest"].as_str().unwrap(),
    ]));
    let job = db
        .claim_preparation(&node, storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    let prepared = db.reserve_preparation(&node, &job, slot).await.unwrap();
    let snapshot = Object::Snapshot {
        root: Object::Directory(Directory::new(vec![]).unwrap())
            .id()
            .unwrap(),
    }
    .id()
    .unwrap();
    db.complete_preparation(
        &node,
        &prepared,
        &PreparedFiles {
            snapshot,
            profile: SourceProfile::full().id(),
            manifest_digest: [81; 32],
        },
    )
    .await
    .unwrap();
    ok(f.cli(&[
        "run-request",
        "--key",
        "seal-run",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--run",
        &run.to_string(),
        "--workspace",
        &workspace.to_string(),
        "--generation",
        "1",
        "--profile-digest",
        &ObjectId::from_bytes(runtime.digest()).to_string(),
        "--agent",
        "test",
        "--task",
        "capture-fixture",
        "--",
        "/bin/task",
    ]));
    let args = [
        "seal-request",
        "--key",
        "seal-request",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--seal",
        &seal.to_string(),
        "--run",
        &run.to_string(),
        "--generation",
        "1",
        "--replace",
        "src",
    ];
    let requested = ok(f.cli(&args));
    assert_eq!(requested["state"], "waiting");
    assert_eq!(ok(f.cli(&args)), requested);
    let plan = db
        .claim_run(&node, storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    assert!(plan.stop_requested());
    db.finish_run(&node, &plan).await.unwrap();
    let plan = db
        .claim_workspace_seal(&node, storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    let blob = Object::Blob(b"agent output\n\0\xff".to_vec());
    let empty = Object::Directory(Directory::new(vec![]).unwrap());
    let subtree = Object::Directory(
        Directory::new(vec![
            Entry {
                name: Name::new(b"script".to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Executable,
                    target: blob.id().unwrap(),
                },
            },
            Entry {
                name: Name::new(b"empty".to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Directory,
                    target: empty.id().unwrap(),
                },
            },
        ])
        .unwrap(),
    );
    let changes = Changes::with_subtrees(vec![ChangeOperation {
        path: RepoPath::new(b"src".to_vec()).unwrap(),
        expected: None,
        replacement: Some(EntryRef {
            mode: EntryMode::Directory,
            target: subtree.id().unwrap(),
        }),
    }])
    .unwrap();
    let items = vec![
        blob,
        empty,
        subtree,
        Object::ChangeOperations(changes.clone()),
    ];
    let config = f.config.storage.as_ref().unwrap();
    let store = ManagedStore::open(
        &config.root,
        TenantId::new(f.scope.tenant()).unwrap(),
        &config.backend,
    )
    .unwrap();
    let spec = SealPublication::new(
        [91; 32],
        changes,
        items.iter().map(|o| o.id().unwrap()).collect(),
        BTreeMap::new(),
        items.iter().map(|o| o.encode().unwrap().len() as u64).sum(),
    )
    .unwrap();
    db.begin_seal_publication(&node, &plan, &store, &spec)
        .await
        .unwrap();
    for object in &items {
        let frame = PreparedObject::new(object).unwrap();
        let ticket = db
            .reserve_seal_object(&node, &plan, &store, &frame)
            .await
            .unwrap();
        let receipt = store.put(ticket.generation(), &frame).unwrap();
        db.publish_seal_object(&node, &plan, &ticket, &receipt)
            .await
            .unwrap();
    }
    db.finish_seal_publication(&node, &plan, [91; 32])
        .await
        .unwrap();
    assert_eq!(
        ok(f.cli(&["seal-status", "--seal", &seal.to_string()]))["state"],
        "ready"
    );
    (workspace, run, seal, items)
}
async fn wait_lock(f: &Fixture, lock: i64) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock & 0xffff_ffff)]).await.unwrap().get::<_,bool>(0) {break;}
        assert!(
            Instant::now() < deadline,
            "acceptance did not reach fault boundary"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; real CLI acceptance and server-loss recovery"]
async fn ready_capture_cli_accepts_exact_subtree_and_recovers_before_and_after_commit() {
    let mut f = Fixture::new().await;
    let identity = Uuid::new_v4();
    let mut db = Database::new(f.config.clone())
        .unwrap()
        .connect(true)
        .await
        .unwrap();
    db.metadata
        .bind_storage_backend("test-local", identity)
        .await
        .unwrap();
    drop(db);
    let base = f.seed().await;
    // The trusted seed populated this store before the HTTP service opened it.
    // Give that fixture store an identity; production refuses to adopt an
    // unidentified nonempty directory.
    files::write_private(
        &f.dir.path().join("objects/backend-id"),
        identity.to_string().as_bytes(),
    )
    .unwrap();
    f.config.storage = Some(treeharbor_service::config::StorageConfig {
        root: f.dir.path().join("objects"),
        backend: "test-local".into(),
    });
    f.config.request_timeout_ms = 15000;
    f.config.pool_size = 3;
    f.save_config();
    f.credential_path = role(
        &mut f,
        "agent-a",
        None,
        &["read", "workspace.write", "change.write"],
    );
    let mut server = f.start().await;
    let (workspace, run, seal, items) = managed_fixture(&mut f, base).await;
    let change = Uuid::new_v4();
    let credential = f.credential();
    let body = json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"key":"accept-seal","authority":"standalone","epoch":1,"seal":seal.to_string(),"changeset":change.to_string(),"previous_version":0,"ttl_seconds":300});
    for extra in ["operations", "result_snapshot", "principal"] {
        let mut bad = body.clone();
        bad[extra] = json!("forged");
        assert_eq!(
            client::send_seal_accept(
                &f.config.socket,
                &credential.token,
                bad.to_string().into_bytes(),
                Duration::from_secs(10)
            )
            .await
            .unwrap()
            .status
            .as_u16(),
            400
        );
    }
    let node = Credential::load(&f.dir.path().join("capture-node-native.json")).unwrap();
    assert_eq!(
        client::send_seal_accept(
            &f.config.socket,
            &node.token,
            body.to_string().into_bytes(),
            Duration::from_secs(10)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        403
    );
    let payload = body.to_string();
    let mut request=format!("POST /v1/workspaces/seals/accept HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",credential.token.expose(),payload.len()).into_bytes();
    request.extend(payload.as_bytes());
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION seal_hold_derived() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({lock}::bigint); RETURN NEW; END $$; CREATE TRIGGER seal_hold_derived BEFORE UPDATE ON th_objects FOR EACH ROW WHEN (OLD.state='uploading' AND NEW.state='live' AND NEW.kind=2) EXECUTE FUNCTION seal_hold_derived()")).await.unwrap();
    let mut partial = UnixStream::connect(&f.config.socket).await.unwrap();
    partial.write_all(&request).await.unwrap();
    wait_lock(&f, lock).await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(partial);
    f.sql
        .query_one("SELECT pg_advisory_unlock($1)", &[&lock])
        .await
        .unwrap();
    f.sql
        .batch_execute(
            "DROP TRIGGER seal_hold_derived ON th_objects; DROP FUNCTION seal_hold_derived()",
        )
        .await
        .unwrap();
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        f.sql
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM th_objects WHERE kind=2 AND state='uploading')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    server = f.start().await;
    let mut lost = UnixStream::connect(&f.config.socket).await.unwrap();
    lost.write_all(&request).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE key='accept-seal'",
                &[],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
            == 1
        {
            break;
        }
        assert!(Instant::now() < deadline, "acceptance did not commit");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(lost);
    server = f.start().await;
    let args = [
        "seal-accept",
        "--key",
        "accept-seal",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--seal",
        &seal.to_string(),
        "--changeset",
        &change.to_string(),
    ];
    let accepted = ok(f.cli(&args));
    assert_eq!(accepted["run"], run.to_string());
    assert_eq!(accepted["seal"], seal.to_string());
    assert_eq!(accepted["version"], 1);
    assert_eq!(accepted["base_revision"], base.to_string());
    assert_eq!(accepted["operations"], items[3].id().unwrap().to_string());
    let root = Object::Directory(
        Directory::new(vec![Entry {
            name: Name::new(b"src".to_vec()).unwrap(),
            value: EntryRef {
                mode: EntryMode::Directory,
                target: items[2].id().unwrap(),
            },
        }])
        .unwrap(),
    );
    let expected = Object::Snapshot {
        root: root.id().unwrap(),
    };
    assert_eq!(
        accepted["result_snapshot"],
        expected.id().unwrap().to_string()
    );
    let storage = f.config.storage.as_ref().unwrap();
    let store = ManagedStore::open(
        &storage.root,
        TenantId::new(f.scope.tenant()).unwrap(),
        &storage.backend,
    )
    .unwrap();
    for object in items.iter().chain([&root, &expected]) {
        let generation: i64 = f
            .sql
            .query_one(
                "SELECT generation FROM th_objects WHERE digest=$1 AND state='live'",
                &[&object.id().unwrap().as_bytes().as_slice()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            store.get(generation, object.id().unwrap()).unwrap(),
            *object
        );
    }
    let cancelled = Uuid::new_v4();
    ok(f.cli(&[
        "seal-request",
        "--key",
        "cancel-request",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--seal",
        &cancelled.to_string(),
        "--run",
        &run.to_string(),
        "--generation",
        "1",
        "--replace",
        "src",
    ]));
    assert_eq!(
        ok(f.cli(&[
            "seal-cancel",
            "--key",
            "cancel-seal",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--seal",
            &cancelled.to_string()
        ]))["state"],
        "cancelled"
    );
    ok(f.cli(&[
        "workspace-delete",
        "--key",
        "seal-drain",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &workspace.to_string(),
        "--generation",
        "1",
    ]));
    assert_eq!(ok(f.cli(&args)), accepted);
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='change.accepted'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_native_validations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    drop(server);
    f.cleanup().await;
}
