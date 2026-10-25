use super::*;
use sha2::{Digest, Sha256};
use treeharbor_metadata::github::ManagedPullCheckSpec;

async fn setup(p: &mut Projection) -> (Authorization, Authorization, Uuid, ManagedPullCheckSpec) {
    let pull = inspections::observed_pull(p).await;
    let git: String = connection(&p.f.schema)
        .await
        .query_one(
            "SELECT git_revision FROM th_github_pulls WHERE pull=$1",
            &[&pull],
        )
        .await
        .unwrap()
        .get(0);
    let (owner, validator, import, program) = setup_source(p, &git).await;
    (
        owner,
        validator,
        Uuid::new_v4(),
        ManagedPullCheckSpec {
            pull,
            import,
            validator: "ci".into(),
            program,
        },
    )
}

#[test]
fn pull_check_wire_requires_owner_policy_and_cannot_supply_an_outcome_or_queue() {
    let valid = json!({"action":"managed_check_approve_pull","key":"approve","epoch":1,"check_run":Uuid::new_v4(),"specification":{"pull":Uuid::new_v4(),"import":Uuid::new_v4(),"validator":"ci","program":{"version":1,"runtime_profile":format!("sha256:{}","11".repeat(32)),"argv":["/check"]}}});
    assert_eq!(
        serde_json::from_value::<WireCommand>(valid.clone())
            .unwrap()
            .permission(),
        P::PolicyWrite
    );
    for field in [
        "queue",
        "membership_sequence",
        "outcome",
        "snapshot",
        "head_revision",
    ] {
        let mut bad = valid.clone();
        bad["specification"][field] = json!("caller-controlled");
        assert!(
            serde_json::from_value::<WireCommand>(bad).is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; pre-queue source, trust, exact runtime evidence, replay and stale approval"]
async fn pull_check_runs_without_any_inspection_or_queue_and_cannot_green_a_group() {
    let mut p = Projection::new().await;
    let (owner, validator, id, spec) = setup(&mut p).await;
    let raw = connection(&p.f.schema).await;
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_inspections)+(SELECT count(*) FROM th_github_queue)",&[]).await.unwrap().get::<_,i64>(0),0);
    assert!(
        p.f.db
            .approve_github_managed_pull_check(&p.bridge, &cmd("bad-role"), id, &spec)
            .await
            .is_err()
    );
    // An equal tree imported from a different Git commit is still the wrong source.
    let wrong_import = import_source(&mut p, &format!("git:sha1:{}", "aa".repeat(20))).await;
    for bad in [
        ManagedPullCheckSpec {
            import: wrong_import,
            ..spec.clone()
        },
        ManagedPullCheckSpec {
            pull: Uuid::new_v4(),
            ..spec.clone()
        },
        ManagedPullCheckSpec {
            validator: "bridge".into(),
            ..spec.clone()
        },
    ] {
        assert!(
            p.f.db
                .approve_github_managed_pull_check(
                    &owner,
                    &cmd(&format!("bad-{}", Uuid::new_v4())),
                    Uuid::new_v4(),
                    &bad
                )
                .await
                .is_err()
        );
    }
    let request = json!({"action":"managed_check_approve_pull","key":"managed-approve","epoch":1,"check_run":id,"specification":spec});
    let approved = serde_json::from_value::<WireCommand>(request.clone())
        .unwrap()
        .execute(&mut p.f.db, &owner, None)
        .await
        .unwrap();
    assert_eq!(approved["version"], 2);
    assert_eq!(approved["pull"], spec.pull.to_string());
    assert!(approved.get("queue").is_none());
    assert_eq!(
        approved["subject"]["head_revision"],
        format!("git:sha1:{}", "11".repeat(20))
    );
    assert_eq!(
        approved,
        serde_json::from_value::<WireCommand>(request)
            .unwrap()
            .execute(&mut p.f.db, &owner, None)
            .await
            .unwrap()
    );
    let mut changed = spec.clone();
    changed.program.argv.push("changed".into());
    assert!(matches!(
        p.f.db
            .approve_github_managed_pull_check(&owner, &cmd("approve"), id, &changed)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    let prepared =
        p.f.db
            .prepare_github_managed_check(&validator, &cmd("prepare"), id)
            .await
            .unwrap();
    assert_eq!(
        prepared,
        p.f.db
            .prepare_github_managed_check(&validator, &cmd("prepare"), id)
            .await
            .unwrap()
    );
    assert!(
        p.f.db
            .collect_github_managed_check(&validator, &cmd("early"), id)
            .await
            .is_err()
    );
    let (node, plan) = execution_plan(&mut p, &validator, id).await;
    assert!(plan.is_check());
    assert_eq!(plan.prepared().authority, Authority::Connected);
    p.f.db.admit_run_start(&node, &plan).await.unwrap();
    // Trusted terminal-state fixture only, not a real sandbox or GitHub run.
    raw.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=0,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64)]).await.unwrap();
    p.f.db.finish_run(&node, &plan).await.unwrap();
    let collected =
        p.f.db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap();
    assert_eq!(collected["outcome"], "passed");
    assert_eq!(collected["execution_context_verified"], true);
    for field in [
        "admission_verified",
        "host_policy_verified",
        "merge_verified",
    ] {
        assert_eq!(collected[field], false);
    }
    let definition: Vec<u8> = raw
        .query_one(
            "SELECT definition FROM th_github_managed_executions WHERE check_run=$1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    let value: Value = serde_json::from_slice(&definition).unwrap();
    assert_eq!(value["version"], 2);
    assert_eq!(value["pull"], spec.pull.to_string());
    assert!(value.get("queue").is_none());
    assert_eq!(value["subject"], approved["subject"]);
    let digest = Sha256::digest(
        [
            b"treeharbor-github-check-execution\0v2\0".as_slice(),
            definition.as_slice(),
        ]
        .concat(),
    );
    assert_eq!(
        collected["evidence"],
        ObjectId::from_bytes(digest.into()).to_string()
    );
    p.f.db = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let reader = read(&mut p).await;
    assert_eq!(
        collected,
        p.f.db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap()
    );
    // New source makes old success ineligible while its exact evidence survives.
    p.new_version().await;
    let stale =
        p.f.db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap();
    assert_eq!(stale["current_locally"], false);
    assert_eq!(stale["execution_context_verified"], false);
    assert_eq!(stale["outcome"], "passed");
    assert_eq!(stale["evidence"], collected["evidence"]);
    assert_eq!(
        collected,
        p.f.db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap()
    );
    assert!(
        p.f.db
            .prepare_github_managed_check(&validator, &cmd("reprepare"), id)
            .await
            .is_err()
    );
    assert_eq!(
        p.f.db
            .release_github_managed_check(&validator, &cmd("release"), id)
            .await
            .unwrap()["state"],
        "draining"
    );
    assert_eq!(
        raw.query_one(
            "SELECT (SELECT count(*) FROM th_native_refs)+(SELECT count(*) FROM th_github_queue)",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    for sql in [
        "UPDATE th_github_managed_checks SET pull=NULL",
        "DELETE FROM th_github_managed_checks",
        "UPDATE th_github_managed_executions SET outcome='failed'",
    ] {
        assert!(raw.execute(sql, &[]).await.is_err());
    }
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; PR source and trust fence before actual runtime start"]
async fn pull_check_revocation_and_new_source_fence_execution() {
    for reason in [
        "approval",
        "binding",
        "source",
        "owner-token",
        "validator-token",
        "profile",
        "epoch",
    ] {
        let mut p = Projection::new().await;
        let (owner, validator, id, spec) = setup(&mut p).await;
        p.f.db
            .approve_github_managed_pull_check(&owner, &cmd("approve"), id, &spec)
            .await
            .unwrap();
        p.f.db
            .prepare_github_managed_check(&validator, &cmd("prepare"), id)
            .await
            .unwrap();
        let (node, plan) = execution_plan(&mut p, &validator, id).await;
        let raw = connection(&p.f.schema).await;
        match reason {
            "approval" => {
                p.f.db
                    .revoke_github_managed_check(&owner, &cmd("revoke"), id)
                    .await
                    .unwrap();
            }
            "binding" => {
                p.f.db
                    .revoke_github(&p.owner, &cmd("revoke-binding"), p.binding)
                    .await
                    .unwrap();
            }
            "source" => p.new_version().await,
            "owner-token" | "validator-token" => {
                let sql = if reason == "owner-token" {
                    "SELECT token FROM th_github_managed_checks WHERE check_run=$1"
                } else {
                    "SELECT token FROM th_github_managed_executions WHERE check_run=$1"
                };
                let token = raw.query_one(sql, &[&id]).await.unwrap().get(0);
                p.f.db
                    .revoke_repository_token(&p.f.scope, token, "operator")
                    .await
                    .unwrap();
            }
            "profile" => {
                p.f.db
                    .revoke_runtime_profile(
                        &p.f.scope,
                        *spec
                            .program
                            .runtime_profile
                            .parse::<ObjectId>()
                            .unwrap()
                            .as_bytes(),
                        "operator",
                    )
                    .await
                    .unwrap()
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            p.f.db.admit_run_start(&node, &plan).await.is_err(),
            "{reason}"
        );
        assert_eq!(
            p.f.db.finish_run(&node, &plan).await.unwrap()["state"],
            "cancelled",
            "{reason}"
        );
        cleanup(&p.f.schema).await;
    }
    let mut p = Projection::new().await;
    let (owner, validator, id, spec) = setup(&mut p).await;
    p.f.db
        .approve_github_managed_pull_check(&owner, &cmd("approve"), id, &spec)
        .await
        .unwrap();
    p.f.db
        .prepare_github_managed_check(&validator, &cmd("prepare"), id)
        .await
        .unwrap();
    p.new_version().await;
    assert_eq!(
        p.f.db
            .release_github_managed_check(&validator, &cmd("release"), id)
            .await
            .unwrap()["state"],
        "draining"
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated v26 check/evidence/receipts survive additive PR-source migration"]
async fn pull_check_upgrade_preserves_collected_group_evidence_bytes() {
    let mut f = QueueFixture::new().await;
    let (owner, validator, id, spec) = setup_check(&mut f).await;
    let raw = connection(&f.p.f.schema).await;
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(26) {
        digest = Sha256::digest([digest, std::fs::read(path).unwrap()].concat()).to_vec();
    }
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
    let (node, plan) = execution_plan(&mut f.p, &validator, id).await;
    f.p.f.db.admit_run_start(&node, &plan).await.unwrap();
    f.p.f.db.finish_run(&node, &plan).await.unwrap();
    let before =
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap();
    let bytes: Vec<u8> = raw
        .query_one(
            "SELECT definition FROM th_github_managed_executions WHERE check_run=$1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        serde_json::from_slice::<Value>(&bytes).unwrap()["version"],
        1
    );
    // Reconstruct schema26 from populated compatible v1 records; this is a
    // migration fixture, not an execution by the preceding binary. Remove only
    // the new, unused source column/constraint. Collected v1 rows,
    // their program, runtime and historical receipt are fully populated.
    raw.batch_execute("ALTER TABLE th_github_managed_checks DROP CONSTRAINT th_github_managed_check_source, DROP CONSTRAINT th_github_managed_check_pull, DROP COLUMN pull, ALTER COLUMN queue SET NOT NULL, ALTER COLUMN membership_sequence SET NOT NULL").await.unwrap();
    raw.execute(
        "UPDATE th_schema_version SET version=26,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    f.p.f.db.migrate().await.unwrap();
    f.p.f.db.migrate().await.unwrap();
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        27
    );
    assert_eq!(
        bytes,
        raw.query_one(
            "SELECT definition FROM th_github_managed_executions WHERE check_run=$1",
            &[&id]
        )
        .await
        .unwrap()
        .get::<_, Vec<u8>>(0)
    );
    let reader = read(&mut f.p).await;
    assert_eq!(
        before,
        f.p.f
            .db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap()
    );
    assert_eq!(
        before,
        f.p.f
            .db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap()
    );
    cleanup(&f.p.f.schema).await;
}
