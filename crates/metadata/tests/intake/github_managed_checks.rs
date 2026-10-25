use super::*;
fn cmd(key: &str) -> treeharbor_metadata::github::Command {
    super::cmd(&format!("managed-{key}"))
}
use treeharbor_metadata::{
    github::{ImportSpec, ManagedCheckSpec, WireCommand},
    integration::CheckProgram,
    preparations::{PreparedFiles, StorageSlot},
    runs::RuntimeVolume,
};
use treeharbor_runtime::{Definition, Profile};

async fn setup_check(
    f: &mut QueueFixture,
) -> (Authorization, Authorization, Uuid, ManagedCheckSpec) {
    f.request().await;
    let attempt = f.begin().await;
    let report = groups::group_report(f).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("group"), f.id, attempt, &report)
        .await
        .unwrap();
    let owner = actor(
        &mut f.p.f,
        "policy-owner",
        K::Human,
        &[P::PolicyWrite, P::Read],
    )
    .await;
    let validator = actor(
        &mut f.p.f,
        "ci",
        K::Validator,
        &[P::ValidationReport, P::Read],
    )
    .await;
    // Trusted converter fixture; neither a real GitHub response nor actual execution.
    let raw = Object::Blob(b"trusted converter".to_vec());
    let dir = Object::Directory(Directory::new(vec![]).unwrap());
    let snapshot = Object::Snapshot {
        root: dir.id().unwrap(),
    };
    let changes = Object::ChangeOperations(Changes::new(vec![]).unwrap());
    let commit = Object::NativeCommit(Commit {
        git_origin: Some(Box::new(GitOrigin {
            oid: GitOid::Sha1([0xaa; 20]),
            raw: raw.id().unwrap(),
        })),
        parents: vec![],
        snapshot: snapshot.id().unwrap(),
        changes: changes.id().unwrap(),
        author_name: "test".into(),
        author_email: "test@example.invalid".into(),
        timestamp_seconds: 0,
        timezone_minutes: 0,
        message: "fixture".into(),
    });
    let import = Uuid::new_v4();
    f.p.f
        .db
        .request_github_import(
            &f.p.owner,
            &cmd("import"),
            import,
            f.p.binding,
            &ImportSpec {
                git_revision: format!("git:sha1:{}", "aa".repeat(20)),
                max_objects: 100,
                max_bytes: 1048576,
                ttl_seconds: 60,
            },
        )
        .await
        .unwrap();
    f.p.f
        .db
        .begin_github_import_upload(
            &f.p.reader,
            &cmd("import-begin"),
            import,
            "local-1",
            commit.id().unwrap(),
        )
        .await
        .unwrap();
    for obj in [raw, dir, snapshot, changes, commit] {
        let obj = PreparedObject::new(&obj).unwrap();
        let ticket =
            f.p.f
                .db
                .reserve_github_source_object(&f.p.reader, import, &f.p.f.depot, &obj)
                .await
                .unwrap();
        let receipt = f.p.f.depot.put(ticket.generation(), &obj).unwrap();
        f.p.f
            .db
            .publish_github_source_object(&f.p.reader, &ticket, &receipt)
            .await
            .unwrap();
    }
    f.p.f
        .db
        .finish_github_import(&f.p.reader, &cmd("import-finish"), import)
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
    f.p.f
        .db
        .register_runtime_profile(&f.p.f.scope, &profile, "operator")
        .await
        .unwrap();
    let spec = ManagedCheckSpec {
        queue: f.id,
        membership_sequence: 1,
        import,
        validator: "ci".into(),
        program: CheckProgram {
            version: 1,
            runtime_profile: ObjectId::from_bytes(profile.digest()).to_string(),
            argv: vec!["/check".into()],
        },
    };
    (owner, validator, Uuid::new_v4(), spec)
}
async fn read(f: &mut QueueFixture) -> Authorization {
    let t =
        f.p.f
            .db
            .issue_repository_token(&f.p.f.scope, "policy-owner", &[P::Read], 3600, "operator")
            .await
            .unwrap();
    f.p.f
        .db
        .authorize(&t.secret, &f.p.f.scope, P::Read)
        .await
        .unwrap()
}
async fn execution_plan(
    f: &mut QueueFixture,
    validator: &Authorization,
    id: Uuid,
) -> (Authorization, treeharbor_metadata::runs::RunPlan) {
    let node = actor(&mut f.p.f, "check-node", K::Node, &[P::RuntimeControl]).await;
    let storage = Uuid::new_v4();
    let slot = StorageSlot {
        storage,
        slot: Uuid::new_v4(),
    };
    f.p.f
        .db
        .register_node_storage(f.p.f.scope.tenant(), "check-node", storage, "operator")
        .await
        .unwrap();
    f.p.f
        .db
        .register_runtime_volume(
            f.p.f.scope.tenant(),
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
        f.p.f
            .db
            .claim_preparation(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let prep =
        f.p.f
            .db
            .reserve_preparation(&node, &job, slot)
            .await
            .unwrap();
    assert_eq!(prep.authority(), Authority::Connected);
    // Filesystem receipt fixture only. No fabricated process success.
    f.p.f
        .db
        .complete_preparation(
            &node,
            &prep,
            &PreparedFiles {
                snapshot: prep.snapshot(),
                profile: SourceProfile::full().id(),
                manifest_digest: [91; 32],
            },
        )
        .await
        .unwrap();
    let started =
        f.p.f
            .db
            .start_github_managed_check(validator, &cmd("start"), id)
            .await
            .unwrap();
    assert_eq!(
        started,
        f.p.f
            .db
            .start_github_managed_check(validator, &cmd("start"), id)
            .await
            .unwrap()
    );
    let plan =
        f.p.f
            .db
            .claim_run(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    (node, plan)
}
#[tokio::test]
#[ignore = "requires PostgreSQL; source/authority binding, immutable approval, runtime fencing and no invented success"]
async fn connected_managed_checks_bind_import_run_and_recover_without_native_authority() {
    let mut f = QueueFixture::new().await;
    let (owner, validator, id, spec) = setup_check(&mut f).await;
    assert!(
        f.p.f
            .db
            .approve_github_managed_check(&f.bridge, &cmd("bad-author"), id, &spec)
            .await
            .is_err()
    );
    for bad in [
        ManagedCheckSpec {
            import: Uuid::new_v4(),
            ..spec.clone()
        },
        ManagedCheckSpec {
            membership_sequence: 2,
            ..spec.clone()
        },
    ] {
        assert!(
            f.p.f
                .db
                .approve_github_managed_check(
                    &owner,
                    &cmd(&format!("bad-source-{}", Uuid::new_v4())),
                    Uuid::new_v4(),
                    &bad
                )
                .await
                .is_err()
        );
    }
    let approved =
        f.p.f
            .db
            .approve_github_managed_check(&owner, &cmd("approve"), id, &spec)
            .await
            .unwrap();
    assert_eq!(
        approved,
        f.p.f
            .db
            .approve_github_managed_check(&owner, &cmd("approve"), id, &spec)
            .await
            .unwrap()
    );
    let mut changed = spec.clone();
    changed.program.argv.push("different".into());
    assert!(matches!(
        f.p.f
            .db
            .approve_github_managed_check(&owner, &cmd("approve"), id, &changed)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    assert!(
        f.p.f
            .db
            .prepare_github_managed_check(&owner, &cmd("wrong-owner"), id)
            .await
            .is_err()
    );
    let first =
        f.p.f
            .db
            .prepare_github_managed_check(&validator, &cmd("prepare"), id)
            .await
            .unwrap();
    assert_eq!(
        first,
        f.p.f
            .db
            .prepare_github_managed_check(&validator, &cmd("prepare"), id)
            .await
            .unwrap()
    );
    assert!(
        f.p.f
            .db
            .start_github_managed_check(&validator, &cmd("start"), id)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect-early"), id)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .release_github_managed_check(&validator, &cmd("release-early"), id)
            .await
            .is_err()
    );
    let (node, plan) = execution_plan(&mut f, &validator, id).await;
    assert!(plan.is_check());
    assert!(!plan.stop_requested());
    assert_eq!(plan.prepared().authority, Authority::Connected);
    assert_eq!(plan.task().argv(), spec.program.argv);
    f.p.f.db.admit_run_start(&node, &plan).await.unwrap();
    assert!(
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("early"), id)
            .await
            .is_err()
    );
    assert_eq!(
        f.p.f.db.finish_run(&node, &plan).await.unwrap()["state"],
        "lost"
    );
    let collected =
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap();
    assert_eq!(collected["outcome"], "error");
    assert_eq!(collected["execution_context_verified"], false);
    assert_eq!(collected["admission_verified"], false);
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    assert_eq!(
        collected,
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap()
    );
    assert_eq!(
        collected,
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect-again"), id)
            .await
            .unwrap()
    );
    let raw = connection(&f.p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_native_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT authority FROM th_repositories", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "connected"
    );
    for sql in [
        "DELETE FROM th_github_managed_checks",
        "UPDATE th_github_managed_checks SET epoch=2",
        "UPDATE th_github_managed_executions SET state='preparing'",
        "UPDATE th_github_managed_executions SET outcome='passed'",
    ] {
        assert!(raw.execute(sql, &[]).await.is_err());
    }
    let definition: Vec<u8> = raw
        .query_one("SELECT definition FROM th_github_managed_executions", &[])
        .await
        .unwrap()
        .get(0);
    let facts: Value = serde_json::from_slice(&definition).unwrap();
    assert_eq!(facts["subject"], approved["subject"]);
    assert_eq!(facts["runtime"]["state"], "lost");
    let reader = read(&mut f).await;
    assert_eq!(
        collected,
        f.p.f
            .db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap()
    );
    assert_eq!(
        f.p.f
            .db
            .release_github_managed_check(&validator, &cmd("release"), id)
            .await
            .unwrap()["state"],
        "draining"
    );
    assert_eq!(
        definition,
        raw.query_one("SELECT definition FROM th_github_managed_executions", &[])
            .await
            .unwrap()
            .get::<_, Vec<u8>>(0)
    );
    f.p.f
        .db
        .revoke_github_managed_check(&owner, &cmd("revoke"), id)
        .await
        .unwrap();
    assert_eq!(
        f.p.f
            .db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap()["current_locally"],
        false
    );
    assert!(
        f.p.f
            .db
            .prepare_github_managed_check(&validator, &cmd("new-prepare"), id)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
#[test]
fn managed_check_wire_never_accepts_an_agent_outcome_or_substituted_program_on_start() {
    let id = Uuid::new_v4();
    let valid = json!({"action":"managed_check_collect","epoch":1,"key":"collect","check_run":id});
    assert_eq!(
        serde_json::from_value::<WireCommand>(valid.clone())
            .unwrap()
            .permission(),
        P::ValidationReport
    );
    for (field, value) in [
        ("outcome", json!("passed")),
        ("program", json!({})),
        ("snapshot", json!("other")),
    ] {
        let mut bad = valid.clone();
        bad[field] = value;
        assert!(serde_json::from_value::<WireCommand>(bad).is_err());
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; trusted terminal observation fixture only, NOT real sandbox evidence"]
async fn connected_check_receipt_is_exact_and_stale_success_is_not_current() {
    let mut f = QueueFixture::new().await;
    let (owner, validator, id, spec) = setup_check(&mut f).await;
    f.p.f
        .db
        .approve_github_managed_check(&owner, &cmd("approve"), id, &spec)
        .await
        .unwrap();
    f.p.f
        .db
        .prepare_github_managed_check(&validator, &cmd("prepare"), id)
        .await
        .unwrap();
    let (node, plan) = execution_plan(&mut f, &validator, id).await;
    f.p.f.db.admit_run_start(&node, &plan).await.unwrap();
    let raw = connection(&f.p.f.schema).await;
    // Inject exactly the trusted terminal rows that observe_run persists. This
    // tests the collector predicate, not Docker/gVisor or GitHub authenticity.
    raw.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=0,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64)]).await.unwrap();
    f.p.f.db.finish_run(&node, &plan).await.unwrap();
    let result =
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap();
    assert_eq!(result["outcome"], "passed");
    assert_eq!(result["execution_context_verified"], true);
    assert_eq!(result["admission_verified"], false);
    assert_eq!(result["merge_verified"], false);
    let reader = read(&mut f).await;
    assert_eq!(
        f.p.f
            .db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap(),
        result
    );
    f.p.f
        .db
        .disable_principal(f.p.f.scope.tenant(), "check-node", "operator")
        .await
        .unwrap();
    let after =
        f.p.f
            .db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap();
    assert_eq!(after["execution_context_verified"], false);
    assert_eq!(after["current_locally"], false);
    assert_eq!(after["outcome"], "passed");
    assert_eq!(after["evidence"], result["evidence"]);
    // Historical same-key receipt remains historical; it is not fresh admission.
    assert_eq!(
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap(),
        result
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; runtime start/reconcile stops after authority, approval and source changes"]
async fn connected_check_revocation_fences_runtime_and_prestart_cleanup() {
    for reason in [
        "approval",
        "owner-token",
        "validator-token",
        "profile",
        "binding",
        "queue",
        "source",
        "epoch",
    ] {
        let mut f = QueueFixture::new().await;
        let (owner, validator, id, spec) = setup_check(&mut f).await;
        f.p.f
            .db
            .approve_github_managed_check(&owner, &cmd("approve"), id, &spec)
            .await
            .unwrap();
        f.p.f
            .db
            .prepare_github_managed_check(&validator, &cmd("prepare"), id)
            .await
            .unwrap();
        let (node, plan) = execution_plan(&mut f, &validator, id).await;
        let raw = connection(&f.p.f.schema).await;
        match reason {
            "approval" => {
                f.p.f
                    .db
                    .revoke_github_managed_check(&owner, &cmd("revoke"), id)
                    .await
                    .unwrap();
            }
            "owner-token" => {
                f.p.f
                    .db
                    .revoke_repository_token(
                        &f.p.f.scope,
                        raw.query_one(
                            "SELECT token FROM th_github_managed_checks WHERE check_run=$1",
                            &[&id],
                        )
                        .await
                        .unwrap()
                        .get(0),
                        "operator",
                    )
                    .await
                    .unwrap();
            }
            "validator-token" => {
                f.p.f
                    .db
                    .revoke_repository_token(
                        &f.p.f.scope,
                        raw.query_one(
                            "SELECT token FROM th_github_managed_executions WHERE check_run=$1",
                            &[&id],
                        )
                        .await
                        .unwrap()
                        .get(0),
                        "operator",
                    )
                    .await
                    .unwrap();
            }
            "profile" => {
                f.p.f
                    .db
                    .revoke_runtime_profile(
                        &f.p.f.scope,
                        *spec
                            .program
                            .runtime_profile
                            .parse::<ObjectId>()
                            .unwrap()
                            .as_bytes(),
                        "operator",
                    )
                    .await
                    .unwrap();
            }
            "binding" => {
                f.p.f
                    .db
                    .revoke_github(&f.p.owner, &cmd("binding-revoke"), f.p.binding)
                    .await
                    .unwrap();
            }
            "queue" => {
                f.p.f
                    .db
                    .cancel_github_queue(&f.owner, &cmd("queue-cancel"), f.id)
                    .await
                    .unwrap();
            }
            "source" => {
                f.p.new_version().await;
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            f.p.f.db.admit_run_start(&node, &plan).await.is_err(),
            "{reason}"
        );
        assert_eq!(
            f.p.f.db.finish_run(&node, &plan).await.unwrap()["state"],
            "cancelled",
            "{reason}"
        );
        cleanup(&f.p.f.schema).await;
    }
    let mut f = QueueFixture::new().await;
    let (owner, validator, id, spec) = setup_check(&mut f).await;
    f.p.f
        .db
        .approve_github_managed_check(&owner, &cmd("approve"), id, &spec)
        .await
        .unwrap();
    f.p.f
        .db
        .prepare_github_managed_check(&validator, &cmd("prepare"), id)
        .await
        .unwrap();
    f.p.f
        .db
        .revoke_github_managed_check(&owner, &cmd("revoke"), id)
        .await
        .unwrap();
    assert_eq!(
        f.p.f
            .db
            .release_github_managed_check(&validator, &cmd("release"), id)
            .await
            .unwrap()["state"],
        "draining"
    );
    cleanup(&f.p.f.schema).await;
}
