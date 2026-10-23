//! Real PostgreSQL tests of trusted cleanup observations. No fixture here proves
//! that runtime processes stopped or that a physical directory was deleted.
use super::*;
use treeharbor_metadata::cleanup::{CleanupPlan, DeletedFiles};

fn proof() -> DeletedFiles {
    DeletedFiles {
        intent_digest: [31; 32],
        receipt_digest: [32; 32],
    }
}
async fn cleanup_claim(f: &mut Fixture, node: &Node, workspace: Uuid) -> CleanupPlan {
    f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap()
}
async fn drain(f: &mut Fixture, workspace: Uuid) {
    f.db.drain_workspace(&f.context(&format!("drain-{workspace}")), workspace, 1)
        .await
        .unwrap();
}
async fn pins(raw: &tokio_postgres::Client) -> serde_json::Value {
    raw.query_one(
        "SELECT COALESCE(jsonb_object_agg(holder,to_jsonb(p)), '{}'::jsonb) FROM th_object_pins p",
        &[],
    )
    .await
    .unwrap()
    .get(0)
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; trusted deletion receipts only"]
async fn cleanup_requires_drain_and_admission_for_unreserved_partial_and_ready_workspaces() {
    for prepared_state in 0..3 {
        let mut f = Fixture::new().await;
        let node = f.node("node").await;
        let workspace = f.workspace().await;
        let preparation = if prepared_state > 0 {
            let job = f.claim(&node).await;
            let p = f.reserve(&node, &job).await;
            if prepared_state == 2 {
                f.db.complete_preparation(&node.auth, &p, &f.observed())
                    .await
                    .unwrap();
            }
            Some(p)
        } else {
            None
        };
        assert!(matches!(
            f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
                .await,
            Err(Error::ExecutionState)
        ));
        let raw = connection(&f.schema).await;
        let holder: String = raw
            .query_one(
                "SELECT pin_holder FROM th_workspaces WHERE workspace=$1",
                &[&workspace],
            )
            .await
            .unwrap()
            .get(0);
        let before = pins(&raw).await;
        drain(&mut f, workspace).await;
        let p = cleanup_claim(&mut f, &node, workspace).await;
        assert_eq!(p.generation(), 2);
        assert_eq!(
            p.placement().map(|p| p.attempt),
            preparation.as_ref().map(Preparation::attempt)
        );
        assert_eq!(
            p.placement().and_then(|p| p.manifest_digest),
            if prepared_state == 2 {
                Some([91; 32])
            } else {
                None
            }
        );
        assert!(p.runtime().is_none());
        assert!(matches!(
            f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
                .await,
            Err(Error::ExecutionState)
        ));
        assert_eq!(pins(&raw).await, before);
        f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
            .await
            .unwrap();
        f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
            .await
            .unwrap();
        assert!(matches!(
            f.db.admit_workspace_cleanup(&node.auth, &p, [99; 32]).await,
            Err(Error::IdempotencyConflict)
        ));
        let result =
            f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
                .await
                .unwrap();
        assert_eq!(result["state"], "deleted");
        assert_eq!(
            f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
                .await
                .unwrap(),
            result
        );
        assert_eq!(
            f.db.completed_workspace_cleanup(&node.auth, workspace, node.storage)
                .await
                .unwrap(),
            Some(result)
        );
        assert!(
            f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
                .await
                .unwrap()
                .is_none()
        );
        let mut expected = before;
        expected.as_object_mut().unwrap().remove(&holder).unwrap();
        assert_eq!(
            pins(&raw).await,
            expected,
            "only this workspace's pin may be released"
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='workspace.deleted'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            1
        );
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
        let row = raw
            .query_one(
                "SELECT state,pin_holder FROM th_workspaces WHERE workspace=$1",
                &[&workspace],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "deleted");
        assert_eq!(row.get::<_, Option<String>>(1), None);
        if let Some(preparation) = preparation {
            assert!(matches!(
                f.db.complete_preparation(&node.auth, &preparation, &f.observed())
                    .await,
                Err(Error::ExecutionState)
            ));
            let new_workspace = f.workspace().await;
            let next = f.claim(&node).await;
            let replacement =
                f.db.reserve_preparation(&node.auth, &next, preparation.slot())
                    .await
                    .unwrap();
            assert_eq!(replacement.workspace(), new_workspace);
            assert_ne!(replacement.attempt(), preparation.attempt());
            assert_eq!(
                f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
                    .await
                    .unwrap()["state"],
                "deleted"
            );
            assert_eq!(
                raw.query_one(
                    "SELECT state FROM th_workspace_preparations WHERE attempt=$1",
                    &[&replacement.attempt()]
                )
                .await
                .unwrap()
                .get::<_, String>(0),
                "reserved"
            );
            assert_eq!(
                raw.query_one(
                    "SELECT state FROM th_workspace_preparations WHERE attempt=$1",
                    &[&preparation.attempt()]
                )
                .await
                .unwrap()
                .get::<_, String>(0),
                "released"
            );
            assert!(
                raw.execute(
                    "UPDATE th_workspace_preparations SET state='draining' WHERE attempt=$1",
                    &[&preparation.attempt()]
                )
                .await
                .is_err()
            );
            assert!(
                raw.execute(
                    "UPDATE th_workspace_preparations SET slot=$2 WHERE attempt=$1",
                    &[&preparation.attempt(), &Uuid::new_v4()]
                )
                .await
                .is_err()
            );
        }
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; lease and restart contracts"]
async fn cleanup_reclaim_preserves_admitted_intent_and_rejects_old_worker() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    drain(&mut f, workspace).await;
    let old = cleanup_claim(&mut f, &node, workspace).await;
    f.db.admit_workspace_cleanup(&node.auth, &old, proof().intent_digest)
        .await
        .unwrap();
    assert!(
        f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()-interval '1 second'",
        &[],
    )
    .await
    .unwrap();
    let mut restarted = Metadata::new(connection(&f.schema).await).await.unwrap();
    let next = restarted
        .claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(old.id(), next.id());
    assert_eq!(next.intent(), Some(proof().intent_digest));
    assert!(matches!(
        f.db.renew_workspace_cleanup(&node.auth, &old, 60).await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        f.db.admit_workspace_cleanup(&node.auth, &old, proof().intent_digest)
            .await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        f.db.complete_workspace_cleanup(&node.auth, &old, &proof())
            .await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        restarted
            .admit_workspace_cleanup(&node.auth, &next, [44; 32])
            .await,
        Err(Error::IdempotencyConflict)
    ));
    restarted
        .renew_workspace_cleanup(&node.auth, &next, 60)
        .await
        .unwrap();
    restarted
        .admit_workspace_cleanup(&node.auth, &next, proof().intent_digest)
        .await
        .unwrap();
    let result = restarted
        .complete_workspace_cleanup(&node.auth, &next, &proof())
        .await
        .unwrap();
    assert!(matches!(
        f.db.complete_workspace_cleanup(&node.auth, &old, &proof())
            .await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        restarted
            .complete_workspace_cleanup(
                &node.auth,
                &next,
                &DeletedFiles {
                    intent_digest: proof().intent_digest,
                    receipt_digest: [42; 32]
                }
            )
            .await,
        Err(Error::IdempotencyConflict)
    ));
    // Completed response does not require a live lease or the old authority.
    raw.execute(
        "UPDATE th_repositories SET epoch=epoch+1,state='frozen'",
        &[],
    )
    .await
    .unwrap();
    assert_eq!(
        restarted
            .complete_workspace_cleanup(&node.auth, &next, &proof())
            .await
            .unwrap(),
        result
    );
    assert!(
        raw.execute(
            "UPDATE th_workspace_cleanups SET receipt_digest=$1",
            &[&[1u8; 32].as_slice()]
        )
        .await
        .is_err()
    );
    f.db.revoke_repository_token(&f.scope, node.token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        restarted
            .complete_workspace_cleanup(&node.auth, &next, &proof())
            .await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        restarted
            .completed_workspace_cleanup(&node.auth, workspace, node.storage)
            .await,
        Err(Error::Unauthenticated)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; trusted removal prerequisite"]
async fn cleanup_waits_for_managed_removal_and_preserves_run_history() {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let run = prepared(&mut f, &node).await;
    f.db.request_run(&ctx(&owner, "run"), &run).await.unwrap();
    drain(&mut f, run.workspace).await;
    assert!(matches!(
        f.db.claim_workspace_cleanup(&node.auth, run.workspace, node.storage, Uuid::new_v4(), 60)
            .await,
        Err(Error::ExecutionState)
    ));
    let execution = claim(&mut f, &node).await;
    assert!(execution.stop_requested());
    f.db.finish_run(&node.auth, &execution).await.unwrap();
    let raw = connection(&f.schema).await;
    let run_before: serde_json::Value = raw
        .query_one("SELECT to_jsonb(m) FROM th_managed_runs m", &[])
        .await
        .unwrap()
        .get(0);
    let p = cleanup_claim(&mut f, &node, run.workspace).await;
    assert_eq!(p.runtime().unwrap().run(), run.run);
    assert_eq!(p.runtime().unwrap().prepared().generation, 1);
    // Drain was already accepted. Owner revocation does not reverse destruction
    // or leak resources; current node authorization remains mandatory.
    f.db.set_repository_access(&f.scope, "owner", &[], "operator")
        .await
        .unwrap();
    f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
        .await
        .unwrap();
    f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
        .await
        .unwrap();
    assert_eq!(
        raw.query_one("SELECT to_jsonb(m) FROM th_managed_runs m", &[])
            .await
            .unwrap()
            .get::<_, serde_json::Value>(0),
        run_before
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_agent_runs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; unmanaged runtimes must remain quarantined"]
async fn cleanup_rejects_unmanaged_runtime_even_with_a_terminal_run() {
    for has_preparation in [false, true] {
        let mut f = Fixture::new().await;
        let node = f.node("node").await;
        let workspace = f.workspace().await;
        if has_preparation {
            let job = f.claim(&node).await;
            f.reserve(&node, &job).await;
        }
        let raw = connection(&f.schema).await;
        // Model retained records from the earlier trusted/unmanaged interface.
        raw.execute("UPDATE th_workspaces SET node='node',state='in_use',runtime='legacy-runtime' WHERE workspace=$1", &[&workspace]).await.unwrap();
        raw.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state) SELECT tenant,repository,$1,workspace,generation,epoch,principal,'legacy-agent','legacy-task',runtime,'succeeded' FROM th_workspaces WHERE workspace=$2", &[&Uuid::new_v4(),&workspace]).await.unwrap();
        let before = pins(&raw).await;
        drain(&mut f, workspace).await;
        assert!(matches!(
            f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
                .await,
            Err(Error::ExecutionState)
        ));
        assert_eq!(pins(&raw).await, before);
        assert_eq!(
            raw.query_one("SELECT count(*) FROM th_workspace_cleanups", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; current authority and credential fences"]
async fn cleanup_claims_are_scoped_and_every_mutation_rechecks_current_fences() {
    for fence in [
        "epoch",
        "freeze",
        "authority",
        "node_token",
        "node_grant",
        "node_disabled",
        "generation",
    ] {
        let mut f = Fixture::new().await;
        let node = f.node("node").await;
        let other = f.node("other-node").await;
        let workspace = f.workspace().await;
        let prepare = f.claim(&node).await;
        f.reserve(&node, &prepare).await;
        drain(&mut f, workspace).await;
        assert!(
            f.db.claim_workspace_cleanup(&other.auth, workspace, other.storage, Uuid::new_v4(), 60)
                .await
                .is_err()
        );
        assert!(
            f.db.claim_workspace_cleanup(&node.auth, workspace, other.storage, Uuid::new_v4(), 60)
                .await
                .is_err()
        );
        let p = cleanup_claim(&mut f, &node, workspace).await;
        f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
            .await
            .unwrap();
        let raw = connection(&f.schema).await;
        let before = pins(&raw).await;
        match fence {
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
            "authority" => {
                raw.execute("UPDATE th_repositories SET authority='connected'", &[])
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
            "node_disabled" => {
                raw.execute(
                    "UPDATE th_principals SET enabled=false WHERE principal='node'",
                    &[],
                )
                .await
                .unwrap();
            }
            "generation" => {
                raw.execute("UPDATE th_workspaces SET generation=generation+1", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.db.renew_workspace_cleanup(&node.auth, &p, 60)
                .await
                .is_err(),
            "{fence}"
        );
        assert!(
            f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
                .await
                .is_err(),
            "{fence}"
        );
        assert!(
            f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
                .await
                .is_err(),
            "{fence}"
        );
        assert_eq!(pins(&raw).await, before, "{fence}");
        assert_eq!(
            raw.query_one("SELECT state FROM th_workspace_preparations", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            "draining"
        );
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; concurrent worker claims"]
async fn cleanup_concurrent_claims_have_one_winner_and_agent_cannot_supply_deletion() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let (_, agent) = f.token("agent", K::Agent, P::Read).await;
    let workspace = f.workspace().await;
    drain(&mut f, workspace).await;
    assert!(matches!(
        f.db.claim_workspace_cleanup(&agent, workspace, node.storage, Uuid::new_v4(), 60)
            .await,
        Err(Error::Forbidden)
    ));
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        f.db.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60),
        other.claim_workspace_cleanup(&node.auth, workspace, node.storage, Uuid::new_v4(), 60)
    );
    assert_ne!(a.unwrap().is_some(), b.unwrap().is_some());
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; expiry during final writes"]
async fn cleanup_expiry_rolls_back_receipt_pin_and_slot_release() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    let workspace = f.workspace().await;
    let job = f.claim(&node).await;
    f.reserve(&node, &job).await;
    drain(&mut f, workspace).await;
    let p = cleanup_claim(&mut f, &node, workspace).await;
    f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
        .await
        .unwrap();
    let raw = connection(&f.schema).await;
    let before = pins(&raw).await;
    // This delay is after slot/pin/receipt writes and the operation record.
    raw.batch_execute("CREATE FUNCTION cleanup_delay() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.4); RETURN NEW; END $$; CREATE TRIGGER cleanup_delay BEFORE INSERT ON th_outbox FOR EACH ROW WHEN(NEW.kind='workspace.deleted') EXECUTE FUNCTION cleanup_delay()").await.unwrap();
    raw.execute(
        "UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()+interval '0.2 second'",
        &[],
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
            .await,
        Err(Error::StaleLease)
    ));
    assert_eq!(pins(&raw).await, before);
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
    assert_eq!(
        raw.query_one("SELECT state FROM th_workspace_cleanups", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "deleting"
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_operations WHERE kind='workspace.cleanup.complete.v1'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    let successor = cleanup_claim(&mut f, &node, workspace).await;
    f.db.complete_workspace_cleanup(&node.auth, &successor, &proof())
        .await
        .unwrap();
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; expiry during admission and renewal"]
async fn cleanup_admission_and_renewal_cannot_resurrect_expired_claim() {
    for renewal in [false, true] {
        let mut f = Fixture::new().await;
        let node = f.node("node").await;
        let workspace = f.workspace().await;
        drain(&mut f, workspace).await;
        let p = cleanup_claim(&mut f, &node, workspace).await;
        let raw = connection(&f.schema).await;
        raw.execute(
            "UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()+interval '0.2 second'",
            &[],
        )
        .await
        .unwrap();
        raw.batch_execute("CREATE FUNCTION cleanup_delay() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.4); RETURN NEW; END $$; CREATE TRIGGER cleanup_delay BEFORE UPDATE ON th_workspace_cleanups FOR EACH ROW EXECUTE FUNCTION cleanup_delay()").await.unwrap();
        let result = if renewal {
            f.db.renew_workspace_cleanup(&node.auth, &p, 60).await
        } else {
            f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
                .await
        };
        assert!(
            matches!(result, Err(Error::StaleLease)),
            "renewal={renewal}"
        );
        let row=raw.query_one("SELECT state,intent_digest,lease_until>clock_timestamp() FROM th_workspace_cleanups",&[]).await.unwrap();
        assert_eq!(row.get::<_, String>(0), "claimed");
        assert!(row.get::<_, Option<Vec<u8>>>(1).is_none());
        assert!(!row.get::<_, bool>(2));
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; populated schema13 upgrade"]
async fn cleanup_v13_upgrade_preserves_existing_workspace_and_run_records() {
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
        include_str!("../../migrations/013_workspace_seals.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(13,$1)",
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
    let workspace_before: serde_json::Value = raw
        .query_one("SELECT to_jsonb(w) FROM th_workspaces w", &[])
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
    assert_eq!(upgrade_snapshot(&raw).await, before);
    assert_eq!(
        raw.query_one("SELECT to_jsonb(w) FROM th_workspaces w", &[])
            .await
            .unwrap()
            .get::<_, serde_json::Value>(0),
        workspace_before
    );
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        20
    );
    drain(&mut f, run.workspace).await;
    let stopped = claim(&mut f, &node).await;
    f.db.finish_run(&node.auth, &stopped).await.unwrap();
    let p = cleanup_claim(&mut f, &node, run.workspace).await;
    f.db.admit_workspace_cleanup(&node.auth, &p, proof().intent_digest)
        .await
        .unwrap();
    f.db.complete_workspace_cleanup(&node.auth, &p, &proof())
        .await
        .unwrap();
    cleanup(&f.schema).await;
}
