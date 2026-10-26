//! Real PostgreSQL dispatch boundaries. Host and runtime inputs are fixtures.
use super::*;
use sha2::{Digest, Sha256};
use treeharbor_github_contract::check_dispatch::PolicyProof;

async fn intent(c: &mut Candidate, label: &str) -> (Uuid, Uuid, cp::Target, cp::Preflight) {
    let check = c.checked(0, 0).await;
    let publication = Uuid::new_v4();
    let v =
        c.f.p
            .f
            .db
            .request_github_check_publication(
                &c.publisher,
                &cmd(&format!("{label}-request")),
                publication,
                &CheckPublicationSpec {
                    check_run: check,
                    ttl_seconds: 900,
                },
            )
            .await
            .unwrap();
    let target: cp::Target = serde_json::from_value(v["target"].clone()).unwrap();
    let mut p = preflight(&c.f.p, &target).await;
    let required = c
        .policy
        .required(7, 1, c.f.p.binding, "main", 1)
        .unwrap()
        .into_iter()
        .collect::<Vec<_>>();
    p.policy_before.protection.app_bound_checks = required.clone();
    p.policy_after.protection.app_bound_checks = required;
    let raw = c
        .policy
        .host_configuration
        .as_ref()
        .unwrap()
        .profile
        .protection
        .value
        .as_ref()
        .unwrap();
    let hash = treeharbor_core::ObjectId::from_bytes(
        Sha256::digest(serde_json::to_vec(raw).unwrap()).into(),
    )
    .to_string();
    p.policy_before.protection.sha256 = hash.clone();
    p.policy_after.protection.sha256 = hash;
    let attempt = Uuid::new_v4();
    c.f.p
        .f
        .db
        .begin_github_check_publication(
            &c.bridge,
            &cmd(&format!("{label}-begin")),
            publication,
            attempt,
        )
        .await
        .unwrap();
    (publication, attempt, target, p)
}

#[tokio::test]
#[ignore = "requires PostgreSQL; immutable proof is atomic with the grant, not a complete-checkset or merge certificate"]
async fn dispatch_policy_proof_precedes_grant_and_replay_or_late_receipt_never_reauthorizes() {
    // A second required program has no result yet. Publishing the first must not
    // depend on its own future GitHub receipt or on a peer's future success.
    let mut c = Candidate::new_with_configuration(2, true).await;
    let (id, attempt, target, p) = intent(&mut c, "proof").await;
    let raw = connection(&c.f.p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_dispatch_policy", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let capture = c.host_report().await;
    let sequence = c.save_host("proof-capture", &capture).await;
    let command = cmd("proof-dispatch");
    let grant =
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
            .await
            .unwrap();
    assert_eq!(grant["dispatch_granted"], true);
    let stored = raw.query_one("SELECT proof,digest,host_policy_sequence FROM th_github_check_dispatch_policy WHERE publication=$1",&[&id]).await.unwrap();
    let proof: PolicyProof = serde_json::from_value(stored.get("proof")).unwrap();
    assert_eq!(proof.target, target);
    let wire: treeharbor_metadata::github::WireCommand = serde_json::from_value(json!({
        "action":"check_dispatch_policy", "epoch":1, "publication":id
    }))
    .unwrap();
    let read = wire
        .execute(&mut c.f.p.f.db, &c.reader, None)
        .await
        .unwrap();
    assert_eq!(read["recorded"], true);
    assert_eq!(read["merge_verified"], false);
    assert_eq!(read["proof"], json!(proof));
    assert!(
        c.f.p
            .f
            .db
            .github_check_dispatch_policy(&c.bridge, 1, id)
            .await
            .is_err()
    );
    assert_eq!(proof.host_policy_sequence, sequence);
    let digest: treeharbor_core::ObjectId = proof.digest().unwrap().parse().unwrap();
    assert_eq!(stored.get::<_, Vec<u8>>("digest"), digest.as_bytes());
    assert_eq!(proof.collection_key, "managed-aggregate-0-collect");
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_operations WHERE kind='github.check_publication.dispatch.v1'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    for sql in [
        "UPDATE th_github_check_dispatch_policy SET proof=proof",
        "DELETE FROM th_github_check_dispatch_policy",
    ] {
        assert!(raw.batch_execute(sql).await.is_err());
    }
    let retry =
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
            .await
            .unwrap();
    assert_eq!(retry["dispatch_granted"], false);
    assert!(
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &cmd("different-replay"), id, attempt, &p)
            .await
            .is_err()
    );
    // Cancellation and a later policy capture cannot revoke the historical fact
    // that the original POST was authorized or force recovery to POST again.
    c.f.p
        .f
        .db
        .cancel_github_check_publication(&c.publisher, &cmd("proof-cancel"), id)
        .await
        .unwrap();
    let r = report(&c.f.p, &target, Some(p), cp::Outcome::Observed).await;
    c.f.p
        .f
        .db
        .report_github_check_publication(&c.bridge, &cmd("proof-late-receipt"), id, attempt, &r)
        .await
        .unwrap();
    let retained: Value = raw
        .query_one(
            "SELECT proof FROM th_github_check_dispatch_policy WHERE publication=$1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(retained, json!(proof));
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; bad current capture cannot leave a grant, proof, receipt or outbox event"]
async fn dispatch_policy_refuses_missing_stale_drifted_or_untrusted_captures_before_grant() {
    for case in [
        "missing",
        "stale",
        "before_collection",
        "configuration",
        "revoked",
        "preflight",
    ] {
        let mut c = Candidate::new_with_configuration(1, true).await;
        let (id, attempt, _, mut p) = intent(&mut c, case).await;
        let raw = connection(&c.f.p.f.schema).await;
        if case != "missing" {
            let mut capture = c.host_report().await;
            match case {
                "stale" => {
                    capture.started_at_unix_ms -= 130_000;
                    capture.finished_at_unix_ms -= 130_000;
                }
                "before_collection" => {
                    capture.started_at_unix_ms = 1;
                    capture.finished_at_unix_ms = 1;
                }
                "configuration" => {
                    let h = capture.host_policy.as_mut().unwrap();
                    for s in [&mut h.before, &mut h.after] {
                        s.visibility.as_mut().unwrap().legacy.as_mut().unwrap().id =
                            "BPR_replaced".into();
                    }
                }
                _ => (),
            }
            let saved =
                c.f.p
                    .f
                    .db
                    .report_github_queue(
                        &c.f.bridge,
                        &cmd("deny-capture"),
                        c.f.id,
                        c.attempt,
                        &capture,
                    )
                    .await;
            let seq = if matches!(case, "stale" | "before_collection") {
                // The queue intake already rejects captures predating the
                // attempt. The dispatch must still have no usable capture.
                assert!(saved.is_err(), "{case}");
                0
            } else {
                saved.unwrap()["report_sequence"].as_i64().unwrap()
            };
            if case == "revoked" {
                let token: Uuid = raw
                    .query_one(
                        "SELECT token FROM th_github_queue_reports WHERE sequence=$1",
                        &[&seq],
                    )
                    .await
                    .unwrap()
                    .get(0);
                c.f.p
                    .f
                    .db
                    .revoke_repository_token(&c.f.p.f.scope, token, "operator")
                    .await
                    .unwrap();
            }
            if case == "preflight" {
                p.policy_before.protection.sha256 = format!("sha256:{}", "ab".repeat(32));
                p.policy_after.protection.sha256 = p.policy_before.protection.sha256.clone();
            }
        }
        let command = cmd("denied-dispatch");
        assert!(
            c.f.p
                .f
                .db
                .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(
            raw.query_one("SELECT count(*) FROM th_github_check_dispatch_policy", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0,
            "{case}"
        );
        assert!(
            raw.query_one(
                "SELECT preflight IS NULL FROM th_github_check_publications WHERE publication=$1",
                &[&id]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_operations WHERE key='denied-dispatch'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_outbox WHERE operation_key='denied-dispatch'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; a failure after proof insertion rolls back proof, dispatch, receipt and outbox together"]
async fn dispatch_policy_rollback_removes_proof_before_retrying_same_command() {
    let mut c = Candidate::new_with_configuration(1, true).await;
    let (id, attempt, _, p) = intent(&mut c, "rollback").await;
    let capture = c.host_report().await;
    c.save_host("rollback-host", &capture).await;
    let raw = connection(&c.f.p.f.schema).await;
    raw.batch_execute("CREATE FUNCTION reject_dispatch_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.check_publication.dispatch.v1' THEN IF (SELECT count(*) FROM th_github_check_dispatch_policy)<>1 THEN RAISE EXCEPTION 'fixture missing proof before receipt'; END IF; RAISE EXCEPTION 'fixture rollback after proof'; END IF; RETURN NEW; END; $$; CREATE TRIGGER reject_dispatch BEFORE INSERT ON th_operations FOR EACH ROW EXECUTE FUNCTION reject_dispatch_receipt();").await.unwrap();
    let command = cmd("rollback-dispatch");
    assert!(
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
            .await
            .is_err()
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_dispatch_policy", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        raw.query_one(
            "SELECT preflight IS NULL FROM th_github_check_publications WHERE publication=$1",
            &[&id]
        )
        .await
        .unwrap()
        .get::<_, bool>(0)
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_operations WHERE kind='github.check_publication.dispatch.v1'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    raw.batch_execute(
        "DROP TRIGGER reject_dispatch ON th_operations; DROP FUNCTION reject_dispatch_receipt();",
    )
    .await
    .unwrap();
    assert_eq!(
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
            .await
            .unwrap()["dispatch_granted"],
        true
    );
    assert_eq!(
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; two concurrent deliveries issue only one dispatch grant and retain one proof"]
async fn dispatch_policy_concurrent_replay_keeps_one_grant_and_one_proof() {
    let mut c = Candidate::new_with_configuration(1, true).await;
    let (id, attempt, _, p) = intent(&mut c, "concurrent").await;
    let capture = c.host_report().await;
    c.save_host("concurrent-host", &capture).await;
    let mut other = Metadata::new(connection(&c.f.p.f.schema).await)
        .await
        .unwrap();
    let command = cmd("concurrent-dispatch");
    let (first, second) = tokio::join!(
        c.f.p
            .f
            .db
            .dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p),
        other.dispatch_github_check_publication(&c.bridge, &command, id, attempt, &p)
    );
    let first = first.unwrap();
    let second = second.unwrap();
    assert_ne!(first["dispatch_granted"], second["dispatch_granted"]);
    assert_eq!(first["receipt"], second["receipt"]);
    let raw = connection(&c.f.p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_dispatch_policy", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_operations WHERE kind='github.check_publication.dispatch.v1'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; a failure can be reported without a green-policy certificate and legacy success stays uncertified"]
async fn dispatch_policy_failure_and_legacy_paths_never_invent_success_proofs() {
    for full in [true, false] {
        let mut c = Candidate::new_with_configuration(1, full).await;
        let released = c
            .release(0, if full { 1 } else { 0 }, 2, cp::Outcome::Observed)
            .await;
        assert_eq!(
            released.target.conclusion,
            if full {
                cp::Conclusion::Failure
            } else {
                cp::Conclusion::Success
            }
        );
        let record =
            c.f.p
                .f
                .db
                .github_check_dispatch_policy(&c.reader, 1, released.publication)
                .await
                .unwrap();
        assert_eq!(record["recorded"], false);
        assert!(record["proof"].is_null());
        assert_eq!(record["merge_verified"], false);
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated v30 migration retains old dispatch history without inventing a policy proof"]
async fn dispatch_policy_migration_does_not_backfill_legacy_authorizations() {
    let mut c = Candidate::new_with_configuration(1, true).await;
    let released = c.release(0, 0, 2, cp::Outcome::Observed).await;
    let raw = connection(&c.f.p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_dispatch_policy", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    // Reconstruct the old schema: v30 could retain this dispatch, but had no
    // policy-proof table. Never infer a certificate from a later migration.
    raw.batch_execute("DROP TABLE th_github_check_dispatch_policy; DROP INDEX th_github_managed_check_collections")
        .await
        .unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(30) {
        digest = Sha256::digest([digest, std::fs::read(path).unwrap()].concat()).to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=30,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    let query = "SELECT jsonb_build_object('publications',(SELECT jsonb_agg(to_jsonb(p) ORDER BY publication) FROM th_github_check_publications p),'reports',(SELECT jsonb_agg(to_jsonb(r) ORDER BY publication,sequence) FROM th_github_check_publication_reports r),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o),'outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM th_outbox o),'captures',(SELECT jsonb_agg(to_jsonb(r) ORDER BY queue,sequence) FROM th_github_queue_reports r))";
    let before: Value = raw.query_one(query, &[]).await.unwrap().get(0);
    c.f.p.f.db = Metadata::new(connection(&c.f.p.f.schema).await)
        .await
        .unwrap();
    c.f.p.f.db.migrate().await.unwrap();
    c.f.p.f.db.migrate().await.unwrap();
    assert_eq!(
        raw.query_one(query, &[]).await.unwrap().get::<_, Value>(0),
        before
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_dispatch_policy", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let status =
        c.f.p
            .f
            .db
            .github_check_publication_status(&c.reader, 1, released.publication, false)
            .await
            .unwrap();
    assert_eq!(status["state"], "observed");
    let old =
        c.f.p
            .f
            .db
            .github_check_dispatch_policy(&c.reader, 1, released.publication)
            .await
            .unwrap();
    assert_eq!(old["recorded"], false);
    assert!(old["proof"].is_null());
    assert_eq!(old["merge_verified"], false);
    cleanup(&c.f.p.f.schema).await;
}
