//! Real PostgreSQL lifecycle contracts. Prepared/removed observations below are
//! trusted test inputs, not evidence that these tests executed a real sandbox.
use super::*;
use treeharbor_metadata::runs::{RunPlan, RunRequest, RuntimeVolume};
use treeharbor_runtime::{Definition, Profile};

struct Owner {
    token: IssuedToken,
    write: Authorization,
    read: Authorization,
}
async fn owner(f: &mut Fixture, name: &str) -> Owner {
    f.db.provision_principal(f.scope.tenant(), name, K::Human, "operator")
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
    let write =
        f.db.authorize(&token.secret, &f.scope, P::WorkspaceWrite)
            .await
            .unwrap();
    let read =
        f.db.authorize(&token.secret, &f.scope, P::Read)
            .await
            .unwrap();
    Owner { token, write, read }
}
async fn prepared(f: &mut Fixture, node: &Node) -> RunRequest {
    let workspace = f.workspace().await;
    let slot = StorageSlot {
        storage: node.storage,
        slot: Uuid::new_v4(),
    };
    f.db.register_runtime_volume(
        f.scope.tenant(),
        "node",
        &RuntimeVolume {
            slot,
            digest: [71; 32],
            limit_bytes: 134217728,
        },
        "operator",
    )
    .await
    .unwrap();
    let profile = Profile::new(Definition {
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
    f.db.register_runtime_profile(&f.scope, &profile, "operator")
        .await
        .unwrap();
    let job = f.claim(node).await;
    let plan =
        f.db.reserve_preparation(&node.auth, &job, slot)
            .await
            .unwrap();
    assert_eq!(plan.volume_digest(), Some([71; 32]));
    f.db.complete_preparation(&node.auth, &plan, &f.observed())
        .await
        .unwrap();
    RunRequest {
        run: Uuid::new_v4(),
        workspace,
        generation: 1,
        profile: profile.digest(),
        argv: vec!["/bin/tool".into(), "literal $(not a host shell)".into()],
        agent: "fixture-agent".into(),
        task: "fixture-task".into(),
    }
}
fn ctx(owner: &Owner, key: &str) -> CommandContext {
    owner.write.context(key.into(), Authority::Standalone, 1)
}
async fn claim(f: &mut Fixture, node: &Node) -> RunPlan {
    f.db.claim_run(&node.auth, node.storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap()
}
fn wire(f: &Fixture, command: serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"command":command})).unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runtime observations are trusted test inputs"]
async fn managed_terminal_proposals_require_removed_runtime_and_current_workspace() {
    use treeharbor_metadata::{proposals::ChangeUploadSpec, uploads::UploadLimits};
    for cancel in [false, true] {
        let mut f = Fixture::new().await;
        let owner = owner(&mut f, "owner").await;
        let auth =
            f.db.authorize(&owner.token.secret, &f.scope, P::ChangeWrite)
                .await
                .unwrap();
        let node = f.node("node").await;
        let input = prepared(&mut f, &node).await;
        f.db.request_run(&ctx(&owner, "request"), &input)
            .await
            .unwrap();
        let spec = ChangeUploadSpec {
            upload: Uuid::new_v4(),
            run: input.run,
            generation: 1,
            limits: UploadLimits {
                objects: 10,
                bytes: 1024,
                ttl_seconds: 60,
            },
        };
        let context = auth.context("begin-change".into(), Authority::Standalone, 1);
        assert!(matches!(
            f.db.begin_change_upload(&context, "preparation-tests", &spec)
                .await,
            Err(Error::ExecutionState)
        ));
        let plan = claim(&mut f, &node).await;
        if cancel {
            f.db.stop_run(&ctx(&owner, "cancel"), input.run)
                .await
                .unwrap();
        }
        assert!(matches!(
            f.db.begin_change_upload(&context, "preparation-tests", &spec)
                .await,
            Err(Error::ExecutionState)
        ));
        // A trusted removal observation creates eligibility; unknown exit stays lost.
        let result = f.db.finish_run(&node.auth, &plan).await.unwrap();
        assert_eq!(result["state"], if cancel { "cancelled" } else { "lost" });
        let begin =
            f.db.begin_change_upload(&context, "preparation-tests", &spec)
                .await
                .unwrap();
        assert_eq!(begin["state"], "active");
        f.db.drain_workspace(&ctx(&owner, "drain"), input.workspace, 1)
            .await
            .unwrap();
        let next = ChangeUploadSpec {
            upload: Uuid::new_v4(),
            ..spec
        };
        assert!(matches!(
            f.db.begin_change_upload(
                &auth.context("after-drain".into(), Authority::Standalone, 1),
                "preparation-tests",
                &next
            )
            .await,
            Err(Error::ExecutionState)
        ));
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn managed_request_is_owner_bound_durable_and_idempotent_through_command_boundary() {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let mut input = prepared(&mut f, &node).await;
    let correct = input.profile;
    input.profile = [0; 32];
    assert!(matches!(
        f.db.request_run(&ctx(&owner, "request"), &input).await,
        Err(Error::ExecutionState)
    ));
    input.profile = correct;
    let request = wire(
        &f,
        json!({"method":"run_request","key":"request","authority":"standalone","epoch":1,
        "run":input.run.to_string(),"workspace":input.workspace.to_string(),"generation":1,
        "profile_digest":ObjectId::from_bytes(input.profile).to_string(),"argv":input.argv,
        "agent":input.agent,"task":input.task}),
    );
    let mut second = Metadata::new(connection(&f.schema).await).await.unwrap();
    let (one, two) = tokio::join!(
        f.db.execute_agent_command(&owner.token.secret, &request),
        second.execute_agent_command(&owner.token.secret, &request)
    );
    assert_eq!(one.unwrap(), two.unwrap());
    let raw = connection(&f.schema).await;
    let counts = raw.query_one("SELECT (SELECT count(*) FROM th_managed_runs), (SELECT count(*) FROM th_outbox WHERE kind='run.execute')", &[]).await.unwrap();
    assert_eq!(counts.get::<_, i64>(0), 1);
    assert_eq!(counts.get::<_, i64>(1), 1);
    let state = raw
        .query_one(
            "SELECT state,runtime,started_at IS NULL FROM th_agent_runs WHERE run=$1",
            &[&input.run],
        )
        .await
        .unwrap();
    assert_eq!(state.get::<_, String>(0), "requested");
    assert_eq!(state.get::<_, Option<String>>(1), None);
    assert!(state.get::<_, bool>(2));
    input.argv.push("changed".into());
    assert!(matches!(
        f.db.request_run(&ctx(&owner, "request"), &input).await,
        Err(Error::IdempotencyConflict)
    ));
    input.run = Uuid::new_v4();
    assert!(matches!(
        f.db.request_run(&ctx(&owner, "second-run"), &input).await,
        Err(Error::ExecutionState)
    ));
    let run: Uuid = raw
        .query_one("SELECT run FROM th_managed_runs", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        f.db.run_status(&owner.read, run).await.unwrap()["state"],
        "requested"
    );
    let other = self::owner(&mut f, "other-owner").await;
    assert!(matches!(
        f.db.run_status(&other.read, run).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.db.stop_run(&ctx(&other, "stop-other"), run).await,
        Err(Error::NotFound)
    ));
    let mut injected: serde_json::Value = serde_json::from_slice(&request).unwrap();
    injected["command"]["image"] = json!("attacker:latest");
    assert_eq!(
        f.db.execute_agent_command(&owner.token.secret, &serde_json::to_vec(&injected).unwrap())
            .await
            .unwrap_err()
            .code(),
        "invalid_request"
    );
    assert!(
        raw.execute(
            "UPDATE th_agent_runs SET principal='other-owner' WHERE run=$1",
            &[&run]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute(
            "UPDATE th_managed_runs SET authority='connected' WHERE run=$1",
            &[&run]
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn managed_reclaim_fences_old_worker_and_terminal_receipt_survives_lost_ack() {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let input = prepared(&mut f, &node).await;
    f.db.request_run(&ctx(&owner, "request"), &input)
        .await
        .unwrap();
    let other = f.node("other-node").await;
    assert!(
        f.db.claim_run(&other.auth, other.storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    let old = claim(&mut f, &node).await;
    assert_eq!(old.task().argv(), input.argv);
    assert_eq!(old.prepared().revision, f.base);
    assert_eq!(old.prepared().snapshot, f.snapshot);
    assert_eq!(old.prepared().volume_digest, Some([71; 32]));
    f.db.admit_run_start(&node.auth, &old).await.unwrap();
    assert_eq!(
        f.db.run_status(&owner.read, input.run).await.unwrap()["state"],
        "launching"
    );
    let raw = connection(&f.schema).await;
    raw.execute("UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE kind='run.execute'", &[]).await.unwrap();
    let current = claim(&mut f, &node).await;
    assert_eq!(old.attempt(), current.attempt());
    assert_eq!(old.binding(), current.binding());
    assert!(matches!(
        f.db.admit_run_start(&node.auth, &old).await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        f.db.finish_run(&node.auth, &old).await,
        Err(Error::StaleLease)
    ));
    let pins = raw
        .query_one("SELECT count(*) FROM th_object_pins", &[])
        .await
        .unwrap()
        .get::<_, i64>(0);
    f.db.stop_run(&ctx(&owner, "stop"), input.run)
        .await
        .unwrap();
    assert!(f.db.renew_run(&node.auth, &current, 60).await.unwrap());
    assert!(matches!(
        f.db.admit_run_start(&node.auth, &current).await,
        Err(Error::ExecutionState)
    ));
    // Trusted removal input only. The Linux harness must prove actual teardown.
    let finished = f.db.finish_run(&node.auth, &current).await.unwrap();
    assert_eq!(finished["state"], "cancelled");
    let mut restarted = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        restarted.finish_run(&node.auth, &current).await.unwrap(),
        finished
    );
    assert_eq!(
        restarted.completed_run(&node.auth, &current).await.unwrap(),
        Some(finished)
    );
    assert!(matches!(
        restarted.finish_run(&node.auth, &old).await,
        Err(Error::StaleLease)
    ));
    assert!(matches!(
        restarted.completed_run(&other.auth, &current).await,
        Err(Error::Forbidden)
    ));
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_object_pins", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        pins
    );
    assert!(raw.execute("UPDATE th_agent_runs SET state='running',runtime='fabricated',started_at=clock_timestamp() WHERE run=$1", &[&input.run]).await.is_err());
    assert!(
        raw.execute(
            "UPDATE th_managed_runs SET finished_worker=$2 WHERE run=$1",
            &[&input.run, &Uuid::new_v4()]
        )
        .await
        .is_err()
    );
    f.db.revoke_repository_token(&f.scope, node.token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        restarted.finish_run(&node.auth, &current).await,
        Err(Error::Unauthenticated)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn managed_policy_fences_launch_but_retains_authenticated_cleanup() {
    for fence in ["drain", "epoch", "profile", "owner-grant", "owner-disabled"] {
        let mut f = Fixture::new().await;
        let owner = owner(&mut f, "owner").await;
        let node = f.node("node").await;
        let input = prepared(&mut f, &node).await;
        f.db.request_run(&ctx(&owner, "request"), &input)
            .await
            .unwrap();
        let raw = connection(&f.schema).await;
        match fence {
            "drain" => {
                f.db.drain_workspace(&ctx(&owner, "drain"), input.workspace, 1)
                    .await
                    .unwrap();
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=epoch+1", &[])
                    .await
                    .unwrap();
            }
            "profile" => {
                f.db.revoke_runtime_profile(&f.scope, input.profile, "operator")
                    .await
                    .unwrap();
            }
            "owner-grant" => {
                f.db.set_repository_access(&f.scope, "owner", &[P::Read], "operator")
                    .await
                    .unwrap();
            }
            "owner-disabled" => {
                raw.execute(
                    "UPDATE th_principals SET enabled=false WHERE principal='owner'",
                    &[],
                )
                .await
                .unwrap();
            }
            _ => unreachable!(),
        }
        let plan = claim(&mut f, &node).await;
        assert!(plan.stop_requested(), "{fence}");
        assert!(
            matches!(
                f.db.admit_run_start(&node.auth, &plan).await,
                Err(Error::ExecutionState)
            ),
            "{fence}"
        );
        assert!(f.db.renew_run(&node.auth, &plan, 60).await.unwrap());
        assert_eq!(
            f.db.finish_run(&node.auth, &plan).await.unwrap()["state"],
            "cancelled",
            "{fence}"
        );
        assert!(
            raw.query_one("SELECT count(*)>0 FROM th_object_pins", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn managed_finish_rolls_back_if_lease_expires_inside_transaction() {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let input = prepared(&mut f, &node).await;
    f.db.request_run(&ctx(&owner, "request"), &input)
        .await
        .unwrap();
    let plan = claim(&mut f, &node).await;
    let raw = connection(&f.schema).await;
    raw.batch_execute("CREATE FUNCTION delay_finish() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(0.2); RETURN NEW; END $$; CREATE TRIGGER delay_finish BEFORE UPDATE ON th_managed_runs FOR EACH ROW WHEN (NEW.removed) EXECUTE FUNCTION delay_finish();").await.unwrap();
    raw.execute("UPDATE th_outbox SET lease_until=clock_timestamp()+interval '0.1 second' WHERE kind='run.execute'", &[]).await.unwrap();
    assert!(matches!(
        f.db.finish_run(&node.auth, &plan).await,
        Err(Error::StaleLease)
    ));
    let status = f.db.run_status(&owner.read, input.run).await.unwrap();
    assert_eq!(status["state"], "requested");
    assert_eq!(status["runtime_removed"], false);
    assert!(
        f.db.completed_run(&node.auth, &plan)
            .await
            .unwrap()
            .is_none()
    );
    let successor = claim(&mut f, &node).await;
    assert_eq!(
        f.db.finish_run(&node.auth, &successor).await.unwrap()["state"],
        "lost"
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn managed_launch_holds_owner_revocation_fence_through_commit() {
    let mut f = Fixture::new().await;
    let owner = owner(&mut f, "owner").await;
    let node = f.node("node").await;
    let input = prepared(&mut f, &node).await;
    f.db.request_run(&ctx(&owner, "request"), &input)
        .await
        .unwrap();
    let plan = claim(&mut f, &node).await;
    let gate = connection(&f.schema).await;
    let key = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    gate.query_one("SELECT pg_advisory_lock($1)", &[&key])
        .await
        .unwrap();
    gate.batch_execute(&format!("CREATE FUNCTION hold_launch() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({key}); RETURN NEW; END $$; CREATE TRIGGER hold_launch BEFORE UPDATE ON th_agent_runs FOR EACH ROW WHEN (NEW.state='launching') EXECUTE FUNCTION hold_launch();")).await.unwrap();
    let client = connection(&f.schema).await;
    let launch_pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut launcher = Metadata::new(client).await.unwrap();
    let auth = node.auth.clone();
    let launch = tokio::spawn(async move {
        let result = launcher.admit_run_start(&auth, &plan).await;
        (result, plan)
    });
    wait_blocked(&gate, launch_pid, "advisory").await;
    let client = connection(&f.schema).await;
    let revoke_pid: i32 = client
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let mut revoker = Metadata::new(client).await.unwrap();
    let tenant = f.scope.tenant().to_owned();
    let revoke = tokio::spawn(async move {
        revoker
            .disable_principal(&tenant, "owner", "operator")
            .await
    });
    wait_blocked(&gate, revoke_pid, "transactionid").await;
    assert!(!revoke.is_finished());
    gate.query_one("SELECT pg_advisory_unlock($1)", &[&key])
        .await
        .unwrap();
    let (result, plan) = launch.await.unwrap();
    result.unwrap();
    revoke.await.unwrap().unwrap();
    assert!(f.db.renew_run(&node.auth, &plan, 60).await.unwrap());
    assert!(matches!(
        f.db.admit_run_start(&node.auth, &plan).await,
        Err(Error::ExecutionState)
    ));
    assert_eq!(
        f.db.finish_run(&node.auth, &plan).await.unwrap()["state"],
        "cancelled"
    );
    cleanup(&f.schema).await;
}

async fn wait_blocked(client: &tokio_postgres::Client, pid: i32, event: &str) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if client
                .query_one(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event=$2)",
                    &[&pid, &event],
                )
                .await
                .unwrap()
                .get::<_, bool>(0)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("transaction did not reach expected database lock");
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn populated_v7_upgrade_preserves_legacy_run_and_reserved_preparation() {
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
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut hash = Sha256::new();
        hash.update(digest);
        hash.update(sql.as_bytes());
        digest = hash.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(7,$1)",
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
    let node = f.node("node").await;
    let pending = f.workspace().await;
    let attempt = Uuid::new_v4();
    let slot = Uuid::new_v4();
    // Build valid v7 records directly; calling v8 preparation code before
    // migration would not be a test of the old schema's durable data.
    raw.execute("INSERT INTO th_workspace_preparations(attempt,tenant,repository,workspace,generation,epoch,node,storage_instance,slot,job_id,base_revision,snapshot,profile,state) SELECT $1,w.tenant,w.repository,w.workspace,w.generation,w.epoch,'node',$2,$3,o.id,w.base_revision,b.snapshot,w.resolved_profile,'reserved' FROM th_workspaces w JOIN th_bases b ON b.tenant=w.tenant AND b.repository=w.repository AND b.revision=w.base_revision JOIN th_outbox o ON o.tenant=w.tenant AND o.repository=w.repository AND o.kind='workspace.prepare' AND o.payload->>'workspace'=w.workspace::text WHERE w.workspace=$4",
        &[&attempt,&node.storage,&slot,&pending]).await.unwrap();
    raw.execute(
        "UPDATE th_workspaces SET node='node' WHERE workspace=$1",
        &[&pending],
    )
    .await
    .unwrap();
    let legacy = f.workspace().await;
    let run = Uuid::new_v4();
    raw.execute("UPDATE th_workspaces SET node='node',state='in_use',runtime='legacy-runtime' WHERE workspace=$1", &[&legacy]).await.unwrap();
    raw.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state) SELECT tenant,repository,$1,workspace,generation,epoch,principal,'legacy-agent','legacy-task',runtime,'running' FROM th_workspaces WHERE workspace=$2", &[&run,&legacy]).await.unwrap();
    let before = upgrade_snapshot(&raw).await;
    assert!(matches!(
        f.db.check_schema().await,
        Err(Error::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    assert_eq!(before, upgrade_snapshot(&raw).await);
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_managed_runs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let job = f.claim(&node).await;
    assert_eq!(job.payload["workspace"], pending.to_string());
    let recovered =
        f.db.reserve_preparation(
            &node.auth,
            &job,
            StorageSlot {
                storage: node.storage,
                slot,
            },
        )
        .await
        .unwrap();
    assert_eq!(recovered.attempt(), attempt);
    assert_eq!(recovered.volume_digest(), None);
    f.db.complete_preparation(&node.auth, &recovered, &f.observed())
        .await
        .unwrap();
    cleanup(&f.schema).await;
}

async fn upgrade_snapshot(raw: &tokio_postgres::Client) -> serde_json::Value {
    raw.query_one("SELECT jsonb_build_object('runs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY run) FROM th_agent_runs r),'preparations',(SELECT jsonb_agg(to_jsonb(p) ORDER BY attempt) FROM th_workspace_preparations p),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o),'pins',(SELECT jsonb_agg(to_jsonb(p) ORDER BY holder) FROM th_object_pins p))", &[]).await.unwrap().get(0)
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; populated prior-version upgrade"]
async fn populated_v8_upgrade_preserves_managed_run_and_can_accept_new_uploads() {
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
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(8,$1)",
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
    let input = prepared(&mut f, &node).await;
    let request =
        f.db.request_run(&ctx(&owner, "request"), &input)
            .await
            .unwrap();
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
        request,
        f.db.request_run(&ctx(&owner, "request"), &input)
            .await
            .unwrap()
    );
    let plan = claim(&mut f, &node).await;
    assert_eq!(
        f.db.finish_run(&node.auth, &plan).await.unwrap()["state"],
        "lost"
    );
    let auth =
        f.db.authorize(&owner.token.secret, &f.scope, P::ChangeWrite)
            .await
            .unwrap();
    f.db.begin_change_upload(
        &auth.context("after-upgrade".into(), Authority::Standalone, 1),
        "preparation-tests",
        &treeharbor_metadata::proposals::ChangeUploadSpec {
            upload: Uuid::new_v4(),
            run: input.run,
            generation: 1,
            limits: treeharbor_metadata::uploads::UploadLimits {
                objects: 10,
                bytes: 4096,
                ttl_seconds: 60,
            },
        },
    )
    .await
    .unwrap();
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn reserved_legacy_slot_cannot_gain_a_runtime_volume_after_the_fact() {
    let mut f = Fixture::new().await;
    let node = f.node("node").await;
    f.workspace().await;
    let job = f.claim(&node).await;
    let plan = f.reserve(&node, &job).await;
    assert_eq!(plan.volume_digest(), None);
    assert!(matches!(
        f.db.register_runtime_volume(
            f.scope.tenant(),
            "node",
            &RuntimeVolume {
                slot: plan.slot(),
                digest: [71; 32],
                limit_bytes: 134217728,
            },
            "operator"
        )
        .await,
        Err(Error::ExecutionState)
    ));
    f.db.complete_preparation(&node.auth, &plan, &f.observed())
        .await
        .unwrap();
    assert_eq!(
        f.db.prepared_workspace(&node.auth, plan.workspace())
            .await
            .unwrap()
            .volume_digest,
        None
    );
    cleanup(&f.schema).await;
}
