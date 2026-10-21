mod common;
use common::{cleanup, connection, setup};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::*;
use treeharbor_metadata::catalog::ObjectLease;
use treeharbor_metadata::intake::*;
use treeharbor_metadata::{Authority, Error as MetaError, Metadata, Scope};
use treeharbor_storage::{LocalStore, ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

fn context(scope: &Scope, key: &str) -> CommandContext {
    CommandContext::trusted(
        scope.clone(),
        key.into(),
        "agent-owner".into(),
        Authority::Standalone,
        1,
    )
}
fn private_temp() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}
fn put(store: &LocalStore, object: Object) -> ObjectId {
    store.put(&object).unwrap()
}
fn changes(store: &LocalStore, bytes: &[u8]) -> Changes {
    let replacement = put(store, Object::Blob(bytes.to_vec()));
    Changes::new(vec![ChangeOperation {
        path: RepoPath::new(b"src/file".to_vec()).unwrap(),
        expected: None,
        replacement: Some(EntryRef {
            mode: EntryMode::File,
            target: replacement,
        }),
    }])
    .unwrap()
}
// Small-fixture graph upload; production batching/materialization is a separate path.
async fn upload_graph(
    db: &mut Metadata,
    depot: &ManagedStore,
    source: &LocalStore,
    root: ObjectId,
) -> ObjectLease {
    let mut done = BTreeSet::new();
    let mut pending = vec![(root, false)];
    let mut leases = BTreeMap::new();
    while let Some((id, children_done)) = pending.pop() {
        if done.contains(&id) {
            continue;
        }
        let object = source.get(id).unwrap();
        if !children_done {
            pending.push((id, true));
            pending.extend(
                object
                    .references()
                    .into_iter()
                    .map(|(child, _)| (child, false)),
            );
            continue;
        }
        let prepared = PreparedObject::new(&object).unwrap();
        let lease = db
            .reserve_object(depot, &prepared, Uuid::new_v4(), 60)
            .await
            .unwrap();
        if lease.requires_upload() {
            let receipt = depot.put(lease.key().generation(), &prepared).unwrap();
            db.publish_object(&lease, &receipt).await.unwrap();
        }
        done.insert(id);
        leases.insert(id, lease);
    }
    leases.remove(&root).unwrap()
}
struct Fixture {
    schema: String,
    scope: Scope,
    db: Metadata,
    stage_root: tempfile::TempDir,
    depot_root: tempfile::TempDir,
    stage: LocalStore,
    depot: ManagedStore,
    base: ObjectId,
    revision: RevisionRef,
    workspace: Uuid,
    run: Uuid,
}
impl Fixture {
    async fn new() -> Self {
        let (schema, scope, mut db) = setup().await;
        let stage_root = private_temp();
        let depot_root = private_temp();
        let tenant = TenantId::new(scope.tenant()).unwrap();
        let stage = LocalStore::open(stage_root.path(), &tenant).unwrap();
        let depot = ManagedStore::open(depot_root.path(), tenant, "local-1").unwrap();
        let root = put(&stage, Object::Directory(Directory::new(vec![]).unwrap()));
        let base = put(&stage, Object::Snapshot { root });
        let empty = put(
            &stage,
            Object::ChangeOperations(Changes::new(vec![]).unwrap()),
        );
        let commit = Commit {
            parents: vec![],
            snapshot: base,
            changes: empty,
            author_name: "Fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: "base".into(),
        };
        let id = put(&stage, Object::NativeCommit(commit.clone()));
        let lease = upload_graph(&mut db, &depot, &stage, id).await;
        db.register_native_base(&context(&scope, "base"), &commit, &lease)
            .await
            .unwrap();
        let mut f = Self {
            schema,
            scope,
            db,
            stage_root,
            depot_root,
            stage,
            depot,
            base,
            revision: RevisionRef::Native(id),
            workspace: Uuid::nil(),
            run: Uuid::nil(),
        };
        let (workspace, run) = f.execution().await;
        f.workspace = workspace;
        f.run = run;
        f
    }
    async fn execution(&mut self) -> (Uuid, Uuid) {
        let workspace = Uuid::new_v4();
        let run = Uuid::new_v4();
        let runtime = format!("fixture-{workspace}");
        self.db
            .create_workspace(
                &context(&self.scope, &format!("create-{workspace}")),
                &WorkspaceSpec {
                    id: workspace,
                    base: self.revision,
                    profile: [0; 32],
                    format: WorkspaceFormat::Native,
                },
            )
            .await
            .unwrap();
        let job = self
            .db
            .claim(self.scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
        self.db
            .record_workspace_ready(
                &context(&self.scope, &format!("ready-{workspace}")),
                &WorkspaceReady {
                    id: workspace,
                    generation: 1,
                    node: "fixture-node".into(),
                    runtime: runtime.clone(),
                },
                &job,
            )
            .await
            .unwrap();
        self.db
            .record_running_agent(
                &context(&self.scope, &format!("run-{run}")),
                &RunningAgent {
                    id: run,
                    workspace,
                    workspace_generation: 1,
                    agent: "test-agent/1".into(),
                    task: "test-task".into(),
                    runtime,
                },
            )
            .await
            .unwrap();
        (workspace, run)
    }
    async fn prepare(&mut self, bytes: &[u8]) -> (AppliedChange, ObjectLease, ObjectLease) {
        let applied = prepare_change(&self.stage, self.base, &changes(&self.stage, bytes)).unwrap();
        let ops = upload_graph(&mut self.db, &self.depot, &self.stage, applied.operations()).await;
        let result = upload_graph(
            &mut self.db,
            &self.depot,
            &self.stage,
            applied.result_snapshot(),
        )
        .await;
        (applied, ops, result)
    }
}
fn submission<'a>(
    id: Uuid,
    run: Uuid,
    prepared: &'a (AppliedChange, ObjectLease, ObjectLease),
) -> ChangeSubmission<'a> {
    ChangeSubmission {
        id,
        previous_version: 0,
        run,
        workspace_generation: 1,
        applied: &prepared.0,
        operations: &prepared.1,
        result: &prepared.2,
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn two_agents_preserve_overlapping_proposals_without_granting_merge_permission() {
    let mut f = Fixture::new().await;
    let (_, second_run) = f.execution().await;
    let a = f.prepare(b"agent A").await;
    let b = f.prepare(b"agent B").await;
    let a_id = Uuid::new_v4();
    let b_id = Uuid::new_v4();
    let a_result =
        f.db.accept_change(&context(&f.scope, "accept-a"), &submission(a_id, f.run, &a))
            .await
            .unwrap();
    let b_result =
        f.db.accept_change(
            &context(&f.scope, "accept-b"),
            &submission(b_id, second_run, &b),
        )
        .await
        .unwrap();
    assert_eq!(a_result["state"], "accepted");
    assert_eq!(b_result["state"], "accepted");
    assert_ne!(a_result["result_snapshot"], b_result["result_snapshot"]);
    drop(f.db);
    let mut db = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        db.change_version(&f.scope, a_id, 1).await.unwrap(),
        a_result
    );
    assert_eq!(
        db.accept_change(&context(&f.scope, "accept-a"), &submission(a_id, f.run, &a))
            .await
            .unwrap(),
        a_result
    );
    assert!(matches!(
        db.accept_change(&context(&f.scope, "accept-a"), &submission(a_id, f.run, &b))
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 hour'",
        &[],
    )
    .await
    .unwrap();
    assert!(db.try_retire_object(a.2.key(), 0).await.unwrap().is_none());
    let (reader, object) = (
        db.lease_pinned_root(
            &f.scope,
            &format!("change:{a_id}:1:result"),
            Uuid::new_v4(),
            60,
        )
        .await
        .unwrap(),
        a.0.result_snapshot(),
    );
    assert!(matches!(
        f.depot.get(reader.key().generation(), object).unwrap(),
        Object::Snapshot { .. }
    ));
    let foreign = Scope::new("foreign", f.scope.repository()).unwrap();
    assert!(matches!(
        db.change_version(&foreign, a_id, 1).await,
        Err(MetaError::NotFound)
    ));
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='change.accepted'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        2
    );
    // Retention cannot accidentally release a root still referenced by a version.
    assert!(
        raw.execute(
            "UPDATE th_change_versions SET run=$1 WHERE changeset=$2",
            &[&second_run, &a_id]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute(
            "UPDATE th_bases SET snapshot=$1",
            &[&a.0.result_snapshot().as_bytes().as_slice()]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute(
            "DELETE FROM th_object_pins WHERE holder=$1",
            &[&format!("change:{a_id}:1:result")]
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn current_run_base_principal_epoch_and_upload_holds_are_required() {
    let mut f = Fixture::new().await;
    let prepared = f.prepare(b"proposal").await;
    let id = Uuid::new_v4();
    let mut wrong_owner = context(&f.scope, "wrong-owner");
    wrong_owner.principal = "other".into();
    assert!(matches!(
        f.db.accept_change(&wrong_owner, &submission(id, f.run, &prepared))
            .await,
        Err(MetaError::ExecutionState)
    ));
    let mut wrong_generation = submission(id, f.run, &prepared);
    wrong_generation.workspace_generation = 2;
    assert!(matches!(
        f.db.accept_change(&context(&f.scope, "wrong-generation"), &wrong_generation)
            .await,
        Err(MetaError::ExecutionState)
    ));
    let wrong_base = prepare_change(
        &f.stage,
        prepared.0.result_snapshot(),
        &Changes::new(vec![]).unwrap(),
    )
    .unwrap();
    let ops = upload_graph(&mut f.db, &f.depot, &f.stage, wrong_base.operations()).await;
    let result = upload_graph(&mut f.db, &f.depot, &f.stage, wrong_base.result_snapshot()).await;
    assert!(matches!(
        f.db.accept_change(
            &context(&f.scope, "wrong-base"),
            &submission(id, f.run, &(wrong_base, ops, result))
        )
        .await,
        Err(MetaError::ReceiptMismatch)
    ));
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 hour' WHERE token=$1",
        &[&prepared.2.token()],
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.accept_change(
            &context(&f.scope, "expired-upload"),
            &submission(id, f.run, &prepared)
        )
        .await,
        Err(MetaError::StaleLease)
    ));
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_changesets", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'change:%'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    raw.execute("UPDATE th_repositories SET epoch=2", &[])
        .await
        .unwrap();
    assert!(matches!(
        f.db.accept_change(
            &context(&f.scope, "old-epoch"),
            &submission(id, f.run, &prepared)
        )
        .await,
        Err(MetaError::Fenced)
    ));
    let mut fresh = context(&f.scope, "new-epoch-old-run");
    fresh.epoch = 2;
    assert!(matches!(
        f.db.accept_change(&fresh, &submission(id, f.run, &prepared))
            .await,
        Err(MetaError::ExecutionState)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn concurrent_versions_use_compare_and_swap_and_never_replace_old_version() {
    let mut f = Fixture::new().await;
    let a = f.prepare(b"first").await;
    let b = f.prepare(b"second").await;
    let c = f.prepare(b"third").await;
    let id = Uuid::new_v4();
    let original =
        f.db.accept_change(&context(&f.scope, "v1"), &submission(id, f.run, &a))
            .await
            .unwrap();
    let mut left = submission(id, f.run, &b);
    left.previous_version = 1;
    let mut right = submission(id, f.run, &c);
    right.previous_version = 1;
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let lc = context(&f.scope, "v2-left");
    let rc = context(&f.scope, "v2-right");
    let (l, r) = tokio::join!(
        f.db.accept_change(&lc, &left),
        other.accept_change(&rc, &right)
    );
    assert_ne!(l.is_ok(), r.is_ok());
    assert!(
        matches!(l, Err(MetaError::VersionConflict))
            || matches!(r, Err(MetaError::VersionConflict))
    );
    assert_eq!(
        f.db.change_version(&f.scope, id, 1).await.unwrap(),
        original
    );
    assert_eq!(
        f.db.change_version(&f.scope, id, 2).await.unwrap()["version"],
        2
    );
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn drain_and_accept_serialize_without_deleting_active_data() {
    let mut f = Fixture::new().await;
    let prepared = f.prepare(b"drain race").await;
    let id = Uuid::new_v4();
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let accept = context(&f.scope, "accept");
    let drain = context(&f.scope, "drain");
    let change = submission(id, f.run, &prepared);
    let (accepted, drained) = tokio::join!(
        f.db.accept_change(&accept, &change),
        other.drain_workspace(&drain, f.workspace, 1)
    );
    assert_eq!(drained.unwrap()["state"], "draining");
    if let Ok(receipt) = accepted {
        assert_eq!(f.db.accept_change(&accept, &change).await.unwrap(), receipt);
    } else {
        assert!(matches!(accepted, Err(MetaError::ExecutionState)));
    }
    assert!(matches!(
        f.db.accept_change(&context(&f.scope, "new-after-drain"), &change)
            .await,
        Err(MetaError::ExecutionState)
    ));
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT state FROM th_agent_runs WHERE run=$1", &[&f.run])
            .await
            .unwrap()
            .get::<_, String>(0),
        "stopping"
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder=$1",
            &[&format!("workspace:{}", f.workspace)]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn stale_preparation_worker_cannot_mark_workspace_ready() {
    let mut f = Fixture::new().await;
    let workspace = Uuid::new_v4();
    f.db.create_workspace(
        &context(&f.scope, "pending"),
        &WorkspaceSpec {
            id: workspace,
            base: f.revision,
            profile: [4; 32],
            format: WorkspaceFormat::Native,
        },
    )
    .await
    .unwrap();
    let old =
        f.db.claim(f.scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1",
        &[&old.id],
    )
    .await
    .unwrap();
    let new =
        f.db.claim(f.scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let mut tampered = new.clone();
    tampered.payload["generation"] = json!(2);
    let fake = WorkspaceReady {
        id: workspace,
        generation: 2,
        node: "node".into(),
        runtime: "runtime".into(),
    };
    assert!(matches!(
        f.db.record_workspace_ready(&context(&f.scope, "tampered-job"), &fake, &tampered)
            .await,
        Err(MetaError::StaleLease)
    ));
    let ready = WorkspaceReady {
        id: workspace,
        generation: 1,
        node: "node".into(),
        runtime: "runtime".into(),
    };
    assert!(matches!(
        f.db.record_workspace_ready(&context(&f.scope, "stale-ready"), &ready, &old)
            .await,
        Err(MetaError::StaleLease)
    ));
    f.db.drain_workspace(&context(&f.scope, "drain-pending"), workspace, 1)
        .await
        .unwrap();
    assert!(matches!(
        f.db.record_workspace_ready(&context(&f.scope, "drained-ready"), &ready, &new)
            .await,
        Err(MetaError::ExecutionState)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn accepted_change_survives_worker_process_kill_and_same_request_replay() {
    // Re-enter this exact test in a real child process, then kill it after the
    // committed response. No destructor or graceful shutdown is involved.
    if let Ok(data) = std::env::var("TH_INTAKE_CRASH_FIXTURE") {
        let data: Value = serde_json::from_str(&data).unwrap();
        let schema = data["schema"].as_str().unwrap();
        assert!(schema.starts_with("th_test_"));
        let scope = Scope::new(
            "test-tenant",
            data["repository"].as_str().unwrap().parse().unwrap(),
        )
        .unwrap();
        let mut db = Metadata::new(connection(schema).await).await.unwrap();
        let tenant = TenantId::new(scope.tenant()).unwrap();
        let stage = LocalStore::open(data["stage"].as_str().unwrap(), &tenant).unwrap();
        let depot = ManagedStore::open(data["depot"].as_str().unwrap(), tenant, "local-1").unwrap();
        let base = data["base"].as_str().unwrap().parse().unwrap();
        let applied =
            prepare_change(&stage, base, &changes(&stage, b"durable after SIGKILL")).unwrap();
        let ops = upload_graph(&mut db, &depot, &stage, applied.operations()).await;
        let result = upload_graph(&mut db, &depot, &stage, applied.result_snapshot()).await;
        let id = data["id"].as_str().unwrap().parse().unwrap();
        let run = data["run"].as_str().unwrap().parse().unwrap();
        let receipt = db
            .accept_change(
                &context(&scope, "process-loss"),
                &submission(id, run, &(applied, ops, result)),
            )
            .await
            .unwrap();
        println!("TH_COMMITTED {receipt}");
        use std::io::Write;
        std::io::stdout().flush().unwrap();
        std::future::pending::<()>().await;
        unreachable!()
    }
    let mut f = Fixture::new().await;
    let prepared = f.prepare(b"durable after SIGKILL").await;
    let id = Uuid::new_v4();
    let data = json!({"schema":f.schema,"repository":f.scope.repository().to_string(),"stage":f.stage_root.path(),"depot":f.depot_root.path(),"base":f.base.to_string(),"id":id.to_string(),"run":f.run.to_string()});
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "accepted_change_survives_worker_process_kill_and_same_request_replay",
            "--nocapture",
        ])
        .env("TH_INTAKE_CRASH_FIXTURE", data.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
        .unwrap();
    let output = child.stdout.take().unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(output).lines() {
            if let Ok(line) = line
                && let Some(receipt) = line.strip_prefix("TH_COMMITTED ")
            {
                let _ = sender.send(receipt.to_owned());
                break;
            }
        }
    });
    let receipt = receiver.recv_timeout(std::time::Duration::from_secs(30));
    let _ = child.kill();
    let status = child.wait().unwrap();
    assert!(!status.success());
    let receipt: Value =
        serde_json::from_str(&receipt.expect("child must commit before being killed")).unwrap();
    assert_eq!(f.db.change_version(&f.scope, id, 1).await.unwrap(), receipt);
    assert_eq!(
        f.db.accept_change(
            &context(&f.scope, "process-loss"),
            &submission(id, f.run, &prepared)
        )
        .await
        .unwrap(),
        receipt
    );
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='change.accepted'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn simultaneous_submission_retries_commit_one_version_and_one_event() {
    let mut f = Fixture::new().await;
    let prepared = f.prepare(b"same payload").await;
    let id = Uuid::new_v4();
    let scope = f.scope.clone();
    let schema = f.schema.clone();
    let run = f.run;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let scope = scope.clone();
        let schema = schema.clone();
        let prepared = prepared.clone();
        tasks.push(tokio::spawn(async move {
            let mut db = Metadata::new(connection(&schema).await).await.unwrap();
            db.accept_change(
                &context(&scope, "shared-request"),
                &submission(id, run, &prepared),
            )
            .await
            .unwrap()
        }));
    }
    let expected = f.db.change_version(&scope, id, 1).await.ok();
    let mut receipt = expected;
    for task in tasks {
        let value = task.await.unwrap();
        if let Some(previous) = &receipt {
            assert_eq!(&value, previous)
        } else {
            receipt = Some(value)
        }
    }
    let raw = connection(&schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='change.accepted'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn published_v2_catalog_upgrade_preserves_live_pinned_bytes() {
    use sha2::{Digest, Sha256};
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let first = include_str!("../migrations/001_transactions.sql");
    let second = include_str!("../migrations/002_object_catalog.sql");
    raw.batch_execute(first).await.unwrap();
    raw.batch_execute(second).await.unwrap();
    let mut digest = Sha256::new();
    digest.update(Sha256::digest(first.as_bytes()));
    digest.update(second.as_bytes());
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(2,$1)",
        &[&digest.finalize().to_vec()],
    )
    .await
    .unwrap();
    let scope = Scope::new("test-tenant", Uuid::new_v4()).unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let stage_root = private_temp();
    let depot_root = private_temp();
    let tenant = TenantId::new(scope.tenant()).unwrap();
    let stage = LocalStore::open(stage_root.path(), &tenant).unwrap();
    let depot = ManagedStore::open(depot_root.path(), tenant, "local-1").unwrap();
    let id = put(&stage, Object::Blob(b"before schema upgrade".to_vec()));
    let lease = upload_graph(&mut db, &depot, &stage, id).await;
    let request = treeharbor_metadata::Request::new(
        scope.clone(),
        "old-pin",
        "owner",
        "pin",
        Authority::Standalone,
        1,
        b"old-pin",
    )
    .unwrap();
    let treeharbor_metadata::Begin::Fresh(tx) = db.begin(request).await.unwrap() else {
        panic!()
    };
    tx.pin_object("upgrade-root", &lease).await.unwrap();
    tx.complete(json!({"ok":true}), vec![]).await.unwrap();
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    let read = db
        .lease_pinned_root(&scope, "upgrade-root", Uuid::new_v4(), 60)
        .await
        .unwrap();
    assert_eq!(
        depot.get(read.key().generation(), id).unwrap(),
        Object::Blob(b"before schema upgrade".to_vec())
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn authenticated_change_acceptance_requires_owner_permission_and_live_credential_on_replay() {
    use treeharbor_metadata::access::{Permission as P, PrincipalKind as K};
    let mut f = Fixture::new().await;
    let prepared = f.prepare(b"authenticated source change").await;
    let change = submission(Uuid::new_v4(), f.run, &prepared);
    for principal in ["agent-owner", "other-agent"] {
        f.db.provision_principal(f.scope.tenant(), principal, K::Agent, "operator")
            .await
            .unwrap();
        f.db.set_repository_access(
            &f.scope,
            principal,
            &[P::ChangeWrite, P::WorkspaceWrite],
            "operator",
        )
        .await
        .unwrap();
    }
    let token =
        f.db.issue_repository_token(
            &f.scope,
            "agent-owner",
            &[P::ChangeWrite, P::WorkspaceWrite],
            3600,
            "operator",
        )
        .await
        .unwrap();
    let other =
        f.db.issue_repository_token(&f.scope, "other-agent", &[P::ChangeWrite], 3600, "operator")
            .await
            .unwrap();
    let wrong_permission =
        f.db.authorize(&token.secret, &f.scope, P::WorkspaceWrite)
            .await
            .unwrap();
    assert!(matches!(
        f.db.accept_change(
            &wrong_permission.context("accept".into(), Authority::Standalone, 1),
            &change
        )
        .await,
        Err(MetaError::Forbidden)
    ));
    let wrong_owner =
        f.db.authorize(&other.secret, &f.scope, P::ChangeWrite)
            .await
            .unwrap();
    assert!(matches!(
        f.db.accept_change(
            &wrong_owner.context("other".into(), Authority::Standalone, 1),
            &change
        )
        .await,
        Err(MetaError::ExecutionState)
    ));
    let owner =
        f.db.authorize(&token.secret, &f.scope, P::ChangeWrite)
            .await
            .unwrap();
    let ctx = owner.context("accept".into(), Authority::Standalone, 1);
    let receipt = f.db.accept_change(&ctx, &change).await.unwrap();
    assert_eq!(receipt["state"], "accepted");
    assert_eq!(f.db.accept_change(&ctx, &change).await.unwrap(), receipt);
    f.db.revoke_repository_token(&f.scope, token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.accept_change(&ctx, &change).await,
        Err(MetaError::Unauthenticated)
    ));
    assert_eq!(
        connection(&f.schema)
            .await
            .query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&f.schema).await;
}
