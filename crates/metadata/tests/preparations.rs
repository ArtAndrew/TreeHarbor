mod common;
use common::{cleanup, connection, setup};
use serde_json::json;
use sha2::{Digest, Sha256};
use treeharbor_core::{Changes, Commit, Directory, Object, ObjectId, RevisionRef, SourceProfile};
use treeharbor_metadata::{
    Authority, Begin, Error, Event, Job, Metadata, Scope,
    access::{Authorization, IssuedToken, Permission as P, PrincipalKind as K},
    intake::{CommandContext, RunningAgent, WorkspaceFormat, WorkspaceReady, WorkspaceSpec},
    preparations::{Preparation, PreparedFiles, StorageSlot},
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

struct Node {
    auth: Authorization,
    token: IssuedToken,
    storage: Uuid,
}
struct Fixture {
    schema: String,
    scope: Scope,
    db: Metadata,
    _objects: tempfile::TempDir,
    base: RevisionRef,
    snapshot: ObjectId,
}
impl Fixture {
    async fn new() -> Self {
        let (schema, scope, db) = setup().await;
        Self::from_db(schema, scope, db).await
    }
    async fn from_db(schema: String, scope: Scope, mut db: Metadata) -> Self {
        let objects = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(objects.path(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let depot = ManagedStore::open(
            objects.path(),
            TenantId::new(scope.tenant()).unwrap(),
            "preparation-tests",
        )
        .unwrap();
        let root = Object::Directory(Directory::new(vec![]).unwrap());
        let snapshot = Object::Snapshot {
            root: root.id().unwrap(),
        };
        let snapshot_id = snapshot.id().unwrap();
        let changes = Object::ChangeOperations(Changes::new(vec![]).unwrap());
        let commit = Commit {
            parents: vec![],
            snapshot: snapshot_id,
            changes: changes.id().unwrap(),
            author_name: "Fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: "base".into(),
        };
        let native = Object::NativeCommit(commit.clone());
        let base = RevisionRef::Native(native.id().unwrap());
        let mut last = None;
        for object in [root, snapshot, changes, native] {
            let prepared = PreparedObject::new(&object).unwrap();
            let lease = db
                .reserve_object(&depot, &prepared, Uuid::new_v4(), 60)
                .await
                .unwrap();
            if lease.requires_upload() {
                let receipt = depot.put(lease.key().generation(), &prepared).unwrap();
                db.publish_object(&lease, &receipt).await.unwrap();
            }
            last = Some(lease);
        }
        let ctx = |key: &str| {
            CommandContext::trusted(
                scope.clone(),
                key.into(),
                "owner".into(),
                Authority::Standalone,
                1,
            )
        };
        db.register_native_base(&ctx("base"), &commit, &last.unwrap())
            .await
            .unwrap();
        db.put_source_profile(&ctx("profile"), &SourceProfile::full())
            .await
            .unwrap();
        Self {
            schema,
            scope,
            db,
            _objects: objects,
            base,
            snapshot: snapshot_id,
        }
    }
    fn context(&self, key: &str) -> CommandContext {
        CommandContext::trusted(
            self.scope.clone(),
            key.into(),
            "owner".into(),
            Authority::Standalone,
            1,
        )
    }
    async fn token(&mut self, name: &str, kind: K, permission: P) -> (IssuedToken, Authorization) {
        self.db
            .provision_principal(self.scope.tenant(), name, kind, "operator")
            .await
            .unwrap();
        self.db
            .set_repository_access(&self.scope, name, &[permission], "operator")
            .await
            .unwrap();
        let token = self
            .db
            .issue_repository_token(&self.scope, name, &[permission], 3600, "operator")
            .await
            .unwrap();
        let auth = self
            .db
            .authorize(&token.secret, &self.scope, permission)
            .await
            .unwrap();
        (token, auth)
    }
    async fn node(&mut self, name: &str) -> Node {
        let (token, auth) = self.token(name, K::Node, P::RuntimeControl).await;
        let storage = Uuid::new_v4();
        self.db
            .register_node_storage(self.scope.tenant(), name, storage, "operator")
            .await
            .unwrap();
        Node {
            auth,
            token,
            storage,
        }
    }
    async fn workspace(&mut self) -> Uuid {
        let id = Uuid::new_v4();
        self.db
            .create_workspace(
                &self.context(&format!("create-{id}")),
                &WorkspaceSpec {
                    id,
                    base: self.base,
                    profile: *SourceProfile::full().id().as_bytes(),
                    format: WorkspaceFormat::Native,
                },
            )
            .await
            .unwrap();
        id
    }
    async fn claim(&mut self, node: &Node) -> Job {
        self.db
            .claim_preparation(&node.auth, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap()
    }
    async fn reserve(&mut self, node: &Node, job: &Job) -> Preparation {
        self.db
            .reserve_preparation(
                &node.auth,
                job,
                StorageSlot {
                    storage: node.storage,
                    slot: Uuid::new_v4(),
                },
            )
            .await
            .unwrap()
    }
    fn observed(&self) -> PreparedFiles {
        // Metadata observation only, not a claim that this fixture ran a sandbox.
        PreparedFiles {
            snapshot: self.snapshot,
            profile: SourceProfile::full().id(),
            manifest_digest: [91; 32],
        }
    }
    async fn expire(&self, job: &Job) {
        connection(&self.schema).await.execute("UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1",&[&job.id]).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn node_storage_identity_is_immutable_and_preparation_claim_is_scoped() {
    let mut f = Fixture::new().await;
    let a = f.node("node-a").await;
    let b = f.node("node-b").await;
    f.db.register_node_storage(f.scope.tenant(), "node-a", a.storage, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.register_node_storage(f.scope.tenant(), "node-a", Uuid::new_v4(), "operator")
            .await,
        Err(Error::Fenced)
    ));
    assert!(matches!(
        f.db.register_node_storage(f.scope.tenant(), "node-b", a.storage, "operator")
            .await,
        Err(Error::Fenced)
    ));
    let (_, agent) = f.token("agent", K::Agent, P::Read).await;
    assert!(matches!(
        f.db.register_node_storage(f.scope.tenant(), "agent", Uuid::new_v4(), "operator")
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.db.claim_preparation(&agent, a.storage, Uuid::new_v4(), 60)
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.db.claim_preparation(&a.auth, b.storage, Uuid::new_v4(), 60)
            .await,
        Err(Error::Fenced)
    ));
    let request = treeharbor_metadata::Request::new(
        f.scope.clone(),
        "unrelated",
        "owner",
        "test-event",
        Authority::Standalone,
        1,
        b"unrelated",
    )
    .unwrap();
    let Begin::Fresh(tx) = f.db.begin(request).await.unwrap() else {
        panic!()
    };
    tx.complete(
        json!({"ok":true}),
        vec![Event {
            kind: "change.accepted".into(),
            payload: json!({}),
        }],
    )
    .await
    .unwrap();
    let workspace = f.workspace().await;
    let job = f.claim(&a).await;
    assert_eq!(job.kind, "workspace.prepare");
    assert_eq!(job.payload["workspace"], workspace.to_string());
    // Older unrelated event remains pending and claimable by its own dispatcher.
    let other =
        f.db.claim(f.scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    assert_eq!(other.kind, "change.accepted");
    let plan = f.reserve(&a, &job).await;
    assert!(matches!(
        f.db.reserve_preparation(
            &b.auth,
            &job,
            StorageSlot {
                storage: b.storage,
                slot: Uuid::new_v4()
            }
        )
        .await,
        Err(Error::ExecutionState)
    ));
    let mut altered = job.clone();
    altered.payload["generation"] = json!(99);
    assert!(matches!(
        f.db.reserve_preparation(&a.auth, &altered, plan.slot())
            .await,
        Err(Error::StaleLease)
    ));
    assert_eq!(plan.revision(), &f.base);
    assert_eq!(plan.snapshot(), f.snapshot);
    assert_eq!(plan.profile(), &SourceProfile::full());
    assert_eq!(plan.source_holder(), format!("workspace:{workspace}"));
    f.expire(&job).await;
    assert!(
        f.db.claim_preparation(&b.auth, b.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        f.db.preparation_status(&b.auth, workspace).await,
        Err(Error::NotFound)
    ));
    let replay = f.claim(&a).await;
    assert_eq!(replay.id, job.id);
    assert_eq!(replay.generation, job.generation + 1);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn reservation_survives_reconnect_reclaim_and_rejects_stale_completion() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    let job = f.claim(&node).await;
    let old = f.reserve(&node, &job).await;
    f.db.renew_preparation(&node.auth, &old, 60).await.unwrap();
    let duplicate =
        f.db.reserve_preparation(&node.auth, &job, old.slot())
            .await
            .unwrap();
    assert_eq!(duplicate.attempt(), old.attempt());
    assert!(matches!(
        f.db.reserve_preparation(
            &node.auth,
            &job,
            StorageSlot {
                storage: node.storage,
                slot: Uuid::new_v4()
            }
        )
        .await,
        Err(Error::ExecutionState)
    ));
    // A reconnect sees the durable reservation, not an in-memory node map.
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let status =
        f.db.preparation_status(&node.auth, workspace)
            .await
            .unwrap();
    assert_eq!(status["attempt"], old.attempt().to_string());
    assert_eq!(status["slot"], old.slot().slot.to_string());
    f.expire(&job).await;
    assert!(matches!(
        f.db.renew_preparation(&node.auth, &old, 60).await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        f.db.reserve_preparation(&node.auth, &job, old.slot()).await,
        Err(Error::StaleLease)
    ));
    let new_job = f.claim(&node).await;
    let new =
        f.db.reserve_preparation(&node.auth, &new_job, old.slot())
            .await
            .unwrap();
    assert_eq!(new.attempt(), old.attempt());
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &old, &f.observed())
            .await,
        Err(Error::StaleLease)
    ));
    let wrong = PreparedFiles {
        snapshot: ObjectId::from_bytes([7; 32]),
        ..f.observed()
    };
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &new, &wrong).await,
        Err(Error::ReceiptMismatch)
    ));
    let first =
        f.db.complete_preparation(&node.auth, &new, &f.observed())
            .await
            .unwrap();
    assert_eq!(
        first,
        f.db.complete_preparation(&node.auth, &new, &f.observed())
            .await
            .unwrap()
    );
    let wrong = PreparedFiles {
        manifest_digest: [8; 32],
        ..f.observed()
    };
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &new, &wrong).await,
        Err(Error::ReceiptMismatch)
    ));
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &old, &f.observed())
            .await,
        Err(Error::StaleLease)
    ));
    let row = connection(&f.schema)
        .await
        .query_one(
            "SELECT state,runtime FROM th_workspaces WHERE workspace=$1",
            &[&workspace],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, String>(0), "ready");
    assert!(row.get::<_, Option<String>>(1).is_none());
    assert!(
        f.db.claim_preparation(&node.auth, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let run = RunningAgent {
        id: Uuid::new_v4(),
        workspace,
        workspace_generation: 1,
        agent: "test".into(),
        task: "test".into(),
        runtime: "invented-runtime".into(),
    };
    assert!(matches!(
        f.db.record_running_agent(
            &node
                .auth
                .context("legacy-run".into(), Authority::Standalone, 1),
            &run
        )
        .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.renew_preparation(&node.auth, &new, 60).await,
        Err(Error::ExecutionState)
    ));
    f.db.drain_workspace(&f.context("drain-after-prepared"), workspace, 1)
        .await
        .unwrap();
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &new, &f.observed())
            .await,
        Err(Error::ExecutionState)
    ));
    assert_eq!(
        f.db.preparation_status(&node.auth, workspace)
            .await
            .unwrap()["state"],
        "draining"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn drain_retains_slot_and_pins_and_blocks_all_readiness_paths() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    let job = f.claim(&node).await;
    let plan = f.reserve(&node, &job).await;
    let ready = WorkspaceReady {
        id: workspace,
        generation: 1,
        node: "node".into(),
        runtime: "fake-runtime".into(),
    };
    assert!(matches!(
        f.db.record_workspace_ready(
            &node
                .auth
                .context("legacy-ready".into(), Authority::Standalone, 1),
            &ready,
            &job
        )
        .await,
        Err(Error::ExecutionState)
    ));
    f.db.drain_workspace(&f.context("drain"), workspace, 1)
        .await
        .unwrap();
    assert!(matches!(
        f.db.renew_preparation(&node.auth, &plan, 60).await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &plan, &f.observed())
            .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.reserve_preparation(&node.auth, &job, plan.slot())
            .await,
        Err(Error::ExecutionState)
    ));
    let status =
        f.db.preparation_status(&node.auth, workspace)
            .await
            .unwrap();
    assert_eq!(status["state"], "draining");
    assert_eq!(status["attempt"], plan.attempt().to_string());
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder=$1",
            &[&plan.source_holder()]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    f.workspace().await;
    let second = f.claim(&node).await;
    assert!(matches!(
        f.db.reserve_preparation(&node.auth, &second, plan.slot())
            .await,
        Err(Error::ExecutionState)
    ));
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_preparations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn cached_node_authorization_and_old_epoch_do_not_authorize_recovery() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    let job = f.claim(&node).await;
    let plan = f.reserve(&node, &job).await;
    f.db.revoke_repository_token(&f.scope, node.token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.renew_preparation(&node.auth, &plan, 60).await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        f.db.reserve_preparation(&node.auth, &job, plan.slot())
            .await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        f.db.complete_preparation(&node.auth, &plan, &f.observed())
            .await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        f.db.preparation_status(&node.auth, workspace).await,
        Err(Error::Unauthenticated)
    ));
    let (_, fresh) = f.token("node", K::Node, P::RuntimeControl).await;
    connection(&f.schema)
        .await
        .execute(
            "UPDATE th_repositories SET epoch=epoch+1 WHERE tenant=$1 AND repository=$2",
            &[&f.scope.tenant(), &f.scope.repository()],
        )
        .await
        .unwrap();
    assert!(matches!(
        f.db.complete_preparation(&fresh, &plan, &f.observed())
            .await,
        Err(Error::Fenced)
    ));
    assert!(matches!(
        f.db.reserve_preparation(&fresh, &job, plan.slot()).await,
        Err(Error::Fenced)
    ));
    f.expire(&job).await;
    assert!(
        f.db.claim_preparation(&fresh, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    // Inspection is for recovery planning; it does not grant a live lease.
    assert_eq!(
        f.db.preparation_status(&fresh, workspace).await.unwrap()["state"],
        "reserved"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn concurrent_reservation_has_one_identity_and_database_rejects_rebinding() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    f.workspace().await;
    let job = f.claim(&node).await;
    let slot = StorageSlot {
        storage: node.storage,
        slot: Uuid::new_v4(),
    };
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        f.db.reserve_preparation(&node.auth, &job, slot),
        other.reserve_preparation(&node.auth, &job, slot)
    );
    let a = a.unwrap();
    assert_eq!(a.attempt(), b.unwrap().attempt());
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_preparations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(
        raw.execute(
            "UPDATE th_workspace_preparations SET slot=$1 WHERE attempt=$2",
            &[&Uuid::new_v4(), &a.attempt()]
        )
        .await
        .is_err()
    );
    // Partial completion observations must not pass a SQL CHECK via NULL logic.
    assert!(raw.execute("UPDATE th_workspace_preparations SET manifest_digest=$1,completed_worker=$2,prepared_at=clock_timestamp() WHERE attempt=$3",&[&[4_u8;32].as_slice(),&Uuid::new_v4(),&a.attempt()]).await.is_err());
    f.db.complete_preparation(&node.auth, &a, &f.observed())
        .await
        .unwrap();
    assert!(
        raw.execute(
            "UPDATE th_workspace_preparations SET manifest_digest=$1 WHERE attempt=$2",
            &[&[4_u8; 32].as_slice(), &a.attempt()]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute(
            "UPDATE th_workspace_preparations SET state='reserved' WHERE attempt=$1",
            &[&a.attempt()]
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn populated_v6_upgrade_keeps_sources_receipts_and_pending_work() {
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let mut digest = Vec::new();
    for sql in [
        include_str!("../migrations/001_transactions.sql"),
        include_str!("../migrations/002_object_catalog.sql"),
        include_str!("../migrations/003_change_intake.sql"),
        include_str!("../migrations/004_access_control.sql"),
        include_str!("../migrations/005_source_profiles.sql"),
        include_str!("../migrations/006_source_uploads.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut hash = Sha256::new();
        hash.update(digest);
        hash.update(sql.as_bytes());
        digest = hash.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(6,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let mut f = Fixture::from_db(schema, scope, db).await;
    let workspace = f.workspace().await;
    let before = raw
        .query("SELECT key,result FROM th_operations ORDER BY key", &[])
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, String>(0), r.get::<_, serde_json::Value>(1)))
        .collect::<Vec<_>>();
    let pins = raw
        .query_one("SELECT count(*) FROM th_object_pins", &[])
        .await
        .unwrap()
        .get::<_, i64>(0);
    assert!(matches!(
        f.db.check_schema().await,
        Err(Error::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    let after = raw
        .query("SELECT key,result FROM th_operations ORDER BY key", &[])
        .await
        .unwrap()
        .into_iter()
        .map(|r| (r.get::<_, String>(0), r.get::<_, serde_json::Value>(1)))
        .collect::<Vec<_>>();
    assert_eq!(before, after);
    assert_eq!(
        pins,
        raw.query_one("SELECT count(*) FROM th_object_pins", &[])
            .await
            .unwrap()
            .get::<_, i64>(0)
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_preparations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let node = f.node("node").await;
    let job = f.claim(&node).await;
    assert_eq!(job.payload["workspace"], workspace.to_string());
    let plan = f.reserve(&node, &job).await;
    f.db.complete_preparation(&node.auth, &plan, &f.observed())
        .await
        .unwrap();
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn preparation_identity_and_job_cannot_cross_tenant_scopes() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    let job = f.claim(&node).await;
    let plan = f.reserve(&node, &job).await;
    let other = Scope::new("other-tenant", Uuid::new_v4()).unwrap();
    f.db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    f.db.provision_principal(other.tenant(), "node", K::Node, "operator")
        .await
        .unwrap();
    f.db.set_repository_access(&other, "node", &[P::RuntimeControl], "operator")
        .await
        .unwrap();
    let token =
        f.db.issue_repository_token(&other, "node", &[P::RuntimeControl], 3600, "operator")
            .await
            .unwrap();
    let auth =
        f.db.authorize(&token.secret, &other, P::RuntimeControl)
            .await
            .unwrap();
    assert!(matches!(
        f.db.register_node_storage(other.tenant(), "node", node.storage, "operator")
            .await,
        Err(Error::Fenced)
    ));
    let storage = Uuid::new_v4();
    f.db.register_node_storage(other.tenant(), "node", storage, "operator")
        .await
        .unwrap();
    assert!(
        f.db.claim_preparation(&auth, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        f.db.reserve_preparation(
            &auth,
            &job,
            StorageSlot {
                storage,
                slot: plan.slot().slot
            }
        )
        .await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        f.db.preparation_status(&auth, workspace).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.db.complete_preparation(&auth, &plan, &f.observed()).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.db.renew_preparation(&auth, &plan, 60).await,
        Err(Error::Forbidden)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn lease_expiring_after_initial_check_rolls_back_the_whole_reservation() {
    use std::time::{Duration, Instant};
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    let job = f.claim(&node).await;
    let raw = connection(&f.schema).await;
    let gate = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    // Block exactly inside INSERT, after job/auth validation, using a separate
    // real database session. No dependency on a guessed client sleep boundary.
    raw.batch_execute(&format!("CREATE FUNCTION pause_preparation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({gate}); RETURN NEW; END; $$; CREATE TRIGGER pause_preparation BEFORE INSERT ON th_workspace_preparations FOR EACH ROW EXECUTE FUNCTION pause_preparation();")).await.unwrap();
    let blocker = connection(&f.schema).await;
    blocker.batch_execute("BEGIN").await.unwrap();
    blocker
        .query_one("SELECT pg_advisory_xact_lock($1)", &[&gate])
        .await
        .unwrap();
    raw.execute(
        "UPDATE th_outbox SET lease_until=clock_timestamp()+interval '2 seconds' WHERE id=$1",
        &[&job.id],
    )
    .await
    .unwrap();
    let writer = connection(&f.schema).await;
    let pid: i32 = writer
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut db = Metadata::new(writer).await.unwrap();
    let auth = node.auth.clone();
    let worker_job = job.clone();
    let slot = StorageSlot {
        storage: node.storage,
        slot: Uuid::new_v4(),
    };
    let task = tokio::spawn(async move { db.reserve_preparation(&auth, &worker_job, slot).await });
    let until = Instant::now() + Duration::from_secs(4);
    loop {
        let blocked:bool=raw.query_one("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock' AND wait_event='advisory')",&[&pid]).await.unwrap().get(0);
        if blocked {
            break;
        }
        assert!(
            Instant::now() < until,
            "worker never reached the insertion boundary"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    loop {
        let expired: bool = raw
            .query_one(
                "SELECT lease_until<=clock_timestamp() FROM th_outbox WHERE id=$1",
                &[&job.id],
            )
            .await
            .unwrap()
            .get(0);
        if expired {
            break;
        }
        assert!(
            Instant::now() < until,
            "database lease clock did not advance"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    blocker.batch_execute("COMMIT").await.unwrap();
    assert!(matches!(task.await.unwrap(), Err(Error::StaleLease)));
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_preparations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        raw.query_one(
            "SELECT node FROM th_workspaces WHERE workspace=$1",
            &[&workspace]
        )
        .await
        .unwrap()
        .get::<_, Option<String>>(0)
        .is_none()
    );
    let next = f.claim(&node).await;
    assert_eq!(next.generation, job.generation + 1);
    f.db.reserve_preparation(&node.auth, &next, slot)
        .await
        .unwrap();
    cleanup(&f.schema).await;
}
