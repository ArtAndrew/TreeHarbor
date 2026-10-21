mod support;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use support::{Fixture, database};
use treeharbor_core::SourceProfile;
use treeharbor_materializer::Limits;
use treeharbor_metadata::{
    Metadata, Scope,
    access::{Permission, SecretToken},
    intake::{WorkspaceFormat, WorkspaceSpec},
};
use treeharbor_node::{Node, Prepare};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn real_preparation_replay_and_restart_preserve_edits_and_distinct_placements() {
    let mut f = Fixture::new().await;
    let node = f.node.take().unwrap();
    let mut heartbeat = f.metadata().await;
    let plan = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
    assert_eq!(plan.workspace(), f.workspace);
    let tree = node
        .prepare(
            &mut f.db,
            &mut heartbeat,
            Prepare {
                authorization: &f.auth,
                plan: &plan,
                store: &f.store,
                limits: Limits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(fs::read(tree.path().join("a")).unwrap(), b"base contents");
    let ready = f.db.prepared_workspace(&f.auth, f.workspace).await.unwrap();
    assert_eq!(ready.attempt, plan.attempt());
    assert_eq!(ready.slot, plan.slot());
    assert!(node.claim(&mut f.db, &f.auth).await.unwrap().is_none());
    fs::write(tree.path().join("a"), b"agent edit").unwrap();
    let replay = node
        .prepare(
            &mut f.db,
            &mut heartbeat,
            Prepare {
                authorization: &f.auth,
                plan: &plan,
                store: &f.store,
                limits: Limits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(replay.path(), tree.path());
    assert!(replay.statistics().is_none());
    drop(node);
    let node = f.reopen();
    let reopened = node
        .open_prepared(&mut f.db, &f.auth, f.workspace, Limits::default())
        .await
        .unwrap();
    assert_eq!(fs::read(reopened.path().join("a")).unwrap(), b"agent edit");
    let second = Uuid::new_v4();
    f.db.create_workspace(
        &f.context("second"),
        &WorkspaceSpec {
            id: second,
            base: f.base,
            profile: *SourceProfile::full().id().as_bytes(),
            format: WorkspaceFormat::Native,
        },
    )
    .await
    .unwrap();
    let second_plan = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
    let second_tree = node
        .prepare(
            &mut f.db,
            &mut heartbeat,
            Prepare {
                authorization: &f.auth,
                plan: &second_plan,
                store: &f.store,
                limits: Limits::default(),
            },
        )
        .await
        .unwrap();
    assert_ne!(second_tree.path(), tree.path());
    assert_eq!(
        fs::read(second_tree.path().join("a")).unwrap(),
        b"base contents"
    );
    assert_eq!(
        fs::read(second_tree.manifest_path()).unwrap(),
        fs::read(tree.manifest_path()).unwrap()
    );
    // An identical baseline cannot make another placement's publication seal valid.
    let a = tree.manifest_path().with_file_name("publication.json");
    let b = second_tree
        .manifest_path()
        .with_file_name("publication.json");
    let original = fs::read(&b).unwrap();
    fs::write(&b, fs::read(a).unwrap()).unwrap();
    assert!(
        node.open_prepared(&mut f.db, &f.auth, second, Limits::default())
            .await
            .is_err()
    );
    fs::write(&b, original).unwrap();
    f.db.drain_workspace(&f.context("drain"), f.workspace, 1)
        .await
        .unwrap();
    assert!(
        node.open_prepared(&mut f.db, &f.auth, f.workspace, Limits::default())
            .await
            .is_err()
    );
    assert_eq!(fs::read(tree.path().join("a")).unwrap(), b"agent edit");
    f.db.revoke_repository_token(&f.scope, f.token.id, "operator")
        .await
        .unwrap();
    assert!(
        node.open_prepared(&mut f.db, &f.auth, second, Limits::default())
            .await
            .is_err()
    );
    drop(node);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL"]
async fn two_concurrent_workspaces_have_independent_files_and_current_epoch_is_required() {
    let mut f = Fixture::new().await;
    let node = f.node.take().unwrap();
    let second = Uuid::new_v4();
    f.db.create_workspace(
        &f.context("second-concurrent"),
        &WorkspaceSpec {
            id: second,
            base: f.base,
            profile: *SourceProfile::full().id().as_bytes(),
            format: WorkspaceFormat::Native,
        },
    )
    .await
    .unwrap();
    let a = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
    let b = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
    let (mut db_a, mut hb_a, mut db_b, mut hb_b) = (
        f.metadata().await,
        f.metadata().await,
        f.metadata().await,
        f.metadata().await,
    );
    let (a, b) = tokio::join!(
        node.prepare(
            &mut db_a,
            &mut hb_a,
            Prepare {
                authorization: &f.auth,
                plan: &a,
                store: &f.store,
                limits: Limits::default()
            }
        ),
        node.prepare(
            &mut db_b,
            &mut hb_b,
            Prepare {
                authorization: &f.auth,
                plan: &b,
                store: &f.store,
                limits: Limits::default()
            }
        )
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    fs::write(a.path().join("a"), b"first agent").unwrap();
    fs::write(b.path().join("a"), b"second agent").unwrap();
    assert_eq!(fs::read(a.path().join("a")).unwrap(), b"first agent");
    assert_eq!(fs::read(b.path().join("a")).unwrap(), b"second agent");
    database::connection(&f.schema)
        .await
        .execute("UPDATE th_repositories SET epoch=epoch+1", &[])
        .await
        .unwrap();
    assert!(
        node.open_prepared(&mut f.db, &f.auth, f.workspace, Limits::default())
            .await
            .is_err()
    );
    assert_eq!(fs::read(a.path().join("a")).unwrap(), b"first agent");
    drop(node);
    f.cleanup().await;
}

#[test]
#[ignore = "requires disposable PostgreSQL; observes a real 15-second renewal"]
fn reservation_renews_while_the_filesystem_executor_is_blocked() {
    tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap().block_on(async {
        let mut f = Fixture::new().await;
        let node = f.node.take().unwrap();
        let plan = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
        let attempt = plan.attempt();
        let sql = database::connection(&f.schema).await;
        let initial: String = sql.query_one("SELECT o.lease_until::text FROM th_outbox o JOIN th_workspace_preparations p ON p.job_id=o.id WHERE p.attempt=$1", &[&attempt]).await.unwrap().get(0);
        let (mut db, mut heartbeat) = (f.metadata().await, f.metadata().await);
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, entered) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || { started.send(()).unwrap(); blocked.recv().unwrap(); });
        entered.await.unwrap();
        let runner = node.clone();
        let auth = f.auth.clone();
        let store = f.store.clone();
        let task = tokio::spawn(async move {
            runner.prepare(&mut db, &mut heartbeat, Prepare { authorization:&auth, plan:&plan, store:&store, limits:Limits::default() }).await
        });
        let deadline = Instant::now() + Duration::from_secs(25);
        loop {
            let renewed: bool = sql.query_one("SELECT o.lease_until > $2::text::timestamptz + interval '10 seconds' FROM th_outbox o JOIN th_workspace_preparations p ON p.job_id=o.id WHERE p.attempt=$1", &[&attempt,&initial]).await.unwrap().get(0);
            if renewed { break; }
            assert!(!task.is_finished());
            assert!(Instant::now() < deadline, "reservation did not renew during blocked filesystem work");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        release.send(()).unwrap();
        blocker.await.unwrap();
        let tree = tokio::time::timeout(Duration::from_secs(10), task).await.unwrap().unwrap().unwrap();
        assert_eq!(fs::read(tree.path().join("a")).unwrap(), b"base contents");
        drop(node);
        f.cleanup().await;
    });
}

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
impl Process {
    fn kill(&mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}
async fn child_database(schema: &str) -> Metadata {
    let connection = database::connection(schema).await;
    connection
        .query_one(
            "SELECT set_config('application_name',$1,false)",
            &[&format!("node-{schema}")],
        )
        .await
        .unwrap();
    Metadata::new(connection).await.unwrap()
}

async fn process_helper() {
    let Some(input) = std::env::var_os("TH_NODE_TEST_INPUT") else {
        return;
    };
    let config: serde_json::Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let text = |key: &str| config[key].as_str().unwrap();
    let scope = Scope::new(text("tenant"), text("repository").parse().unwrap()).unwrap();
    let mut db = child_database(text("schema")).await;
    let mut heartbeat = child_database(text("schema")).await;
    let auth = db
        .authorize(
            &SecretToken::parse(text("token")).unwrap(),
            &scope,
            Permission::RuntimeControl,
        )
        .await
        .unwrap();
    let node = Node::open(
        &PathBuf::from(text("root")),
        scope.tenant(),
        "node-1",
        text("storage").parse().unwrap(),
        4,
    )
    .unwrap();
    let store = ManagedStore::open(
        text("objects"),
        TenantId::new(scope.tenant()).unwrap(),
        "node-tests",
    )
    .unwrap();
    let plan = node.claim(&mut db, &auth).await.unwrap().unwrap();
    node.prepare(
        &mut db,
        &mut heartbeat,
        Prepare {
            authorization: &auth,
            plan: &plan,
            store: &store,
            limits: Limits::default(),
        },
    )
    .await
    .unwrap();
    fs::write(text("finished"), b"acknowledged").unwrap();
    std::future::pending::<()>().await;
}

fn start(f: &Fixture) -> Process {
    let input = f.directory.path().join("child.json");
    fs::write(&input, serde_json::to_vec(&json!({"schema":f.schema,"tenant":f.scope.tenant(),
        "repository":f.scope.repository().to_string(),"token":f.token.secret.expose(),
        "root":f.directory.path().join("node"),"storage":f.storage.to_string(),"objects":f.objects.path(),
        "finished":f.directory.path().join("finished")})).unwrap()).unwrap();
    fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
    let log = fs::File::create(f.directory.path().join("child.log")).unwrap();
    Process(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process_loss_after_acknowledgment_reopens_ready_tree_without_reexecuting_job",
                "--ignored",
                "--nocapture",
            ])
            .env("TH_NODE_TEST_INPUT", input)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    )
}

async fn crash(before_publication: bool) {
    let mut f = Fixture::new().await;
    drop(f.node.take());
    let gate = database::connection(&f.schema).await;
    let key = i64::from_be_bytes(Uuid::new_v4().as_bytes()[..8].try_into().unwrap());
    gate.query_one("SELECT pg_advisory_lock($1)", &[&key])
        .await
        .unwrap();
    let trigger = if before_publication {
        "BEFORE INSERT ON th_object_holds FOR EACH ROW"
    } else {
        "BEFORE UPDATE OF state ON th_workspaces FOR EACH ROW WHEN (NEW.state='ready')"
    };
    gate.batch_execute(&format!("CREATE FUNCTION node_pause() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({key}::bigint); RETURN NEW; END $$; CREATE TRIGGER node_pause {trigger} EXECUTE FUNCTION node_pause()" )).await.unwrap();
    let mut child = start(&f);
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let paused: bool = gate.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event='advisory')",
            &[&format!("node-{}", f.schema)]).await.unwrap().get(0);
        if paused {
            break;
        }
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child stopped before fault boundary"
        );
        assert!(Instant::now() < deadline, "did not reach fault boundary");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let row = gate
        .query_one(
            "SELECT attempt,slot FROM th_workspace_preparations WHERE workspace=$1",
            &[&f.workspace],
        )
        .await
        .unwrap();
    let attempt: Uuid = row.get(0);
    let slot: Uuid = row.get(1);
    let slot_path = f.directory.path().join("node/slots").join(slot.to_string());
    let intent = fs::read(slot_path.join("intent.json")).unwrap();
    assert_eq!(slot_path.join("tree").exists(), !before_publication);
    if before_publication {
        assert_eq!(
            fs::read_dir(slot_path.join("staging").join(f.scope.tenant()))
                .unwrap()
                .count(),
            1
        );
    }
    let original = if before_publication {
        None
    } else {
        Some(fs::read(slot_path.join("tree/publication.json")).unwrap())
    };
    let original_inode = if before_publication {
        None
    } else {
        Some(fs::metadata(slot_path.join("tree/files/a")).unwrap().ino())
    };
    child.kill();
    gate.query_one("SELECT pg_advisory_unlock($1)", &[&key])
        .await
        .unwrap();
    gate.batch_execute("DROP FUNCTION node_pause() CASCADE")
        .await
        .unwrap();
    f.expire().await;
    let node = f.reopen();
    let plan = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
    assert_eq!(plan.attempt(), attempt);
    assert_eq!(plan.slot().slot, slot);
    let mut heartbeat = f.metadata().await;
    let tree = node
        .prepare(
            &mut f.db,
            &mut heartbeat,
            Prepare {
                authorization: &f.auth,
                plan: &plan,
                store: &f.store,
                limits: Limits::default(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        tree.path(),
        slot_path.join("tree/files").canonicalize().unwrap()
    );
    assert_eq!(fs::read(tree.path().join("a")).unwrap(), b"base contents");
    if let Some(inode) = original_inode {
        assert_eq!(fs::metadata(tree.path().join("a")).unwrap().ino(), inode);
    }
    assert_eq!(fs::read(slot_path.join("intent.json")).unwrap(), intent);
    assert_eq!(
        fs::read_dir(slot_path.join("staging").join(f.scope.tenant()))
            .unwrap()
            .count(),
        0
    );
    if let Some(original) = original {
        assert_eq!(
            fs::read(slot_path.join("tree/publication.json")).unwrap(),
            original
        );
    }
    assert_eq!(
        f.db.prepared_workspace(&f.auth, f.workspace)
            .await
            .unwrap()
            .attempt,
        attempt
    );
    drop(node);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; kills a real worker"]
async fn process_loss_before_publication_cleans_only_unpublished_staging_and_reuses_attempt() {
    crash(true).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; kills a real worker"]
async fn process_loss_after_publication_reuses_durable_tree_and_receipt_before_acknowledging() {
    crash(false).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; kills a real worker"]
async fn process_loss_after_acknowledgment_reopens_ready_tree_without_reexecuting_job() {
    if std::env::var_os("TH_NODE_TEST_INPUT").is_some() {
        process_helper().await;
        return;
    }
    let mut f = Fixture::new().await;
    drop(f.node.take());
    let mut child = start(&f);
    let deadline = Instant::now() + Duration::from_secs(12);
    while !f.directory.path().join("finished").exists() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child stopped before acknowledgment"
        );
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill();
    let node = f.reopen();
    assert!(node.claim(&mut f.db, &f.auth).await.unwrap().is_none());
    let tree = node
        .open_prepared(&mut f.db, &f.auth, f.workspace, Limits::default())
        .await
        .unwrap();
    fs::remove_file(tree.path().join("a")).unwrap();
    fs::write(tree.path().join("untracked"), b"retained").unwrap();
    drop(node);
    let node = f.reopen();
    let reopened = node
        .open_prepared(&mut f.db, &f.auth, f.workspace, Limits::default())
        .await
        .unwrap();
    assert_eq!(reopened.path(), tree.path());
    assert!(!reopened.path().join("a").exists());
    assert_eq!(
        fs::read(reopened.path().join("untracked")).unwrap(),
        b"retained"
    );
    drop(node);
    f.cleanup().await;
}

#[test]
#[ignore = "requires disposable PostgreSQL; exercises renewal against a blocked completion transaction"]
fn renewal_waiting_for_completion_lock_does_not_stop_commit_progress() {
    tokio::runtime::Builder::new_current_thread().enable_all().max_blocking_threads(1).build().unwrap().block_on(async {
        let mut f = Fixture::new().await;
        let node = f.node.take().unwrap();
        let plan = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
        let gate = database::connection(&f.schema).await;
        let key = (u64::from_be_bytes(Uuid::new_v4().as_bytes()[..8].try_into().unwrap()) & i64::MAX as u64) as i64;
        gate.query_one("SELECT pg_advisory_lock($1)", &[&key]).await.unwrap();
        // Only this test trigger extends the primary transaction's five-second
        // lock timeout so the production renewal interval can actually overlap.
        gate.batch_execute(&format!("CREATE FUNCTION completion_pause() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM set_config('lock_timeout','25s',true); PERFORM pg_advisory_xact_lock({key}::bigint); RETURN NEW; END $$; CREATE TRIGGER completion_pause BEFORE UPDATE OF state ON th_workspaces FOR EACH ROW WHEN (NEW.state='ready') EXECUTE FUNCTION completion_pause()" )).await.unwrap();
        let completion_connection = database::connection(&f.schema).await;
        let completion_pid: i32 = completion_connection.query_one("SELECT pg_backend_pid()", &[]).await.unwrap().get(0);
        let mut db = Metadata::new(completion_connection).await.unwrap();
        let heartbeat_connection = database::connection(&f.schema).await;
        let heartbeat_name = format!("hb-{}", f.schema);
        heartbeat_connection.query_one("SELECT set_config('application_name',$1,false)", &[&heartbeat_name]).await.unwrap();
        let mut heartbeat = Metadata::new(heartbeat_connection).await.unwrap();
        let (release, blocked) = std::sync::mpsc::channel();
        let (started, entered) = tokio::sync::oneshot::channel();
        let blocker = tokio::task::spawn_blocking(move || { started.send(()).unwrap(); blocked.recv().unwrap(); });
        entered.await.unwrap();
        let runner = node.clone();
        let auth = f.auth.clone();
        let store = f.store.clone();
        let task = tokio::spawn(async move {
            runner.prepare(&mut db, &mut heartbeat, Prepare { authorization:&auth, plan:&plan, store:&store, limits:Limits::default() }).await
        });
        // Start the completion query later than the lease timer, so its 15-second
        // SQL timeout cannot fire at the first 15-second renewal tick.
        tokio::time::sleep(Duration::from_secs(2)).await;
        release.send(()).unwrap();
        blocker.await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let waiting: bool = gate.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE application_name=$1 AND wait_event_type='Lock' AND $2=ANY(pg_blocking_pids(pid)))", &[&heartbeat_name,&completion_pid]).await.unwrap().get(0);
            if waiting { break; }
            if task.is_finished() {
                panic!("worker stopped before lock overlap: {:?}", task.await.unwrap().err());
            }
            assert!(Instant::now() < deadline, "renewal did not contend with completion");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        gate.query_one("SELECT pg_advisory_unlock($1)", &[&key]).await.unwrap();
        let tree = tokio::time::timeout(Duration::from_secs(4), task).await.unwrap().unwrap().unwrap();
        assert_eq!(fs::read(tree.path().join("a")).unwrap(), b"base contents");
        assert_eq!(f.db.prepared_workspace(&f.auth, f.workspace).await.unwrap().workspace, f.workspace);
        drop(node);
        f.cleanup().await;
    });
}
