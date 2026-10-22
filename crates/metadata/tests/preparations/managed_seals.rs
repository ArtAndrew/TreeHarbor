//! Real PostgreSQL contracts. Runtime removal here is trusted test input; only
//! the separate Linux runtime harness can prove physical stopped-file capture.
use super::*;
use treeharbor_core::{CaptureSelection, RepoPath};
use treeharbor_metadata::{seals::SealRequest, uploads::UploadLimits};
fn request(run: Uuid) -> SealRequest {
    SealRequest {
        seal: Uuid::new_v4(),
        run,
        generation: 1,
        selection: CaptureSelection::new(
            vec![RepoPath::new(b"src".to_vec()).unwrap()],
            vec![],
            vec![],
        )
        .unwrap(),
        limits: UploadLimits {
            objects: 100,
            bytes: 1048576,
            ttl_seconds: 300,
        },
    }
}
async fn live() -> (Fixture, Owner, Node, RunRequest) {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let run = prepared(&mut f, &node).await;
    f.db.request_run(&ctx(&owner, "run"), &run).await.unwrap();
    (f, owner, node, run)
}
async fn removed(f: &mut Fixture, node: &Node) {
    let run = claim(f, node).await;
    assert!(run.stop_requested());
    f.db.finish_run(&node.auth, &run).await.unwrap();
}
async fn claim_seal(f: &mut Fixture, node: &Node) -> treeharbor_metadata::seals::SealPlan {
    f.db.claim_workspace_seal(&node.auth, node.storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap()
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; not a physical quiescence proof"]
async fn seal_waits_for_removed_run_and_replays_exact_owner_request() {
    let (mut f, owner, node, run) = live().await;
    let input = request(run.run);
    let context = ctx(&owner, "seal");
    let result = f.db.request_workspace_seal(&context, &input).await.unwrap();
    assert_eq!(result["state"], "waiting");
    assert_eq!(
        f.db.request_workspace_seal(&context, &input).await.unwrap(),
        result
    );
    assert_eq!(
        f.db.run_status(&owner.read, run.run).await.unwrap()["state"],
        "stopping"
    );
    assert!(
        f.db.claim_workspace_seal(&node.auth, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let other = f.node("other-node").await;
    removed(&mut f, &node).await;
    assert!(
        f.db.claim_workspace_seal(&other.auth, other.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let mut restarted = Metadata::new(connection(&f.schema).await).await.unwrap();
    let plan = restarted
        .claim_workspace_seal(&node.auth, node.storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(plan.id(), input.seal);
    assert_eq!(plan.context().run(), run.run);
    assert_eq!(plan.context().prepared().workspace, run.workspace);
    assert_eq!(plan.context().prepared().snapshot, f.snapshot);
    assert_eq!(plan.context().prepared().node, "node");
    assert_eq!(plan.context().engine(), None);
    assert_eq!(plan.context().container(), None);
    assert_eq!(plan.selection().encode(), input.selection.encode());
    assert_eq!(plan.max_objects(), 100);
    assert_eq!(plan.max_bytes(), 1048576);
    assert!(
        restarted
            .claim_workspace_seal(&node.auth, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    restarted
        .renew_workspace_seal(&node.auth, &plan, 120)
        .await
        .unwrap();
    let raw = connection(&f.schema).await;
    assert!(
        raw.query_one(
            "SELECT lease_until<=expires_at FROM th_workspace_seals WHERE seal=$1",
            &[&input.seal]
        )
        .await
        .unwrap()
        .get::<_, bool>(0)
    );
    let holder = format!("seal/{}/base", input.seal);
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder=$1",
            &[&holder]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    let cancelled = restarted
        .cancel_workspace_seal(&ctx(&owner, "cancel"), input.seal)
        .await
        .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    assert!(matches!(
        restarted.renew_workspace_seal(&node.auth, &plan, 60).await,
        Err(Error::StaleLease)
    ));
    assert_eq!(
        restarted
            .request_workspace_seal(&context, &input)
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        restarted
            .workspace_seal_status(&owner.read, input.seal)
            .await
            .unwrap()["state"],
        "cancelled"
    );
    assert!(
        raw.execute(
            "UPDATE th_workspace_seals SET state='waiting',completed_at=NULL WHERE seal=$1",
            &[&input.seal]
        )
        .await
        .is_err()
    );
    let mut changed = request(run.run);
    changed.seal = input.seal;
    changed.limits.bytes += 1;
    assert!(matches!(
        restarted.request_workspace_seal(&context, &changed).await,
        Err(Error::IdempotencyConflict)
    ));
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; not a physical quiescence proof"]
async fn seal_reclaim_keeps_artifact_binding_but_fences_previous_worker() {
    let (mut f, owner, node, run) = live().await;
    let input = request(run.run);
    f.db.request_workspace_seal(&ctx(&owner, "seal"), &input)
        .await
        .unwrap();
    removed(&mut f, &node).await;
    let old = claim_seal(&mut f, &node).await;
    let raw = connection(&f.schema).await;
    raw.execute("UPDATE th_workspace_seals SET lease_until=clock_timestamp()-interval '1 second' WHERE seal=$1",&[&input.seal]).await.unwrap();
    assert!(matches!(
        f.db.renew_workspace_seal(&node.auth, &old, 60).await,
        Err(Error::StaleLease)
    ));
    let current = claim_seal(&mut f, &node).await;
    assert_eq!(old.binding(), current.binding());
    assert_eq!(old.context().binding(), current.context().binding());
    assert!(matches!(
        f.db.renew_workspace_seal(&node.auth, &old, 60).await,
        Err(Error::StaleLease)
    ));
    f.db.renew_workspace_seal(&node.auth, &current, 3600)
        .await
        .unwrap();
    assert!(
        raw.query_one(
            "SELECT lease_until=expires_at FROM th_workspace_seals WHERE seal=$1",
            &[&input.seal]
        )
        .await
        .unwrap()
        .get::<_, bool>(0)
    );
    for statement in [
        "UPDATE th_workspace_seals SET selection='changed' WHERE seal=$1",
        "UPDATE th_workspace_seals SET max_bytes=max_bytes+1 WHERE seal=$1",
        "UPDATE th_workspace_seals SET expires_at=expires_at+interval '1 second' WHERE seal=$1",
        "UPDATE th_workspace_seals SET claim_generation=0 WHERE seal=$1",
    ] {
        assert!(raw.execute(statement, &[&input.seal]).await.is_err());
    }
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; not a physical quiescence proof"]
async fn seal_current_access_context_and_node_fences_are_not_bypassed_by_claim() {
    for fence in [
        "owner_disabled",
        "owner_grant",
        "node_token",
        "node_grant",
        "drain",
        "epoch",
        "freeze",
    ] {
        let (mut f, owner, node, run) = live().await;
        let input = request(run.run);
        f.db.request_workspace_seal(&ctx(&owner, "seal"), &input)
            .await
            .unwrap();
        removed(&mut f, &node).await;
        let plan = claim_seal(&mut f, &node).await;
        let raw = connection(&f.schema).await;
        match fence {
            "owner_disabled" => {
                raw.execute(
                    "UPDATE th_principals SET enabled=false WHERE principal='owner'",
                    &[],
                )
                .await
                .unwrap();
            }
            "owner_grant" => {
                f.db.set_repository_access(&f.scope, "owner", &[P::Read], "operator")
                    .await
                    .unwrap();
            }
            "node_token" => {
                f.db.revoke_repository_token(&f.scope, node.token.id, "operator")
                    .await
                    .unwrap();
            }
            "node_grant" => {
                f.db.set_repository_access(&f.scope, "node", &[P::Read], "operator")
                    .await
                    .unwrap();
            }
            "drain" => {
                f.db.drain_workspace(&ctx(&owner, "drain"), run.workspace, 1)
                    .await
                    .unwrap();
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=epoch+1", &[])
                    .await
                    .unwrap();
            }
            "freeze" => {
                raw.execute("UPDATE th_repositories SET state='frozen'", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.db.renew_workspace_seal(&node.auth, &plan, 60)
                .await
                .is_err(),
            "{fence}"
        );
        cleanup(&f.schema).await;
    }
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; not a physical quiescence proof"]
async fn seal_concurrent_requests_have_one_winner_and_no_foreign_owner_access() {
    let (mut f, owner, node, run) = live().await;
    let a = request(run.run);
    let b = request(run.run);
    let ac = ctx(&owner, "a");
    let bc = ctx(&owner, "b");
    let mut second = Metadata::new(connection(&f.schema).await).await.unwrap();
    let (ar, br) = tokio::join!(
        f.db.request_workspace_seal(&ac, &a),
        second.request_workspace_seal(&bc, &b)
    );
    assert_ne!(ar.is_ok(), br.is_ok());
    let id = if ar.is_ok() { a.seal } else { b.seal };
    let other = super::owner(&mut f, "another-owner").await;
    assert!(matches!(
        f.db.workspace_seal_status(&other.read, id).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.db.cancel_workspace_seal(&ctx(&other, "foreign-cancel"), id)
            .await,
        Err(Error::NotFound)
    ));
    assert!(
        f.db.request_workspace_seal(&ctx(&other, "foreign-request"), &request(run.run))
            .await
            .is_err()
    );
    assert!(f.db.workspace_seal_status(&node.auth, id).await.is_err());
    assert!(
        f.db.claim_workspace_seal(&owner.write, node.storage, Uuid::new_v4(), 60)
            .await
            .is_err()
    );
    assert!(
        f.db.request_workspace_seal(
            &node
                .auth
                .context("node-request".into(), Authority::Standalone, 1),
            &request(run.run)
        )
        .await
        .is_err()
    );
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_seals", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'seal/%'",
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
#[ignore = "requires disposable PostgreSQL; not a physical quiescence proof"]
async fn expired_seal_does_not_get_new_deadline_on_retry_or_claim() {
    let (mut f, owner, node, run) = live().await;
    let mut input = request(run.run);
    input.limits.ttl_seconds = 1;
    let context = ctx(&owner, "seal");
    let first = f.db.request_workspace_seal(&context, &input).await.unwrap();
    removed(&mut f, &node).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(
        f.db.claim_workspace_seal(&node.auth, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        f.db.request_workspace_seal(&context, &input).await.unwrap(),
        first
    );
    assert_eq!(
        f.db.workspace_seal_status(&owner.read, input.seal)
            .await
            .unwrap()["unexpired"],
        false
    );
    f.db.cancel_workspace_seal(&ctx(&owner, "cancel"), input.seal)
        .await
        .unwrap();
    let next = request(run.run);
    f.db.request_workspace_seal(&ctx(&owner, "new-selection"), &next)
        .await
        .unwrap();
    assert_eq!(claim_seal(&mut f, &node).await.id(), next.seal);
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; not a physical quiescence proof"]
async fn seal_rejects_incomplete_sparse_subtree_before_stopping_run() {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let source = SourceProfile::sparse(vec![RepoPath::new(b"src/file".to_vec()).unwrap()]).unwrap();
    f.db.put_source_profile(&ctx(&owner, "sparse"), &source)
        .await
        .unwrap();
    let workspace = Uuid::new_v4();
    f.db.create_workspace(
        &ctx(&owner, "workspace"),
        &WorkspaceSpec {
            id: workspace,
            base: f.base,
            profile: *source.id().as_bytes(),
            format: WorkspaceFormat::Native,
        },
    )
    .await
    .unwrap();
    let run = prepare_workspace(&mut f, &node, workspace, source).await;
    f.db.request_run(&ctx(&owner, "run"), &run).await.unwrap();
    let mut input = request(run.run);
    assert!(matches!(
        f.db.request_workspace_seal(&ctx(&owner, "bad-selection"), &input)
            .await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(
        f.db.run_status(&owner.read, run.run).await.unwrap()["state"],
        "requested"
    );
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_seals", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'seal/%'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    input.selection = CaptureSelection::new(
        vec![RepoPath::new(b"src/file".to_vec()).unwrap()],
        vec![],
        vec![],
    )
    .unwrap();
    f.db.request_workspace_seal(&ctx(&owner, "valid-selection"), &input)
        .await
        .unwrap();
    removed(&mut f, &node).await;
    assert_eq!(
        claim_seal(&mut f, &node).await.selection().encode(),
        input.selection.encode()
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; preserves actual populated schema12"]
async fn populated_v12_upgrade_preserves_managed_run_and_source_pins_for_seal() {
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let mut digest = Vec::new();
    for sql in [
        include_str!("../../migrations/001_transactions.sql"),
        include_str!("../../migrations/002_object_catalog.sql"),
        include_str!("../../migrations/003_change_intake.sql"),
        include_str!("../../migrations/004_access_control.sql"),
        include_str!("../../migrations/005_source_profiles.sql"),
        include_str!("../../migrations/006_source_uploads.sql"),
        include_str!("../../migrations/007_workspace_preparations.sql"),
        include_str!("../../migrations/008_managed_runs.sql"),
        include_str!("../../migrations/009_change_uploads.sql"),
        include_str!("../../migrations/010_native_integration.sql"),
        include_str!("../../migrations/011_check_execution.sql"),
        include_str!("../../migrations/012_change_rebase.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(12,$1)",
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
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let run = prepared(&mut f, &node).await;
    f.db.request_run(&ctx(&owner, "run"), &run).await.unwrap();
    let before = upgrade_snapshot(&raw).await;
    let managed: serde_json::Value = raw
        .query_one("SELECT to_jsonb(m) FROM th_managed_runs m", &[])
        .await
        .unwrap()
        .get(0);
    assert!(matches!(
        f.db.check_schema().await,
        Err(Error::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    assert_eq!(before, upgrade_snapshot(&raw).await);
    assert_eq!(
        managed,
        raw.query_one("SELECT to_jsonb(m) FROM th_managed_runs m", &[])
            .await
            .unwrap()
            .get::<_, serde_json::Value>(0)
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_workspace_seals", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let input = request(run.run);
    f.db.request_workspace_seal(&ctx(&owner, "seal"), &input)
        .await
        .unwrap();
    removed(&mut f, &node).await;
    let plan = claim_seal(&mut f, &node).await;
    assert_eq!(plan.context().prepared().snapshot, f.snapshot);
    f.db.renew_workspace_seal(&node.auth, &plan, 60)
        .await
        .unwrap();
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; exercises deadline during transaction"]
async fn seal_deadline_expiring_inside_claim_or_renewal_rolls_back() {
    for renewal in [false, true] {
        let (mut f, owner, node, run) = live().await;
        let mut input = request(run.run);
        input.limits.ttl_seconds = 2;
        f.db.request_workspace_seal(&ctx(&owner, "seal"), &input)
            .await
            .unwrap();
        removed(&mut f, &node).await;
        let old = if renewal {
            Some(claim_seal(&mut f, &node).await)
        } else {
            None
        };
        let raw = connection(&f.schema).await;
        let before: serde_json::Value = raw
            .query_one(
                "SELECT to_jsonb(c) FROM th_workspace_seals c WHERE seal=$1",
                &[&input.seal],
            )
            .await
            .unwrap()
            .get(0);
        raw.batch_execute("CREATE FUNCTION test_seal_delay() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(2.1); RETURN NEW; END $$; CREATE TRIGGER test_seal_delay BEFORE UPDATE ON th_workspace_seals FOR EACH ROW EXECUTE FUNCTION test_seal_delay()").await.unwrap();
        if let Some(plan) = old {
            assert!(matches!(
                f.db.renew_workspace_seal(&node.auth, &plan, 60).await,
                Err(Error::StaleLease)
            ));
        } else {
            assert!(matches!(
                f.db.claim_workspace_seal(&node.auth, node.storage, Uuid::new_v4(), 60)
                    .await,
                Err(Error::StaleLease)
            ));
        }
        assert_eq!(
            before,
            raw.query_one(
                "SELECT to_jsonb(c) FROM th_workspace_seals c WHERE seal=$1",
                &[&input.seal]
            )
            .await
            .unwrap()
            .get::<_, serde_json::Value>(0)
        );
        cleanup(&f.schema).await;
    }
}
