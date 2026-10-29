use super::*;
use treeharbor_core::SourceProfile;
use treeharbor_metadata::Error;
use treeharbor_metadata::{
    integration::CheckProgram,
    preparations::{PreparedFiles, StorageSlot},
    runs::RuntimeVolume,
};
use treeharbor_runtime::{Definition, Profile};
async fn program(f: &mut Fixture, a: &Actors) -> (Profile, CheckProgram) {
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
    let p = CheckProgram {
        version: 1,
        runtime_profile: ObjectId::from_bytes(profile.digest()).to_string(),
        argv: vec!["/check".into(), "literal $(not shell)".into()],
    };
    let admin = auth(f, &a.admin, P::PolicyWrite).await;
    let result =
        f.db.put_native_check_program(&admin, &cmd("program"), &p)
            .await
            .unwrap();
    assert_eq!(
        result,
        f.db.put_native_check_program(&admin, &cmd("program"), &p)
            .await
            .unwrap()
    );
    let mut policy = policy();
    policy.checks[0].definition = p.digest().unwrap().to_string();
    policy.checks[0].environment = p.runtime_profile.clone();
    policy.checks[0].toolchain = profile.definition().image.clone();
    policy.checks[0].dependencies = profile.definition().image.clone();
    f.db.put_native_policy(&admin, &cmd("check-policy"), &policy)
        .await
        .unwrap();
    f.db.change_native_policy(
        &admin,
        &cmd("check-select"),
        "main",
        1,
        ObjectId::from_bytes(policy.digest().unwrap()),
    )
    .await
    .unwrap();
    (profile, p)
}
async fn assigned(f: &mut Fixture, a: &Actors, candidate: Uuid) -> (Uuid, Authorization) {
    let worker = auth(f, &a.worker, P::IntegrationControl).await;
    let run = Uuid::new_v4();
    f.db.assign_native_check(
        &worker,
        &cmd(&format!("assign-{run}")),
        candidate,
        "tests",
        run,
        "ci",
    )
    .await
    .unwrap();
    (run, auth(f, &a.validator, P::ValidationReport).await)
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; no runtime success is fabricated"]
async fn managed_checks_bind_fresh_sources_replay_and_reject_manual_success() {
    let mut f = Fixture::new().await;
    let a = controls(&mut f).await;
    let (profile, p) = program(&mut f, &a).await;
    let entry = proposal(&mut f, &a, "managed", b"tested\n").await;
    let (cid, worker) = start(&mut f, &a, entry).await;
    let candidate = prepare(&mut f, &worker, cid).await;
    let (run, validator) = assigned(&mut f, &a, cid).await;
    assert!(matches!(
        f.db.report_native_validation(
            &validator,
            &cmd("fake-pass"),
            run,
            Uuid::new_v4(),
            "passed",
            ObjectId::from_bytes([9; 32])
        )
        .await,
        Err(Error::Forbidden)
    ));
    let owner = auth(&mut f, &a.owner, P::Read).await;
    assert!(matches!(
        f.db.prepare_native_check(&owner, &cmd("owner-prepare"), run)
            .await,
        Err(Error::Forbidden)
    ));
    let first =
        f.db.prepare_native_check(&validator, &cmd("check-prepare"), run)
            .await
            .unwrap();
    assert_eq!(
        first,
        f.db.prepare_native_check(&validator, &cmd("check-prepare"), run)
            .await
            .unwrap()
    );
    assert_eq!(
        first,
        f.db.prepare_native_check(&validator, &cmd("check-prepare-again"), run)
            .await
            .unwrap()
    );
    assert!(matches!(
        f.db.start_native_check(&validator, &cmd("check-start"), run)
            .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.collect_native_check(&validator, &cmd("check-collect"), run)
            .await,
        Err(Error::ExecutionState)
    ));
    assert!(matches!(
        f.db.release_native_check_workspace(&validator, &cmd("early-release"), run)
            .await,
        Err(Error::ExecutionState)
    ));
    let node_token = actor(
        &mut f,
        "check-node",
        PrincipalKind::Node,
        &[P::RuntimeControl],
    )
    .await;
    let node = auth(&mut f, &node_token, P::RuntimeControl).await;
    let storage = Uuid::new_v4();
    f.db.register_node_storage(f.scope.tenant(), "check-node", storage, "operator")
        .await
        .unwrap();
    let slot = StorageSlot {
        storage,
        slot: Uuid::new_v4(),
    };
    f.db.register_runtime_volume(
        f.scope.tenant(),
        "check-node",
        &RuntimeVolume {
            slot,
            digest: [71; 32],
            limit_bytes: 134217728,
        },
        "operator",
    )
    .await
    .unwrap();
    let job =
        f.db.claim_preparation(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let prep = f.db.reserve_preparation(&node, &job, slot).await.unwrap();
    // Trusted filesystem fixture only; Linux CI exercises actual source files.
    f.db.complete_preparation(
        &node,
        &prep,
        &PreparedFiles {
            snapshot: candidate.commit().snapshot,
            profile: SourceProfile::full().id(),
            manifest_digest: [91; 32],
        },
    )
    .await
    .unwrap();
    let started =
        f.db.start_native_check(&validator, &cmd("check-start"), run)
            .await
            .unwrap();
    assert_eq!(
        started,
        f.db.start_native_check(&validator, &cmd("check-start"), run)
            .await
            .unwrap()
    );
    assert_eq!(
        started,
        f.db.start_native_check(&validator, &cmd("check-start-again"), run)
            .await
            .unwrap()
    );
    let plan =
        f.db.claim_run(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    assert!(plan.is_check());
    assert!(!plan.stop_requested());
    assert_eq!(plan.profile().digest(), profile.digest());
    assert_eq!(plan.task().argv(), p.argv);
    assert_eq!(
        plan.prepared().revision,
        RevisionRef::Native(candidate.revision())
    );
    f.db.admit_run_start(&node, &plan).await.unwrap();
    assert!(
        f.db.collect_native_check(&validator, &cmd("early"), run)
            .await
            .is_err()
    );
    // Without an exit observation, exact runtime removal can only record lost.
    assert_eq!(
        f.db.finish_run(&node, &plan).await.unwrap()["state"],
        "lost"
    );
    let collected =
        f.db.collect_native_check(&validator, &cmd("collect"), run)
            .await
            .unwrap();
    let mut fresh = Metadata::new(connection(&f.schema).await).await.unwrap();
    assert_eq!(
        collected,
        fresh
            .collect_native_check(&validator, &cmd("collect"), run)
            .await
            .unwrap()
    );
    assert_eq!(
        collected,
        fresh
            .collect_native_check(&validator, &cmd("collect-again"), run)
            .await
            .unwrap()
    );
    let raw = connection(&f.schema).await;
    let row=raw.query_one("SELECT outcome,encode(evidence,'hex'),encode(digest,'hex') FROM th_native_validations v JOIN th_check_execution_evidence e USING(tenant,repository,receipt)",&[]).await.unwrap();
    assert_eq!(row.get::<_, String>(0), "error");
    assert_eq!(row.get::<_, String>(1), row.get::<_, String>(2));
    let evidence: serde_json::Value = raw
        .query_one("SELECT to_jsonb(e) FROM th_check_execution_evidence e", &[])
        .await
        .unwrap()
        .get(0);
    assert!(matches!(
        f.db.release_native_check_workspace(&owner, &cmd("owner-release"), run)
            .await,
        Err(Error::Forbidden)
    ));
    let other_token = actor(
        &mut f,
        "other-ci",
        PrincipalKind::Validator,
        &[P::ValidationReport],
    )
    .await;
    let other = auth(&mut f, &other_token, P::ValidationReport).await;
    assert!(matches!(
        f.db.release_native_check_workspace(&other, &cmd("other-release"), run)
            .await,
        Err(Error::Forbidden)
    ));
    let release =
        f.db.release_native_check_workspace(&validator, &cmd("release"), run)
            .await
            .unwrap();
    assert_eq!(release["state"], "draining");
    assert_eq!(release["workspace"], first["workspace"]);
    assert_eq!(
        release,
        f.db.release_native_check_workspace(&validator, &cmd("release"), run)
            .await
            .unwrap()
    );
    assert_eq!(
        release,
        f.db.release_native_check_workspace(&validator, &cmd("release-again"), run)
            .await
            .unwrap()
    );
    assert_eq!(
        raw.query_one("SELECT to_jsonb(e) FROM th_check_execution_evidence e", &[])
            .await
            .unwrap()
            .get::<_, serde_json::Value>(0),
        evidence
    );
    assert_eq!(
        f.db.collect_native_check(&validator, &cmd("collect-after-release"), run)
            .await
            .unwrap(),
        collected
    );
    let changed = Command {
        key: "release-stale".into(),
        epoch: 2,
    };
    assert!(
        f.db.release_native_check_workspace(&validator, &changed, run)
            .await
            .is_err()
    );
    assert!(
        f.db.land_native_candidate(&worker, &cmd("land-lost"), cid)
            .await
            .is_err()
    );
    assert!(
        raw.execute(
            "UPDATE th_check_executions SET execution_run=$1",
            &[&Uuid::new_v4()]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute(
            "UPDATE th_check_execution_evidence SET definition='false'::bytea",
            &[]
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL; cancellation before any runtime"]
async fn stale_or_revoked_check_cannot_start_with_rotated_credentials() {
    for revoke in [false, true] {
        let mut f = Fixture::new().await;
        let a = controls(&mut f).await;
        program(&mut f, &a).await;
        let entry = proposal(&mut f, &a, "fenced", b"test\n").await;
        let (cid, worker) = start(&mut f, &a, entry).await;
        prepare(&mut f, &worker, cid).await;
        let (run, validator) = assigned(&mut f, &a, cid).await;
        f.db.prepare_native_check(&validator, &cmd("prepare"), run)
            .await
            .unwrap();
        if revoke {
            f.db.revoke_repository_token(&f.scope, a.validator.id, "operator")
                .await
                .unwrap();
        } else {
            assigned(&mut f, &a, cid).await;
        }
        let rotated =
            f.db.issue_repository_token(
                &f.scope,
                "ci",
                &[P::Read, P::ValidationReport],
                3600,
                "operator",
            )
            .await
            .unwrap();
        let auth = auth(&mut f, &rotated, P::ValidationReport).await;
        assert!(
            f.db.prepare_native_check(&auth, &cmd("retry"), run)
                .await
                .is_err()
        );
        assert!(
            f.db.start_native_check(&auth, &cmd("start"), run)
                .await
                .is_err()
        );
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; populated schema-10 upgrade"]
async fn check_execution_migration_preserves_native_candidate_policy_and_pending_checks() {
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
        include_str!("../../../migrations/001_transactions.sql"),
        include_str!("../../../migrations/002_object_catalog.sql"),
        include_str!("../../../migrations/003_change_intake.sql"),
        include_str!("../../../migrations/004_access_control.sql"),
        include_str!("../../../migrations/005_source_profiles.sql"),
        include_str!("../../../migrations/006_source_uploads.sql"),
        include_str!("../../../migrations/007_workspace_preparations.sql"),
        include_str!("../../../migrations/008_managed_runs.sql"),
        include_str!("../../../migrations/009_change_uploads.sql"),
        include_str!("../../../migrations/010_native_integration.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(10,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let mut f = Fixture::from_db(schema, scope, db).await;
    let a = controls(&mut f).await;
    let entry = legacy_proposal(&mut f, &a, "before-upgrade", b"already accepted\n").await;
    let (cid, worker) = start(&mut f, &a, entry).await;
    prepare(&mut f, &worker, cid).await;
    assigned(&mut f, &a, cid).await;
    let query = "SELECT jsonb_build_object('candidates',(SELECT jsonb_agg(to_jsonb(c) ORDER BY candidate) FROM th_native_candidates c),'refs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY name) FROM th_native_refs r),'checks',(SELECT jsonb_agg(to_jsonb(c) ORDER BY run) FROM th_native_check_runs c),'pins',(SELECT jsonb_agg(to_jsonb(p) ORDER BY holder) FROM th_object_pins p),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o))";
    let before: Value = raw.query_one(query, &[]).await.unwrap().get(0);
    assert!(matches!(
        f.db.check_schema().await,
        Err(Error::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    assert_eq!(
        before,
        raw.query_one(query, &[]).await.unwrap().get::<_, Value>(0)
    );
    evidence(&mut f, &a, &worker, cid).await;
    assert_eq!(
        f.db.land_native_candidate(&worker, &cmd("after-upgrade"), cid)
            .await
            .unwrap()["state"],
        "landed"
    );
    cleanup(&f.schema).await;
}
