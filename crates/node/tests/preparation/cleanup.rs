use super::support;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    time::Duration,
};
use std::{
    path::PathBuf,
    process::{Command, Stdio},
    time::Instant,
};
use support::{Fixture, database};
use treeharbor_core::SourceProfile;
use treeharbor_materializer::Limits;
use treeharbor_metadata::{
    Scope,
    access::{Permission, SecretToken},
};
use treeharbor_metadata::{
    intake::{WorkspaceFormat, WorkspaceSpec},
    preparations::Preparation,
};
use treeharbor_node::{Node, Prepare};
use uuid::Uuid;

async fn prepare(f: &mut Fixture, node: &Node) -> Preparation {
    let plan = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
    let mut heartbeat = f.metadata().await;
    node.prepare(
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
    plan
}
async fn drain(f: &mut Fixture) {
    f.db.drain_workspace(&f.context("drain"), f.workspace, 1)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; actual file cleanup without an execution runtime"]
async fn cleanup_deletes_owned_files_retains_root_and_replay_does_not_touch_reused_slot() {
    let mut f = Fixture::new().await;
    let node = f.node.take().unwrap();
    let preparation = prepare(&mut f, &node).await;
    let root = f
        .directory
        .path()
        .join("node/slots")
        .join(preparation.slot().slot.to_string())
        .canonicalize()
        .unwrap();
    let inode = fs::metadata(&root).unwrap().ino();
    let files = root.join("tree/files");
    let sentinel = f.directory.path().join("sentinel");
    fs::write(&sentinel, b"retained").unwrap();
    symlink(&sentinel, files.join("outside")).unwrap();
    fs::create_dir(files.join("denied")).unwrap();
    fs::write(files.join("denied/file"), b"delete").unwrap();
    fs::set_permissions(files.join("denied"), fs::Permissions::from_mode(0o0)).unwrap();
    drain(&mut f).await;
    let plan = node
        .claim_cleanup(&mut f.db, &f.auth, f.workspace)
        .await
        .unwrap()
        .unwrap();
    let mut heartbeat = f.metadata().await;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        node.cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &plan, None),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["state"], "deleted");
    assert_eq!(fs::metadata(&root).unwrap().ino(), inode);
    assert!(fs::read_dir(&root).unwrap().next().is_none());
    assert_eq!(fs::read(&sentinel).unwrap(), b"retained");
    let next = Uuid::new_v4();
    f.db.create_workspace(
        &f.context("next"),
        &WorkspaceSpec {
            id: next,
            base: f.base,
            profile: *SourceProfile::full().id().as_bytes(),
            format: WorkspaceFormat::Native,
        },
    )
    .await
    .unwrap();
    let job =
        f.db.claim_preparation(&f.auth, f.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let successor =
        f.db.reserve_preparation(&f.auth, &job, preparation.slot())
            .await
            .unwrap();
    node.prepare(
        &mut f.db,
        &mut heartbeat,
        Prepare {
            authorization: &f.auth,
            plan: &successor,
            store: &f.store,
            limits: Limits::default(),
        },
    )
    .await
    .unwrap();
    fs::write(files.join("a"), b"new workspace edits").unwrap();
    drop(node);
    let restarted = f.reopen();
    assert_eq!(
        restarted
            .cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &plan, None)
            .await
            .unwrap(),
        result
    );
    assert_eq!(fs::read(files.join("a")).unwrap(), b"new workspace edits");
    assert_eq!(
        f.db.prepared_workspace(&f.auth, next)
            .await
            .unwrap()
            .attempt,
        successor.attempt()
    );
    drop(restarted);
    f.cleanup().await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; partial and unreserved cleanup"]
async fn cleanup_handles_unreserved_and_untouched_reservations_but_retains_unknown_files() {
    for state in ["unreserved", "untouched", "unknown"] {
        let mut f = Fixture::new().await;
        let node = f.node.take().unwrap();
        let mut path = None;
        if state != "unreserved" {
            let preparation = node.claim(&mut f.db, &f.auth).await.unwrap().unwrap();
            let root = f
                .directory
                .path()
                .join("node/slots")
                .join(preparation.slot().slot.to_string());
            if state == "unknown" {
                fs::create_dir(&root).unwrap();
                fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
                fs::write(root.join("unrecognized"), b"retain").unwrap();
            }
            path = Some(root);
        }
        drain(&mut f).await;
        let plan = node
            .claim_cleanup(&mut f.db, &f.auth, f.workspace)
            .await
            .unwrap()
            .unwrap();
        let mut heartbeat = f.metadata().await;
        let result = node
            .cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &plan, None)
            .await;
        if state == "unknown" {
            assert!(result.is_err());
            assert_eq!(
                fs::read(path.unwrap().join("unrecognized")).unwrap(),
                b"retain"
            );
            assert!(
                database::connection(&f.schema)
                    .await
                    .query_one("SELECT pin_holder FROM th_workspaces", &[])
                    .await
                    .unwrap()
                    .get::<_, Option<String>>(0)
                    .is_some()
            );
        } else {
            assert_eq!(result.unwrap()["state"], "deleted");
            if let Some(path) = path {
                assert!(!path.exists());
            }
        }
        drop(node);
        f.cleanup().await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; durable files with rolled-back final acknowledgment"]
async fn cleanup_reopens_completion_after_failed_ack_without_redeleting_a_replacement() {
    let mut f = Fixture::new().await;
    let node = f.node.take().unwrap();
    let preparation = prepare(&mut f, &node).await;
    let root = f
        .directory
        .path()
        .join("node/slots")
        .join(preparation.slot().slot.to_string());
    drain(&mut f).await;
    let plan = node
        .claim_cleanup(&mut f.db, &f.auth, f.workspace)
        .await
        .unwrap()
        .unwrap();
    let raw = database::connection(&f.schema).await;
    raw.batch_execute("CREATE FUNCTION cleanup_fail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test lost metadata completion'; END $$; CREATE TRIGGER cleanup_fail BEFORE UPDATE ON th_workspace_cleanups FOR EACH ROW WHEN(NEW.state='deleted') EXECUTE FUNCTION cleanup_fail()").await.unwrap();
    let mut heartbeat = f.metadata().await;
    assert!(
        node.cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &plan, None)
            .await
            .is_err()
    );
    assert!(fs::read_dir(&root).unwrap().next().is_none());
    let marker = f
        .directory
        .path()
        .join("node/cleanup")
        .join(plan.id().to_string())
        .join("complete.json");
    let receipt = fs::read(&marker).unwrap();
    assert_eq!(
        raw.query_one("SELECT state FROM th_workspaces", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "draining"
    );
    assert_eq!(
        raw.query_one("SELECT state FROM th_workspace_preparations", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "draining"
    );
    drop(node);
    let node = f.reopen();
    raw.batch_execute("DROP TRIGGER cleanup_fail ON th_workspace_cleanups")
        .await
        .unwrap();
    // A completion marker cannot authorize deleting new, unexpected bytes.
    fs::write(root.join("unexpected"), b"keep").unwrap();
    assert!(
        node.cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &plan, None)
            .await
            .is_err()
    );
    assert_eq!(fs::read(root.join("unexpected")).unwrap(), b"keep");
    fs::remove_file(root.join("unexpected")).unwrap();
    assert_eq!(
        node.cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &plan, None)
            .await
            .unwrap()["state"],
        "deleted"
    );
    assert_eq!(fs::read(marker).unwrap(), receipt);
    drop(node);
    f.cleanup().await;
}

async fn cleanup_child() {
    let input = std::env::var_os("TH_NODE_CLEANUP_INPUT").unwrap();
    let config: serde_json::Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let text = |key: &str| config[key].as_str().unwrap();
    let scope = Scope::new(text("tenant"), text("repository").parse().unwrap()).unwrap();
    let mut db = super::child_database(text("schema")).await;
    let mut heartbeat = super::child_database(text("schema")).await;
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
    let plan = node
        .claim_cleanup(&mut db, &auth, text("workspace").parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    node.cleanup_workspace(&mut db, &mut heartbeat, &auth, &plan, None)
        .await
        .unwrap();
    fs::write(text("finished"), b"acknowledged").unwrap();
    std::future::pending::<()>().await;
}
fn start_cleanup(f: &Fixture) -> super::Process {
    let input = f.directory.path().join("cleanup-child.json");
    fs::write(&input,serde_json::to_vec(&json!({"schema":f.schema,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"token":f.token.secret.expose(),
        "root":f.directory.path().join("node"),"storage":f.storage.to_string(),"workspace":f.workspace.to_string(),"finished":f.directory.path().join("cleanup-finished")})).unwrap()).unwrap();
    fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
    let log = fs::File::create(f.directory.path().join("cleanup-child.log")).unwrap();
    super::Process(Command::new(std::env::current_exe().unwrap()).args(["--exact","cleanup_tests::physical_cleanup_worker_loss_resumes_after_partial_unlink_and_durable_receipt","--ignored","--nocapture"])
        .env("TH_NODE_CLEANUP_INPUT",input).stdout(Stdio::from(log.try_clone().unwrap())).stderr(Stdio::from(log)).spawn().unwrap())
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; SIGKILL of actual filesystem worker"]
async fn physical_cleanup_worker_loss_resumes_after_partial_unlink_and_durable_receipt() {
    if std::env::var_os("TH_NODE_CLEANUP_INPUT").is_some() {
        cleanup_child().await;
        return;
    }
    for boundary in ["partial", "receipt", "ack"] {
        let mut f = Fixture::new().await;
        let node = f.node.take().unwrap();
        let preparation = prepare(&mut f, &node).await;
        let root = f
            .directory
            .path()
            .join("node/slots")
            .join(preparation.slot().slot.to_string());
        let files = root.join("tree/files");
        if boundary == "partial" {
            // Observe removal of the first actual source file while many later
            // files remain. There is no test fault switch in production code.
            for i in 0..30000 {
                fs::write(files.join(format!("z-{i:05}")), b"").unwrap();
            }
        }
        drain(&mut f).await;
        drop(node);
        let raw = database::connection(&f.schema).await;
        let gate = (u64::from_be_bytes(Uuid::new_v4().as_bytes()[..8].try_into().unwrap())
            & i64::MAX as u64) as i64;
        if boundary == "receipt" {
            raw.query_one("SELECT pg_advisory_lock($1)", &[&gate])
                .await
                .unwrap();
            raw.batch_execute(&format!("CREATE FUNCTION cleanup_pause() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({gate}::bigint); RETURN NEW; END $$; CREATE TRIGGER cleanup_pause BEFORE UPDATE ON th_workspace_cleanups FOR EACH ROW WHEN(NEW.state='deleted') EXECUTE FUNCTION cleanup_pause()")).await.unwrap();
        }
        let mut child = start_cleanup(&f);
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut cleanup_id = None;
        loop {
            if cleanup_id.is_none() {
                cleanup_id = raw
                    .query_opt("SELECT cleanup FROM th_workspace_cleanups", &[])
                    .await
                    .unwrap()
                    .map(|r| r.get::<_, Uuid>(0));
            }
            let reached = match boundary {
                "partial" => !files.join("a").exists(),
                "receipt" => cleanup_id.is_some_and(|id| {
                    f.directory
                        .path()
                        .join("node/cleanup")
                        .join(id.to_string())
                        .join("complete.json")
                        .exists()
                }),
                _ => f.directory.path().join("cleanup-finished").exists(),
            };
            if reached {
                break;
            }
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "worker stopped before {boundary}"
            );
            assert!(Instant::now() < deadline, "boundary {boundary} not reached");
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        child.kill();
        if boundary == "partial" {
            assert!(
                files.join("z-29999").exists(),
                "must kill before all source files disappear"
            );
        }
        if boundary == "receipt" {
            raw.query_one("SELECT pg_advisory_unlock($1)", &[&gate])
                .await
                .unwrap();
        }
        let node = f.reopen();
        let mut heartbeat = f.metadata().await;
        if boundary != "ack" {
            assert_eq!(
                raw.query_one("SELECT state FROM th_workspaces", &[])
                    .await
                    .unwrap()
                    .get::<_, String>(0),
                "draining"
            );
            raw.execute("UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()-interval '1 second'",&[]).await.unwrap();
            let next = node
                .claim_cleanup(&mut f.db, &f.auth, f.workspace)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(Some(next.id()), cleanup_id);
            assert!(next.intent().is_some());
            assert_eq!(
                node.cleanup_workspace(&mut f.db, &mut heartbeat, &f.auth, &next, None)
                    .await
                    .unwrap()["state"],
                "deleted"
            );
        } else {
            assert!(
                node.claim_cleanup(&mut f.db, &f.auth, f.workspace)
                    .await
                    .unwrap()
                    .is_none()
            );
            assert_eq!(
                f.db.completed_workspace_cleanup(&f.auth, f.workspace, f.storage)
                    .await
                    .unwrap()
                    .unwrap()["state"],
                "deleted"
            );
        }
        assert!(fs::read_dir(&root).unwrap().next().is_none());
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_operations WHERE kind='workspace.cleanup.complete.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            1
        );
        drop(node);
        f.cleanup().await;
    }
}
