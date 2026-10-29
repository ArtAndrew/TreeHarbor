use super::*;
use treeharbor_metadata::{
    access::{Authorization, Permission as P, PrincipalKind},
    api_runs::ApiRunSpec,
    workspace_reads::SourceFence,
};

async fn owner(f: &mut Fixture, name: &str) -> (Authorization, Authorization, Authorization) {
    f.db.provision_principal(f.scope.tenant(), name, PrincipalKind::Agent, "operator")
        .await
        .unwrap();
    let permissions = [P::Read, P::WorkspaceWrite, P::ChangeWrite];
    f.db.set_repository_access(&f.scope, name, &permissions, "operator")
        .await
        .unwrap();
    let token =
        f.db.issue_repository_token(&f.scope, name, &permissions, 3600, "operator")
            .await
            .unwrap();
    (
        f.db.authorize(&token.secret, &f.scope, P::WorkspaceWrite)
            .await
            .unwrap(),
        f.db.authorize(&token.secret, &f.scope, P::Read)
            .await
            .unwrap(),
        f.db.authorize(&token.secret, &f.scope, P::ChangeWrite)
            .await
            .unwrap(),
    )
}
fn spec(f: &Fixture) -> ApiRunSpec {
    ApiRunSpec {
        workspace: Uuid::new_v4(),
        run: Uuid::new_v4(),
        base: f.revision,
        profile: *SourceProfile::full().id().as_bytes(),
        agent: "external-controller/1".into(),
        task: "edit-file".into(),
    }
}
fn ctx(auth: &Authorization, key: &str) -> CommandContext {
    auth.context(key.into(), Authority::Standalone, 1)
}

#[tokio::test]
#[ignore = "requires PostgreSQL/object store; external provenance, fencing and retained graphs"]
async fn two_api_contexts_accept_overlapping_changes_and_close_without_runtime_receipts() {
    let mut f = Fixture::new().await;
    let (auth, read, change) = owner(&mut f, "api-owner").await;
    let (stranger, stranger_read, _) = owner(&mut f, "other-owner").await;
    let raw = connection(&f.schema).await;
    let rolled_back = spec(&f);
    raw.batch_execute("CREATE FUNCTION reject_api_creation() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'test-only interruption'; END $$; CREATE TRIGGER reject_api_creation BEFORE INSERT ON th_agent_runs FOR EACH ROW WHEN (NEW.origin='external_api') EXECUTE FUNCTION reject_api_creation()").await.unwrap();
    assert!(
        f.db.create_api_run(&ctx(&auth, "rollback-create"), &rolled_back)
            .await
            .is_err()
    );
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_workspaces WHERE delivery='api')+(SELECT count(*) FROM th_object_pins WHERE holder=$1)+(SELECT count(*) FROM th_operations WHERE key='rollback-create')",&[&format!("workspace:{}",rolled_back.workspace)]).await.unwrap().get::<_,i64>(0),0);
    raw.batch_execute(
        "DROP TRIGGER reject_api_creation ON th_agent_runs; DROP FUNCTION reject_api_creation()",
    )
    .await
    .unwrap();
    let mut invalid = spec(&f);
    invalid.run = Uuid::nil();
    assert!(matches!(
        f.db.create_api_run(&ctx(&auth, "nil-run"), &invalid).await,
        Err(MetaError::Invalid(_))
    ));
    let a = spec(&f);
    let b = spec(&f);
    let before = connection(&f.schema)
        .await
        .query_one("SELECT count(*) FROM th_outbox", &[])
        .await
        .unwrap()
        .get::<_, i64>(0);
    let created =
        f.db.create_api_run(&ctx(&auth, "create-a"), &a)
            .await
            .unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        created,
        f.db.create_api_run(&ctx(&auth, "create-a"), &a)
            .await
            .unwrap()
    );
    f.db.create_api_run(&ctx(&auth, "create-b"), &b)
        .await
        .unwrap();
    assert!(matches!(
        f.db.create_api_run(&ctx(&read, "wrong-permission"), &spec(&f))
            .await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        f.db.close_api_run(
            &auth.context("stale-epoch".into(), Authority::Standalone, 2),
            a.run,
            1
        )
        .await,
        Err(MetaError::Fenced)
    ));

    assert_eq!(created["state"], "recorded");
    assert_eq!(created["workspace_state"], "source_ready");
    assert_eq!(created["execution_observed"], false);
    assert!(matches!(
        f.db.api_run_status(&stranger_read, a.run).await,
        Err(MetaError::NotFound)
    ));
    assert!(matches!(
        f.db.close_api_run(&ctx(&stranger, "foreign-close"), a.run, 1)
            .await,
        Err(MetaError::ExecutionState)
    ));
    assert!(matches!(
        f.db.close_api_run(&ctx(&auth, "stale-close"), a.run, 2)
            .await,
        Err(MetaError::ExecutionState)
    ));
    assert!(matches!(
        f.db.stop_run(&ctx(&auth, "not-stop"), a.run).await,
        Err(MetaError::NotFound)
    ));
    let fence = SourceFence {
        workspace: a.workspace,
        generation: 1,
        authority: Authority::Standalone,
        epoch: 1,
    };
    let source = f.db.open_workspace_source(&read, fence, 60).await.unwrap();
    assert_eq!(source.identity().revision, f.revision);
    assert_eq!(source.identity().snapshot, f.base);
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_outbox", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        before
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_workspace_preparations WHERE workspace IN ($1,$2)",
            &[&a.workspace, &b.workspace]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    for statement in [
        "UPDATE th_agent_runs SET state='succeeded' WHERE origin='external_api'",
        "UPDATE th_agent_runs SET origin='control_plane' WHERE origin='external_api'",
        "UPDATE th_workspaces SET state='ready' WHERE delivery='api'",
        "UPDATE th_workspaces SET delivery='files' WHERE delivery='api'",
    ] {
        assert!(raw.batch_execute(statement).await.is_err());
    }
    let mut changed = spec(&f);
    changed.workspace = a.workspace;
    changed.run = a.run;
    changed.task = "different-task".into();
    assert!(matches!(
        f.db.create_api_run(&ctx(&auth, "create-a"), &changed).await,
        Err(MetaError::IdempotencyConflict)
    ));

    let (applied_a, ops_a, result_a) = f.prepare(b"agent-a").await;
    let (applied_b, ops_b, result_b) = f.prepare(b"agent-b").await;
    let ca = Uuid::new_v4();
    let cb = Uuid::new_v4();
    let sub_a = ChangeSubmission {
        id: ca,
        previous_version: 0,
        run: a.run,
        workspace_generation: 1,
        applied: &applied_a,
        operations: &ops_a,
        result: &result_a,
    };
    let sub_b = ChangeSubmission {
        id: cb,
        previous_version: 0,
        run: b.run,
        workspace_generation: 1,
        applied: &applied_b,
        operations: &ops_b,
        result: &result_b,
    };
    let accepted_a =
        f.db.accept_change(&ctx(&change, "accept-a"), &sub_a)
            .await
            .unwrap();
    let accepted_b =
        f.db.accept_change(&ctx(&change, "accept-b"), &sub_b)
            .await
            .unwrap();
    assert_eq!(accepted_a["base_snapshot"], accepted_b["base_snapshot"]);
    assert_ne!(accepted_a["result_snapshot"], accepted_b["result_snapshot"]);
    assert_eq!(accepted_a["run_origin"], "external_api");
    assert_eq!(accepted_a["execution_observed"], false);
    assert_eq!(
        f.db.change_version(&f.scope, ca, 1).await.unwrap(),
        accepted_a
    );
    let closed =
        f.db.close_api_run(&ctx(&auth, "close-a"), a.run, 1)
            .await
            .unwrap();
    assert_eq!(closed["state"], "deleted");
    assert_eq!(closed["generation"], 2);
    assert_eq!(closed["run_state"], "closed");
    assert!(closed.get("runtime_removed").is_none());
    assert_eq!(
        f.db.close_api_run(&ctx(&auth, "close-a"), a.run, 1)
            .await
            .unwrap(),
        closed
    );
    assert_eq!(
        f.db.api_run_status(&read, a.run).await.unwrap()["state"],
        "closed"
    );
    assert!(matches!(
        f.db.open_workspace_source(&read, fence, 60).await,
        Err(MetaError::ExecutionState)
    ));
    assert_eq!(
        f.db.accept_change(&ctx(&change, "accept-a"), &sub_a)
            .await
            .unwrap(),
        accepted_a
    );
    assert!(matches!(
        f.db.accept_change(&ctx(&change, "late-a"), &sub_a).await,
        Err(MetaError::ExecutionState)
    ));
    let deleted =
        f.db.drain_workspace(&ctx(&auth, "delete-b"), b.workspace, 1)
            .await
            .unwrap();
    assert_eq!(deleted["run_state"], "closed");
    // Expire temporary holds: retained proposal pins, not an upload/read lease,
    // must protect the accepted graphs after both workspace pins are gone.
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    assert!(
        f.db.try_retire_object(result_a.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        f.db.try_retire_object(result_b.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder IN ($1,$2)",
            &[
                &format!("workspace:{}", a.workspace),
                &format!("workspace:{}", b.workspace)
            ]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_cleanups", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='workspace.drain'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    f.db.set_repository_access(&f.scope, "api-owner", &[], "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.create_api_run(&ctx(&auth, "create-a"), &a).await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        f.db.api_run_status(&read, a.run).await,
        Err(MetaError::Forbidden)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; close/accept race is atomic and new writes are fenced"]
async fn api_close_racing_acceptance_keeps_receipt_and_pins_or_rejects_the_write() {
    let mut f = Fixture::new().await;
    let (auth, read, change) = owner(&mut f, "api-owner").await;
    let a = spec(&f);
    f.db.create_api_run(&ctx(&auth, "create"), &a)
        .await
        .unwrap();
    let (applied, operations, result) = f.prepare(b"racing").await;
    let submission = ChangeSubmission {
        id: Uuid::new_v4(),
        previous_version: 0,
        run: a.run,
        workspace_generation: 1,
        applied: &applied,
        operations: &operations,
        result: &result,
    };
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let accepting = ctx(&change, "accept");
    let closing = ctx(&auth, "close");
    let (accepted, closed) = tokio::join!(
        f.db.accept_change(&accepting, &submission),
        other.close_api_run(&closing, a.run, 1)
    );
    closed.unwrap();
    match accepted {
        Ok(value) => assert_eq!(
            f.db.change_version(&f.scope, submission.id, 1)
                .await
                .unwrap(),
            value
        ),
        Err(MetaError::ExecutionState) => assert!(matches!(
            f.db.change_version(&f.scope, submission.id, 1).await,
            Err(MetaError::NotFound)
        )),
        Err(error) => panic!("unexpected race error: {error}"),
    }
    assert_eq!(
        f.db.api_run_status(&read, a.run).await.unwrap()["state"],
        "closed"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated v31 migration retains legacy identities and replay"]
async fn api_run_migration_preserves_existing_workspace_run_and_operation_records() {
    use sha2::{Digest, Sha256};
    let mut f = Fixture::new().await;
    let raw = connection(&f.schema).await;
    crate::common::drop_unpopulated_v32(&raw).await;
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(31) {
        digest = Sha256::digest([digest, std::fs::read(path).unwrap()].concat()).to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=31,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    let snapshot = "SELECT jsonb_build_object('workspaces',(SELECT jsonb_agg(to_jsonb(w)-'delivery' ORDER BY workspace) FROM th_workspaces w),'runs',(SELECT jsonb_agg(to_jsonb(r)-'origin' ORDER BY run) FROM th_agent_runs r),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o),'pins',(SELECT jsonb_agg(to_jsonb(p) ORDER BY holder) FROM th_object_pins p),'outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM th_outbox o))";
    let before: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    assert!(matches!(
        f.db.check_schema().await,
        Err(MetaError::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    assert_eq!(
        raw.query_one(snapshot, &[])
            .await
            .unwrap()
            .get::<_, Value>(0),
        before
    );
    assert_eq!(
        raw.query_one("SELECT delivery FROM th_workspaces", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "files"
    );
    assert_eq!(
        raw.query_one("SELECT origin FROM th_agent_runs", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "control_plane"
    );
    let replay =
        f.db.create_workspace(
            &context(&f.scope, &format!("create-{}", f.workspace)),
            &WorkspaceSpec {
                id: f.workspace,
                base: f.revision,
                profile: *SourceProfile::full().id().as_bytes(),
                format: WorkspaceFormat::Native,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        replay,
        json!({"workspace":f.workspace.to_string(),"generation":1,"state":"requested","base":f.revision.to_string()})
    );
    cleanup(&f.schema).await;
}
