mod common;
use common::{cleanup, connection, setup};
use std::time::Duration;
use treeharbor_core::{Changes, Commit, Directory, Object, RevisionRef};
use treeharbor_metadata::access::{
    Authorization, IssuedToken, Permission as P, PrincipalKind as K,
};
use treeharbor_metadata::intake::{
    CommandContext, RunningAgent, WorkspaceFormat, WorkspaceReady, WorkspaceSpec,
};
use treeharbor_metadata::{Authority, Error, Metadata, Scope};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

struct Fixture {
    schema: String,
    scope: Scope,
    db: Metadata,
    _objects: tempfile::TempDir,
    base: RevisionRef,
}
impl Fixture {
    async fn new() -> Self {
        let (schema, scope, mut db) = setup().await;
        db.put_source_profile(
            &CommandContext::trusted(
                scope.clone(),
                "full-profile".into(),
                "bootstrap".into(),
                Authority::Standalone,
                1,
            ),
            &treeharbor_core::SourceProfile::full(),
        )
        .await
        .unwrap();
        Self::from_database(schema, scope, db).await
    }
    async fn from_database(schema: String, scope: Scope, mut db: Metadata) -> Self {
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
            "test-local",
        )
        .unwrap();
        let root = Object::Directory(Directory::new(vec![]).unwrap());
        let snapshot = Object::Snapshot {
            root: root.id().unwrap(),
        };
        let operations = Object::ChangeOperations(Changes::new(vec![]).unwrap());
        let commit = Commit {
            parents: vec![],
            snapshot: snapshot.id().unwrap(),
            changes: operations.id().unwrap(),
            author_name: "Fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: "base".into(),
        };
        let native = Object::NativeCommit(commit.clone());
        let base = RevisionRef::Native(native.id().unwrap());
        let mut lease = None;
        for object in [root, snapshot, operations, native] {
            let prepared = PreparedObject::new(&object).unwrap();
            let current = db
                .reserve_object(&depot, &prepared, Uuid::new_v4(), 60)
                .await
                .unwrap();
            if current.requires_upload() {
                let receipt = depot.put(current.key().generation(), &prepared).unwrap();
                db.publish_object(&current, &receipt).await.unwrap();
            }
            lease = Some(current);
        }
        db.register_native_base(
            &CommandContext::trusted(
                scope.clone(),
                "base".into(),
                "bootstrap".into(),
                Authority::Standalone,
                1,
            ),
            &commit,
            &lease.unwrap(),
        )
        .await
        .unwrap();
        Self {
            schema,
            scope,
            db,
            _objects: objects,
            base,
        }
    }
    async fn token(&mut self, principal: &str, kind: K, permissions: &[P]) -> IssuedToken {
        self.db
            .provision_principal(self.scope.tenant(), principal, kind, "operator")
            .await
            .unwrap();
        self.db
            .set_repository_access(&self.scope, principal, permissions, "operator")
            .await
            .unwrap();
        self.db
            .issue_repository_token(&self.scope, principal, permissions, 3600, "operator")
            .await
            .unwrap()
    }
    async fn auth(&mut self, token: &IssuedToken, permission: P) -> Authorization {
        self.db
            .authorize(&token.secret, &self.scope, permission)
            .await
            .unwrap()
    }
    fn spec(&self) -> WorkspaceSpec {
        WorkspaceSpec {
            id: Uuid::new_v4(),
            base: self.base,
            profile: *treeharbor_core::SourceProfile::full().id().as_bytes(),
            format: WorkspaceFormat::Native,
        }
    }
}
fn ctx(auth: &Authorization, key: &str) -> CommandContext {
    auth.context(key.into(), Authority::Standalone, 1)
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn workspace_source_scope_owner_fences_and_final_revocation_are_enforced() {
    use treeharbor_metadata::workspace_reads::SourceFence;
    let mut f = Fixture::new().await;
    let owner = f
        .token("owner", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let other = f.token("other", K::Agent, &[P::Read]).await;
    let spec = f.spec();
    let write = f.auth(&owner, P::WorkspaceWrite).await;
    f.db.create_workspace(&ctx(&write, "source-workspace"), &spec)
        .await
        .unwrap();
    let read = f.auth(&owner, P::Read).await;
    let outsider = f.auth(&other, P::Read).await;
    let fence = SourceFence {
        workspace: spec.id,
        generation: 1,
        authority: Authority::Standalone,
        epoch: 1,
    };
    assert!(matches!(
        f.db.open_workspace_source(&write, fence, 60).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.db.open_workspace_source(&outsider, fence, 60).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.db.open_workspace_source(
            &read,
            SourceFence {
                generation: 2,
                ..fence
            },
            60
        )
        .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.open_workspace_source(&read, SourceFence { epoch: 2, ..fence }, 60)
            .await,
        Err(Error::Fenced)
    ));
    assert!(matches!(
        f.db.open_workspace_source(
            &read,
            SourceFence {
                authority: Authority::Connected,
                ..fence
            },
            60
        )
        .await,
        Err(Error::Fenced)
    ));
    let store = ManagedStore::open(
        f._objects.path(),
        TenantId::new(f.scope.tenant()).unwrap(),
        "test-local",
    )
    .unwrap();
    let sql = connection(&f.schema).await;
    let before: i64 = sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    let session = f.db.open_workspace_source(&read, fence, 60).await.unwrap();
    assert_eq!(session.identity().revision, f.base);
    let nodes = [session.root()];
    let values =
        f.db.open_workspace_source_batch(&session, &store, &nodes, 1024)
            .await
            .unwrap()
            .read()
            .unwrap();
    assert!(matches!(values[0].object(), Object::NativeCommit(_)));
    let held: i64 = sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(held, before + 1);
    f.db.revoke_repository_token(&f.scope, owner.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.open_workspace_source_batch(&session, &store, &nodes, 1024)
            .await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        f.db.finish_workspace_source(session).await,
        Err(Error::Unauthenticated)
    ));
    let after: i64 = sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(after, before);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn workspace_source_rejects_drain_epoch_change_and_unresolved_profile() {
    use treeharbor_metadata::workspace_reads::SourceFence;
    let mut f = Fixture::new().await;
    let owner = f
        .token("owner", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let spec = f.spec();
    let write = f.auth(&owner, P::WorkspaceWrite).await;
    f.db.create_workspace(&ctx(&write, "source-workspace"), &spec)
        .await
        .unwrap();
    let auth = f.auth(&owner, P::Read).await;
    let fence = SourceFence {
        workspace: spec.id,
        generation: 1,
        authority: Authority::Standalone,
        epoch: 1,
    };
    let sql = connection(&f.schema).await;
    let session = f.db.open_workspace_source(&auth, fence, 60).await.unwrap();
    sql.execute(
        "UPDATE th_repositories SET epoch=2 WHERE tenant=$1 AND repository=$2",
        &[&f.scope.tenant(), &f.scope.repository()],
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.finish_workspace_source(session).await,
        Err(Error::Fenced)
    ));
    sql.execute(
        "UPDATE th_repositories SET epoch=1 WHERE tenant=$1 AND repository=$2",
        &[&f.scope.tenant(), &f.scope.repository()],
    )
    .await
    .unwrap();
    sql.execute(
        "UPDATE th_workspaces SET resolved_profile=NULL WHERE workspace=$1",
        &[&spec.id],
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.open_workspace_source(&auth, fence, 60).await,
        Err(Error::ExecutionState)
    ));
    sql.execute(
        "UPDATE th_workspaces SET resolved_profile=profile WHERE workspace=$1",
        &[&spec.id],
    )
    .await
    .unwrap();
    let session = f.db.open_workspace_source(&auth, fence, 60).await.unwrap();
    f.db.drain_workspace(&ctx(&write, "drain-source"), spec.id, 1)
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_workspace_source(session).await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.open_workspace_source(&auth, fence, 60).await,
        Err(Error::ExecutionState)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn credentials_are_scoped_narrowed_hashed_and_audited_without_implicit_role_power() {
    let mut f = Fixture::new().await;
    let roles = [
        ("person", K::Human, P::RepositoryWrite),
        ("agent", K::Agent, P::WorkspaceWrite),
        ("node", K::Node, P::RuntimeControl),
        ("validator", K::Validator, P::ValidationReport),
        ("bridge", K::Bridge, P::HostPublish),
    ];
    for (name, kind, allowed) in roles {
        let token = f.token(name, kind, &[P::Read, allowed]).await;
        f.auth(&token, allowed).await;
        for denied in [
            P::RepositoryWrite,
            P::RuntimeControl,
            P::ValidationReport,
            P::HostPublish,
        ] {
            if allowed != denied {
                assert!(matches!(
                    f.db.authorize(&token.secret, &f.scope, denied).await,
                    Err(Error::Forbidden)
                ));
            }
        }
        let narrow =
            f.db.issue_repository_token(&f.scope, name, &[P::Read], 3600, "operator")
                .await
                .unwrap();
        assert!(matches!(
            f.db.authorize(&narrow.secret, &f.scope, allowed).await,
            Err(Error::Forbidden)
        ));
        let row = connection(&f.schema)
            .await
            .query_one(
                "SELECT fingerprint,to_jsonb(t)::text FROM th_access_tokens t WHERE token=$1",
                &[&token.id],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, Vec<u8>>(0).len(), 32);
        assert!(!row.get::<_, String>(1).contains(token.secret.expose()));
        assert!(!format!("{token:?}").contains(token.secret.expose()));
    }
    // Explicit grants cannot override a principal's trust class.
    for permission in [
        P::ValidationReport,
        P::RuntimeControl,
        P::HostPublish,
        P::RepositoryWrite,
    ] {
        assert!(matches!(
            f.db.set_repository_access(&f.scope, "agent", &[permission], "operator")
                .await,
            Err(Error::Forbidden)
        ));
    }
    assert!(matches!(
        f.db.issue_repository_token(&f.scope, "agent", &[P::ChangeWrite], 3600, "operator")
            .await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.db.issue_repository_token(&f.scope, "agent", &[P::Read, P::Read], 3600, "operator")
            .await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        f.db.issue_repository_token(&f.scope, "agent", &[P::Read], 0, "operator")
            .await,
        Err(Error::Invalid(_))
    ));
    let token = f.token("scope-agent", K::Agent, &[P::Read]).await;
    let other = Scope::new(f.scope.tenant(), Uuid::new_v4()).unwrap();
    f.db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    let tenant = Scope::new("another-tenant", f.scope.repository()).unwrap();
    f.db.create_repository(&tenant, Authority::Standalone)
        .await
        .unwrap();
    for foreign in [&other, &tenant] {
        assert!(matches!(
            f.db.authorize(&token.secret, foreign, P::Read).await,
            Err(Error::Forbidden)
        ));
    }
    let unknown =
        treeharbor_metadata::access::SecretToken::parse(&format!("thv1_{}", "0".repeat(64)))
            .unwrap();
    assert!(matches!(
        f.db.authorize(&unknown, &f.scope, P::Read).await,
        Err(Error::Unauthenticated)
    ));
    let raw = connection(&f.schema).await;
    let audit: String = raw
        .query_one("SELECT json_agg(a)::text FROM th_security_audit a", &[])
        .await
        .unwrap()
        .get(0);
    assert!(!audit.contains(token.secret.expose()));
    assert!(
        raw.execute("DELETE FROM th_security_audit", &[])
            .await
            .is_err()
    );
    assert!(
        raw.execute("UPDATE th_security_audit SET operator='forged'", &[])
            .await
            .is_err()
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn context_identity_permission_and_owner_cannot_be_forged() {
    let mut f = Fixture::new().await;
    let alice = f
        .token("alice", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let bob = f
        .token("bob", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let writer = f.auth(&alice, P::WorkspaceWrite).await;
    let reader = f.auth(&alice, P::Read).await;
    let spec = f.spec();
    let mut forged = ctx(&writer, "create");
    forged.principal = "bob".into();
    assert!(matches!(
        f.db.create_workspace(&forged, &spec).await,
        Err(Error::Forbidden)
    ));
    forged = ctx(&writer, "create");
    forged.scope = Scope::new("other", f.scope.repository()).unwrap();
    f.db.create_repository(&forged.scope, Authority::Standalone)
        .await
        .unwrap();
    assert!(matches!(
        f.db.create_workspace(&forged, &spec).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        f.db.create_workspace(&ctx(&reader, "create"), &spec).await,
        Err(Error::Forbidden)
    ));
    let created =
        f.db.create_workspace(&ctx(&writer, "create"), &spec)
            .await
            .unwrap();
    assert_eq!(created["state"], "requested");
    let own =
        f.db.authorized_workspace_status(&reader, spec.id)
            .await
            .unwrap();
    assert_eq!(own["base"], f.base.to_string());
    assert!(own.get("runtime").is_none());
    let bob_reader = f.auth(&bob, P::Read).await;
    assert!(matches!(
        f.db.authorized_workspace_status(&bob_reader, spec.id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.db.authorized_operation_status(&bob_reader, "create")
            .await,
        Err(Error::NotFound)
    ));
    let bob_writer = f.auth(&bob, P::WorkspaceWrite).await;
    assert!(matches!(
        f.db.drain_workspace(&ctx(&bob_writer, "drain-other"), spec.id, 1)
            .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.create_workspace(&ctx(&bob_writer, "create"), &spec)
            .await,
        Err(Error::IdempotencyConflict)
    ));
    assert_eq!(
        f.db.authorized_operation_status(&reader, "create")
            .await
            .unwrap()["result"],
        created
    );
    assert_eq!(
        f.db.authorized_workspace_status(&reader, spec.id)
            .await
            .unwrap()["generation"],
        1
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn rotation_keeps_replay_but_revocation_and_grant_reduction_fence_cached_contexts() {
    let mut f = Fixture::new().await;
    let old = f
        .token("agent", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let cached = f.auth(&old, P::WorkspaceWrite).await;
    let reader = f.auth(&old, P::Read).await;
    let spec = f.spec();
    let created =
        f.db.create_workspace(&ctx(&cached, "create"), &spec)
            .await
            .unwrap();
    f.db.revoke_repository_token(&f.scope, old.id, "operator")
        .await
        .unwrap();
    f.db.revoke_repository_token(&f.scope, old.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.create_workspace(&ctx(&cached, "create"), &spec).await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        f.db.authorized_repository_info(&reader).await,
        Err(Error::Unauthenticated)
    ));
    let new =
        f.db.issue_repository_token(
            &f.scope,
            "agent",
            &[P::Read, P::WorkspaceWrite],
            3600,
            "operator",
        )
        .await
        .unwrap();
    let current = f.auth(&new, P::WorkspaceWrite).await;
    assert_eq!(
        f.db.create_workspace(&ctx(&current, "create"), &spec)
            .await
            .unwrap(),
        created
    );
    f.db.set_repository_access(&f.scope, "agent", &[P::Read], "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.create_workspace(&ctx(&current, "create"), &spec).await,
        Err(Error::Forbidden)
    ));
    let read = f.auth(&new, P::Read).await;
    f.db.disable_principal(f.scope.tenant(), "agent", "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.authorized_operation_status(&read, "create").await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        f.db.provision_principal(f.scope.tenant(), "agent", K::Agent, "operator")
            .await,
        Err(Error::Forbidden)
    ));
    let count: i64 = connection(&f.schema)
        .await
        .query_one(
            "SELECT count(*) FROM th_security_audit WHERE action='token.revoked'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn expired_credential_rolls_back_effects_receipt_pins_and_outbox_at_final_check() {
    let mut f = Fixture::new().await;
    let token = f
        .token("agent", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let auth = f.auth(&token, P::WorkspaceWrite).await;
    let raw = connection(&f.schema).await;
    // A sequence change survives rollback and proves execution reached the final
    // receipt insertion after the first authorization check and workspace writes.
    raw.batch_execute("CREATE SEQUENCE expiry_probe; CREATE FUNCTION delay_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.key='expiry' THEN PERFORM nextval('expiry_probe'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END; $$; CREATE TRIGGER expiry_delay BEFORE INSERT ON th_operations FOR EACH ROW EXECUTE FUNCTION delay_receipt();").await.unwrap();
    raw.execute("UPDATE th_access_tokens SET expires_at=clock_timestamp()+interval '1 second' WHERE token=$1",&[&token.id]).await.unwrap();
    let spec = f.spec();
    assert!(matches!(
        f.db.create_workspace(&ctx(&auth, "expiry"), &spec).await,
        Err(Error::Unauthenticated)
    ));
    assert!(
        raw.query_one("SELECT is_called FROM expiry_probe", &[])
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspaces", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_operations WHERE key='expiry'", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'workspace:%'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_outbox", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(matches!(
        f.db.authorize(&token.secret, &f.scope, P::Read).await,
        Err(Error::Unauthenticated)
    ));
    cleanup(&f.schema).await;
}

async fn wait_blocked(raw: &tokio_postgres::Client, pid: i32) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if raw
                .query_one("SELECT cardinality(pg_blocking_pids($1))>0", &[&pid])
                .await
                .unwrap()
                .get::<_, bool>(0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("database operation did not reach expected lock boundary");
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn revocation_success_waits_for_admitted_write_and_rejects_its_next_replay() {
    let mut f = Fixture::new().await;
    let token = f.token("agent", K::Agent, &[P::WorkspaceWrite]).await;
    let auth = f.auth(&token, P::WorkspaceWrite).await;
    let spec = f.spec();
    let workspace = spec.id;
    let mut raw = connection(&f.schema).await;
    let blocker = raw.transaction().await.unwrap();
    blocker
        .batch_execute("LOCK TABLE th_workspaces IN SHARE MODE")
        .await
        .unwrap();
    let writer = connection(&f.schema).await;
    let writer_pid: i32 = writer
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut writer = Metadata::new(writer).await.unwrap();
    let writer_ctx = ctx(&auth, "racing");
    let writing = tokio::spawn(async move { writer.create_workspace(&writer_ctx, &spec).await });
    let observer = connection(&f.schema).await;
    wait_blocked(&observer, writer_pid).await;
    let revoker = connection(&f.schema).await;
    let revoker_pid: i32 = revoker
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut revoker = Metadata::new(revoker).await.unwrap();
    let scope = f.scope.clone();
    let token_id = token.id;
    let revoking = tokio::spawn(async move {
        revoker
            .revoke_repository_token(&scope, token_id, "operator")
            .await
    });
    wait_blocked(&observer, revoker_pid).await;
    assert!(!revoking.is_finished());
    blocker.commit().await.unwrap();
    assert_eq!(writing.await.unwrap().unwrap()["state"], "requested");
    revoking.await.unwrap().unwrap();
    let spec = WorkspaceSpec {
        id: workspace,
        ..f.spec()
    };
    assert!(matches!(
        f.db.create_workspace(&ctx(&auth, "racing"), &spec).await,
        Err(Error::Unauthenticated)
    ));
    assert_eq!(
        observer
            .query_one("SELECT count(*) FROM th_workspaces", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn node_observations_bind_node_identity_and_preserve_the_agent_as_run_owner() {
    let mut f = Fixture::new().await;
    let agent = f
        .token("agent", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let writer = f.auth(&agent, P::WorkspaceWrite).await;
    let node = f.token("node-a", K::Node, &[P::RuntimeControl]).await;
    let controller = f.auth(&node, P::RuntimeControl).await;
    let wrong_node = f.token("node-b", K::Node, &[P::RuntimeControl]).await;
    let wrong = f.auth(&wrong_node, P::RuntimeControl).await;
    let spec = f.spec();
    f.db.create_workspace(&ctx(&writer, "create"), &spec)
        .await
        .unwrap();
    let job =
        f.db.claim(f.scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let mut ready = WorkspaceReady {
        id: spec.id,
        generation: 1,
        node: "node-b".into(),
        runtime: "observed-runtime".into(),
    };
    assert!(matches!(
        f.db.record_workspace_ready(&ctx(&controller, "ready"), &ready, &job)
            .await,
        Err(Error::Forbidden)
    ));
    ready.node = "node-a".into();
    assert!(matches!(
        f.db.record_workspace_ready(&ctx(&writer, "fake-ready"), &ready, &job)
            .await,
        Err(Error::Forbidden)
    ));
    f.db.record_workspace_ready(&ctx(&controller, "ready"), &ready, &job)
        .await
        .unwrap();
    let run = RunningAgent {
        id: Uuid::new_v4(),
        workspace: spec.id,
        workspace_generation: 1,
        agent: "test/1".into(),
        task: "task".into(),
        runtime: "observed-runtime".into(),
    };
    assert!(matches!(
        f.db.record_running_agent(&ctx(&wrong, "wrong-node"), &run)
            .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.record_running_agent(&ctx(&writer, "fake-run"), &run)
            .await,
        Err(Error::Forbidden)
    ));
    f.db.record_running_agent(&ctx(&controller, "run"), &run)
        .await
        .unwrap();
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT principal FROM th_agent_runs WHERE run=$1",
            &[&run.id]
        )
        .await
        .unwrap()
        .get::<_, String>(0),
        "agent"
    );
    assert_eq!(
        raw.query_one("SELECT principal FROM th_operations WHERE key='run'", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "node-a"
    );
    assert_eq!(
        f.db.drain_workspace(&ctx(&writer, "drain"), spec.id, 1)
            .await
            .unwrap()["state"],
        "draining"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn published_v3_upgrade_retains_workspace_pins_and_receipts_without_granting_access() {
    use sha2::{Digest, Sha256};
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
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut hasher = Sha256::new();
        hasher.update(digest);
        hasher.update(sql.as_bytes());
        digest = hasher.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(3,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let scope = Scope::new("test-tenant", Uuid::new_v4()).unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let mut f = Fixture::from_database(schema, scope, db).await;
    let mut spec = f.spec();
    spec.profile = [0; 32]; // Published v3 allowed an unresolved caller-supplied digest.
    let trusted = CommandContext::trusted(
        f.scope.clone(),
        "legacy-create".into(),
        "legacy".into(),
        Authority::Standalone,
        1,
    );
    // Replay the published v3 creation contract against the actual v3 schema,
    // rather than asking current code to operate on an unsupported old schema.
    let RevisionRef::Native(base_id) = spec.base else {
        panic!()
    };
    let lease =
        f.db.lease_pinned_root(
            &f.scope,
            &format!("base:{}", base_id.hex()),
            Uuid::new_v4(),
            60,
        )
        .await
        .unwrap();
    let request = treeharbor_metadata::Request::new(
        f.scope.clone(),
        &trusted.key,
        &trusted.principal,
        "workspace.create.v1",
        Authority::Standalone,
        1,
        serde_json::json!([
            1,
            spec.id.to_string(),
            spec.base.to_string(),
            spec.profile,
            "native"
        ])
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let treeharbor_metadata::Begin::Fresh(tx) = f.db.begin(request).await.unwrap() else {
        panic!()
    };
    let holder = format!("workspace:{}", spec.id);
    tx.pin_object(&holder, &lease).await.unwrap();
    tx.transaction().execute("INSERT INTO th_workspaces(tenant,repository,workspace,base_revision,principal,profile,format,state,epoch,pin_holder) VALUES($1,$2,$3,$4,'legacy',$5,'native','requested',1,$6)",&[&f.scope.tenant(),&f.scope.repository(),&spec.id,&spec.base.to_string(),&spec.profile.as_slice(),&holder]).await.unwrap();
    let old=tx.complete(serde_json::json!({"workspace":spec.id.to_string(),"generation":1,"state":"requested","base":spec.base.to_string()}),vec![treeharbor_metadata::Event{kind:"workspace.prepare".into(),payload:serde_json::json!({"workspace":spec.id.to_string(),"generation":1,"epoch":1})}]).await.unwrap();
    let pins_before: i64 = raw
        .query_one("SELECT count(*) FROM th_object_pins", &[])
        .await
        .unwrap()
        .get(0);
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        10
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_access_tokens", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_principals", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_object_pins", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        pins_before
    );
    let token = f.token("legacy", K::Agent, &[P::WorkspaceWrite]).await;
    let auth = f.auth(&token, P::WorkspaceWrite).await;
    assert_eq!(
        f.db.create_workspace(&ctx(&auth, "legacy-create"), &spec)
            .await
            .unwrap(),
        old
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspaces", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let unresolved: bool = raw
        .query_one(
            "SELECT resolved_profile IS NULL FROM th_workspaces WHERE workspace=$1",
            &[&spec.id],
        )
        .await
        .unwrap()
        .get(0);
    assert!(unresolved);
    assert!(matches!(
        f.db.create_workspace(
            &ctx(&auth, "new-legacy-create"),
            &WorkspaceSpec {
                id: Uuid::new_v4(),
                base: spec.base,
                profile: spec.profile,
                format: WorkspaceFormat::Native
            }
        )
        .await,
        Err(Error::NotFound)
    ));
    let job =
        f.db.claim(f.scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    assert!(matches!(
        f.db.record_workspace_ready(
            &CommandContext::trusted(
                f.scope.clone(),
                "legacy-ready".into(),
                "legacy".into(),
                Authority::Standalone,
                1
            ),
            &WorkspaceReady {
                id: spec.id,
                generation: 1,
                node: "old-node".into(),
                runtime: "old-runtime".into()
            },
            &job
        )
        .await,
        Err(Error::ExecutionState)
    ));
    assert_eq!(
        f.db.drain_workspace(&ctx(&auth, "legacy-drain"), spec.id, 1)
            .await
            .unwrap()["state"],
        "draining"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn shared_agent_commands_enforce_authentication_replay_fencing_and_drain_semantics() {
    use serde_json::json;
    let mut f = Fixture::new().await;
    let token = f
        .token("api-agent", K::Agent, &[P::Read, P::WorkspaceWrite])
        .await;
    let spec = f.spec();
    let envelope = |command: serde_json::Value| {
        json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"command":command}).to_string()
    };
    let command = json!({"method":"workspace_create","key":"api-create","authority":"standalone","epoch":1,"workspace":spec.id.to_string(),"base":spec.base.to_string(),"profile_digest":treeharbor_core::SourceProfile::full().id().to_string()});
    let create = envelope(command.clone());
    let receipt =
        f.db.execute_agent_command(&token.secret, create.as_bytes())
            .await
            .unwrap();
    assert_eq!(receipt["result"]["state"], "requested");
    assert_eq!(
        f.db.execute_agent_command(&token.secret, create.as_bytes())
            .await
            .unwrap(),
        receipt
    );
    let info = envelope(json!({"method":"repository_info"}));
    assert_eq!(
        f.db.execute_agent_command(&token.secret, info.as_bytes())
            .await
            .unwrap()["result"]["authority"],
        "standalone"
    );
    let status = envelope(json!({"method":"operation_status","key":"api-create"}));
    assert_eq!(
        f.db.execute_agent_command(&token.secret, status.as_bytes())
            .await
            .unwrap()["result"]["result"],
        receipt["result"]
    );
    let mut spoof = command.clone();
    spoof["principal"] = json!("admin");
    assert_eq!(
        f.db.execute_agent_command(&token.secret, envelope(spoof).as_bytes())
            .await
            .unwrap_err()
            .code(),
        "invalid_request"
    );
    let mut changed = command.clone();
    changed["profile_digest"] = json!(format!("sha256:{}", "01".repeat(32)));
    assert_eq!(
        f.db.execute_agent_command(&token.secret, envelope(changed).as_bytes())
            .await
            .unwrap_err()
            .code(),
        "idempotency_conflict"
    );
    let mut stale = command;
    stale["key"] = json!("stale");
    stale["epoch"] = json!(2);
    assert_eq!(
        f.db.execute_agent_command(&token.secret, envelope(stale).as_bytes())
            .await
            .unwrap_err()
            .code(),
        "repository_fenced"
    );
    for method in [
        "workspace_ready",
        "validation_report",
        "queue_force_publish",
        "sql",
        "shell",
    ] {
        assert_eq!(
            f.db.execute_agent_command(
                &token.secret,
                envelope(json!({"method":method})).as_bytes()
            )
            .await
            .unwrap_err()
            .code(),
            "invalid_request"
        );
    }
    let delete = envelope(
        json!({"method":"workspace_delete","key":"api-delete","authority":"standalone","epoch":1,"workspace":spec.id.to_string(),"generation":1}),
    );
    assert_eq!(
        f.db.execute_agent_command(&token.secret, delete.as_bytes())
            .await
            .unwrap()["result"]["state"],
        "draining"
    );
    let workspace = envelope(json!({"method":"workspace_status","workspace":spec.id.to_string()}));
    assert_eq!(
        f.db.execute_agent_command(&token.secret, workspace.as_bytes())
            .await
            .unwrap()["result"]["generation"],
        2
    );
    assert_eq!(
        connection(&f.schema)
            .await
            .query_one(
                "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'workspace:%'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    f.db.revoke_repository_token(&f.scope, token.id, "operator")
        .await
        .unwrap();
    assert_eq!(
        f.db.execute_agent_command(&token.secret, create.as_bytes())
            .await
            .unwrap_err()
            .code(),
        "unauthenticated"
    );
    cleanup(&f.schema).await;
}
