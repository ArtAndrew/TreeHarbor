use super::*;
use treeharbor_github_contract::workflow_definitions as wd;
use treeharbor_metadata::github::PolicyPin;
fn policy() -> wd::Policy {
    serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/workflow-policy-v1.json"
    ))
    .unwrap()
}
async fn owner(f: &mut QueueFixture) -> Authorization {
    actor(
        &mut f.p.f,
        "policy-owner",
        K::Human,
        &[P::PolicyWrite, P::Read],
    )
    .await
}
async fn reader(f: &mut QueueFixture) -> Authorization {
    let token =
        f.p.f
            .db
            .issue_repository_token(&f.p.f.scope, "policy-owner", &[P::Read], 3600, "operator")
            .await
            .unwrap();
    f.p.f
        .db
        .authorize(&token.secret, &f.p.f.scope, P::Read)
        .await
        .unwrap()
}
async fn set(
    f: &mut QueueFixture,
    owner: &Authorization,
    generation: i64,
    p: Option<&wd::Policy>,
) -> Value {
    f.p.f
        .db
        .set_github_check_policy(
            owner,
            &cmd(&format!("policy-{generation}")),
            f.p.binding,
            "main",
            generation,
            p,
        )
        .await
        .unwrap()
}
async fn pinned(f: &mut QueueFixture) -> Authorization {
    let owner = owner(f).await;
    let p = policy();
    let result = set(f, &owner, 0, Some(&p)).await;
    f.spec.policy = Some(PolicyPin {
        digest: result["policy"].as_str().unwrap().into(),
        generation: 1,
    });
    owner
}
#[tokio::test]
#[ignore = "requires PostgreSQL; policy CAS, authority, exact pin, ABA and immutable history"]
async fn workflow_policy_fences_new_dispatch_after_replacement_and_retains_original_receipts() {
    let mut f = QueueFixture::new().await;
    let p = policy();
    assert!(
        f.p.f
            .db
            .set_github_check_policy(
                &f.bridge,
                &cmd("bridge-policy"),
                f.p.binding,
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .set_github_check_policy(
                &f.owner,
                &cmd("owner-no-permission"),
                f.p.binding,
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    let owner = pinned(&mut f).await;
    let read = reader(&mut f).await;
    let created = f.request().await;
    let attempt = f.begin().await;
    assert!(
        f.p.f
            .db
            .github_queue_context(&f.bridge, 1, f.id, true, Some(attempt))
            .await
            .is_ok()
    );
    assert!(
        f.p.f
            .db
            .set_github_check_policy(&owner, &cmd("stale-cas"), f.p.binding, "main", 0, Some(&p))
            .await
            .is_err()
    );
    let mut other = p.clone();
    other.checks[0].expected_context.toolchain = format!("sha256:{}", "ab".repeat(32));
    set(&mut f, &owner, 1, Some(&other)).await;
    set(&mut f, &owner, 2, Some(&p)).await;
    assert!(
        f.p.f
            .db
            .github_queue_context(&f.bridge, 1, f.id, true, Some(attempt))
            .await
            .is_err()
    );
    let status =
        f.p.f
            .db
            .github_queue_context(&read, 1, f.id, false, None)
            .await
            .unwrap();
    assert_eq!(status["workflow_policy"]["pin"]["generation"], 1);
    assert_eq!(status["workflow_policy"]["current"], false);
    assert_eq!(created, f.request().await); // replay cannot grant a new dispatch
    let old =
        f.p.f
            .db
            .set_github_check_policy(&owner, &cmd("policy-0"), f.p.binding, "main", 0, Some(&p))
            .await
            .unwrap();
    assert_eq!(old["generation"], 1);
    let current =
        f.p.f
            .db
            .github_check_policy(&read, 1, f.p.binding, "main")
            .await
            .unwrap();
    assert_eq!(current["generation"], 3);
    assert_eq!(current["definition"], json!(p));
    set(&mut f, &owner, 3, None).await;
    let current =
        f.p.f
            .db
            .github_check_policy(&read, 1, f.p.binding, "main")
            .await
            .unwrap();
    assert!(current["policy"].is_null());
    assert_eq!(current["generation"], 4);
    let raw = connection(&f.p.f.schema).await;
    for sql in [
        "UPDATE th_github_check_policies SET principal='forged'",
        "DELETE FROM th_github_check_policies",
        "UPDATE th_github_queue_policies SET generation=4",
        "DELETE FROM th_github_policy_refs",
        "UPDATE th_github_policy_refs SET generation=generation+2",
    ] {
        assert!(raw.batch_execute(sql).await.is_err(), "{sql}");
    }
    f.p.f
        .db
        .set_repository_access(&f.p.f.scope, "policy-owner", &[], "operator")
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .set_github_check_policy(&owner, &cmd("policy-0"), f.p.binding, "main", 0, Some(&p))
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; stale, mismatching and foreign pins roll back queue creation"]
async fn workflow_policy_pinning_checks_generation_binding_epoch_and_required_checks() {
    let mut f = QueueFixture::new().await;
    let owner = owner(&mut f).await;
    let mut p = policy();
    p.checks[0].required.name = "other".into();
    set(&mut f, &owner, 0, Some(&p)).await;
    f.spec.policy = Some(PolicyPin {
        digest: p.digest().unwrap().to_string(),
        generation: 1,
    });
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("mismatch"), f.id, &f.spec)
            .await
            .is_err()
    );
    let p = policy();
    set(&mut f, &owner, 1, Some(&p)).await;
    f.spec.policy = Some(PolicyPin {
        digest: p.digest().unwrap().to_string(),
        generation: 1,
    });
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("stale-pin"), f.id, &f.spec)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .set_github_check_policy(
                &owner,
                &cmd("foreign-binding"),
                Uuid::new_v4(),
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .set_github_check_policy(
                &owner,
                &Command {
                    key: "foreign-epoch".into(),
                    epoch: 2
                },
                f.p.binding,
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    let raw = connection(&f.p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_queue", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    f.spec.policy.as_mut().unwrap().generation = 2;
    f.request().await;
    f.p.f
        .db
        .revoke_github(&f.p.owner, &cmd("revoke-binding"), f.p.binding)
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .set_github_check_policy(
                &owner,
                &cmd("inactive-binding"),
                f.p.binding,
                "main",
                2,
                Some(&p)
            )
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; full retained attempt/check/membership chain and policy-independent history"]
async fn workflow_definition_binds_source_chain_and_never_promotes_file_approval_to_context() {
    let mut f = QueueFixture::new().await;
    let owner = pinned(&mut f).await;
    let read = reader(&mut f).await;
    f.request().await;
    let attempt = f.begin().await;
    let m = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("membership"), f.id, attempt, &m)
        .await
        .unwrap();
    let c = group_checks::report(&f, &m).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("checks"), f.id, attempt, &c)
        .await
        .unwrap();
    let a = group_attempts::report(&f, &c).await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("attempt"), f.id, attempt, &a)
        .await
        .unwrap();
    let selected =
        f.p.f
            .db
            .github_queue_definition_context(&f.bridge, 1, f.id, 3)
            .await
            .unwrap();
    assert!(
        f.p.f
            .db
            .github_queue_definition_context(&f.owner, 1, f.id, 3)
            .await
            .is_err()
    );
    let mut report: QueueObservation = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/workflow-definition-v7.json"
    ))
    .unwrap();
    report.started_at_unix_ms = f.report().await.started_at_unix_ms;
    report.finished_at_unix_ms = report.started_at_unix_ms;
    report.workflow_definition.as_mut().unwrap().target =
        serde_json::from_value(selected["workflow_definition_target"].clone()).unwrap();
    for case in [
        "wrong-source",
        "missing",
        "changed-check",
        "changed-attempt",
    ] {
        let mut bad = report.clone();
        let d = bad.workflow_definition.as_mut().unwrap();
        match case {
            "wrong-source" => d.target.attempt_sequence = 2,
            "missing" => d.target.attempt_sequence = 4,
            "changed-check" => d.target.check.check.conclusion = Some("FAILURE".into()),
            "changed-attempt" => d.target.selected.conclusion = Some("FAILURE".into()),
            _ => unreachable!(),
        }
        bad.validate(&f.target).unwrap();
        assert!(
            f.p.f
                .db
                .report_github_queue(&f.bridge, &cmd(case), f.id, attempt, &bad)
                .await
                .is_err(),
            "{case}"
        );
    }
    let key = cmd("definition");
    let receipt =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &report)
            .await
            .unwrap();
    assert_eq!(receipt["report_sequence"], 4);
    assert_eq!(receipt["observation"], json!(m));
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    assert_eq!(
        receipt,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &report)
            .await
            .unwrap()
    );
    let page =
        f.p.f
            .db
            .github_queue_definitions(&read, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(
        page["captures"][0]["workflow_file_matches_pinned_policy"],
        true
    );
    assert_eq!(page["workflow_policy"]["current"], true);
    for field in ["definition_verified", "execution_context_verified"] {
        assert_eq!(page["captures"][0][field], false);
    }
    assert_eq!(page["merge_verified"], false);
    assert_eq!(page["host_policy_verified"], false);
    set(&mut f, &owner, 1, None).await;
    let mut changed = report.clone();
    let d = changed.workflow_definition.as_mut().unwrap();
    d.file.content.push('\n');
    d.file.byte_size = d.file.content.len();
    d.file.blob = wd::blob_oid(&d.file.content).unwrap();
    d.file.content_sha256 = wd::content_digest(&d.file.content).to_string();
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("changed-content"), f.id, attempt, &changed)
        .await
        .unwrap();
    let page =
        f.p.f
            .db
            .github_queue_definitions(&read, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(page["workflow_policy"]["current"], false);
    assert_eq!(
        page["captures"][0]["workflow_file_matches_pinned_policy"],
        true
    );
    assert_eq!(page["next_after"], 4);
    let page =
        f.p.f
            .db
            .github_queue_definitions(&read, 1, f.id, 4)
            .await
            .unwrap();
    assert_eq!(
        page["captures"][0]["workflow_file_matches_pinned_policy"],
        false
    );
    assert!(page["next_after"].is_null());
    assert_eq!(
        receipt,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &key, f.id, attempt, &report)
            .await
            .unwrap()
    );
    assert!(
        f.p.f
            .db
            .github_queue_definitions(&read, 2, f.id, 0)
            .await
            .is_err()
    );
    f.p.f
        .db
        .set_repository_access(&f.p.f.scope, "policy-owner", &[], "operator")
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .github_queue_definitions(&read, 1, f.id, 0)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; populated schema24 migration preserves receipts and never inherits a new approval"]
async fn workflow_policy_upgrade_does_not_retroactively_approve_old_queues() {
    use sha2::{Digest, Sha256};
    let mut f = QueueFixture::new().await;
    let before = f.request().await;
    let attempt = f.begin().await;
    let report = f.report().await;
    let receipt =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("old-report"), f.id, attempt, &report)
            .await
            .unwrap();
    let raw = connection(&f.p.f.schema).await;
    let grants:Vec<String>=raw.query_one("SELECT permissions FROM th_access_tokens WHERE principal='human' ORDER BY created_at DESC LIMIT 1",&[]).await.unwrap().get(0);
    crate::common::drop_unpopulated_v26(&raw).await;
    raw.batch_execute("DROP TABLE th_github_queue_policies,th_github_policy_refs,th_github_check_policies; DROP FUNCTION th_github_policy_ref_update();").await.unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for p in paths.iter().take(24) {
        let mut h = Sha256::new();
        h.update(digest);
        h.update(std::fs::read(p).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=24,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    assert!(f.p.f.db.check_schema().await.is_err());
    f.p.f.db.migrate().await.unwrap();
    f.p.f.db.migrate().await.unwrap();
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        29
    );
    assert_eq!(raw.query_one("SELECT permissions FROM th_access_tokens WHERE principal='human' ORDER BY created_at DESC LIMIT 1",&[]).await.unwrap().get::<_,Vec<String>>(0),grants);
    assert_eq!(before, f.request().await);
    assert_eq!(
        receipt,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("old-report"), f.id, attempt, &report)
            .await
            .unwrap()
    );
    let owner = owner(&mut f).await;
    let read = reader(&mut f).await;
    set(&mut f, &owner, 0, Some(&policy())).await;
    let status =
        f.p.f
            .db
            .github_queue_context(&read, 1, f.id, false, None)
            .await
            .unwrap();
    assert!(status["workflow_policy"].is_null());
    assert!(
        f.p.f
            .db
            .github_queue_context(&f.bridge, 1, f.id, true, Some(attempt))
            .await
            .is_ok()
    );
    cleanup(&f.p.f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; revoked credentials after completion writes roll back the entire policy mutation"]
async fn workflow_policy_reauthorizes_after_receipt_and_outbox_writes() {
    for table in ["th_operations", "th_outbox"] {
        let mut f = QueueFixture::new().await;
        let owner = owner(&mut f).await;
        let raw = connection(&f.p.f.schema).await;
        let kind = if table == "th_operations" {
            "github.policy.set.v1"
        } else {
            "github.policy.changed"
        };
        raw.batch_execute(&format!("CREATE SEQUENCE revoke_seen; CREATE FUNCTION revoke_policy_token() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('revoke_seen'); UPDATE th_access_tokens SET revoked_at=clock_timestamp() WHERE principal='policy-owner'; END IF; RETURN NEW; END $$; CREATE TRIGGER revoke_policy_token AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION revoke_policy_token();")).await.unwrap();
        assert!(
            f.p.f
                .db
                .set_github_check_policy(
                    &owner,
                    &cmd("revoked-policy"),
                    f.p.binding,
                    "main",
                    0,
                    Some(&policy())
                )
                .await
                .is_err()
        );
        assert!(
            raw.query_one("SELECT is_called FROM revoke_seen", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_check_policies)+(SELECT count(*) FROM th_github_policy_refs)+(SELECT count(*) FROM th_operations WHERE kind='github.policy.set.v1')",&[]).await.unwrap().get::<_,i64>(0),0);
        cleanup(&f.p.f.schema).await;
    }
}
