use super::*;
use treeharbor_github_contract::{InspectionReport, InspectionTarget, RequiredCheck};
use treeharbor_metadata::github::InspectionSpec;
async fn observed_pull(p: &mut Projection) -> Uuid {
    let (id, _, v) = pulls::requested(p).await;
    let attempt = Uuid::new_v4();
    p.f.db
        .begin_github_pull(&p.bridge, &cmd("pull-begin"), id, attempt)
        .await
        .unwrap();
    let r = pulls::report(p, attempt, &v).await;
    p.f.db
        .report_github_pull(&p.bridge, &cmd("pull-report"), id, &r)
        .await
        .unwrap();
    id
}
fn spec(pull: Uuid) -> InspectionSpec {
    InspectionSpec {
        pull: pull.to_string(),
        required: vec![RequiredCheck {
            name: "CI / build".into(),
            app_id: 7,
        }],
        ttl_seconds: 60,
    }
}
pub(super) async fn requested(p: &mut Projection) -> (Uuid, InspectionSpec, Value) {
    let pull = observed_pull(p).await;
    let id = Uuid::new_v4();
    let spec = spec(pull);
    let v =
        p.f.db
            .request_github_inspection(&p.owner, &cmd("inspection-request"), id, &spec)
            .await
            .unwrap();
    (id, spec, v)
}
pub(super) async fn report(p: &Projection, v: &Value) -> InspectionReport {
    let f: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/inspection-v1.json"
    )))
    .unwrap();
    let mut r: InspectionReport = serde_json::from_value(f["report"].clone()).unwrap();
    let t: InspectionTarget = serde_json::from_value(v["target"].clone()).unwrap();
    for state in [&mut r.before, &mut r.after] {
        state.repository_id = t.repository_id;
        state.number = t.pull_number;
        state.node_id = t.pull_node.clone();
        state.head_ref = t.head_ref.clone();
        state.base_ref = t.base_ref.clone();
        state.head_revision = t.head_revision.clone();
    }
    r.checks[0].runs[0].head_revision = t.head_revision;
    let now = connection(&p.f.schema)
        .await
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    r.started_at_unix_ms = now;
    r.finished_at_unix_ms = now;
    r
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact inspection lineage, immutable replay and stale capture semantics"]
async fn inspection_records_exact_capture_without_granting_merge_or_carrying_old_success() {
    let mut p = Projection::new().await;
    let (id, spec, v) = requested(&mut p).await;
    assert_eq!(
        v,
        p.f.db
            .request_github_inspection(&p.owner, &cmd("inspection-request"), id, &spec)
            .await
            .unwrap()
    );
    let mut changed = spec.clone();
    changed.required[0].app_id += 1;
    assert!(matches!(
        p.f.db
            .request_github_inspection(&p.owner, &cmd("inspection-request"), id, &changed)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    let r = report(&p, &v).await;
    let command = cmd("inspection-report");
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        p.f.db.report_github_inspection(&p.reader, &command, id, &r),
        other.report_github_inspection(&p.reader, &command, id, &r)
    );
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(a["selected_checks_passed_at_capture"], true);
    assert_eq!(a["host_policy_verified"], false);
    assert_eq!(a["merge_authorized"], false);
    assert!(a.get("is_current_version").is_none());
    assert!(a.get("unexpired").is_none());
    let raw = connection(&p.f.schema).await;
    let pin=raw.query_one("SELECT p.digest,q.native_commit FROM th_github_inspections q JOIN th_object_pins p ON p.tenant=q.tenant AND p.repository=q.repository AND p.holder=q.pin_holder",&[]).await.unwrap();
    assert_eq!(pin.get::<_, Vec<u8>>(0), pin.get::<_, Vec<u8>>(1));
    for sql in [
        "UPDATE th_github_inspections SET report='{}'",
        "UPDATE th_github_inspections SET target='{}'",
        "DELETE FROM th_github_inspections",
    ] {
        assert!(raw.execute(sql, &[]).await.is_err());
    }
    p.new_version().await;
    assert_eq!(
        a,
        p.f.db
            .report_github_inspection(&p.reader, &command, id, &r)
            .await
            .unwrap()
    );
    let token =
        p.f.db
            .issue_repository_token(&p.f.scope, "human", &[P::Read], 60, "operator")
            .await
            .unwrap();
    let reader =
        p.f.db
            .authorize(&token.secret, &p.f.scope, P::Read)
            .await
            .unwrap();
    let stale =
        p.f.db
            .github_inspection_context(&reader, 1, id, false, false)
            .await
            .unwrap();
    assert_eq!(stale["selected_checks_passed_at_capture"], true);
    assert_eq!(stale["is_current_version"], false);
    assert_eq!(stale["merge_authorized"], false);
    assert!(
        p.f.db
            .request_github_inspection(&p.owner, &cmd("old-source"), Uuid::new_v4(), &spec)
            .await
            .is_err()
    );
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_native_check_runs)+(SELECT count(*) FROM th_native_refs)",&[]).await.unwrap().get::<_,i64>(0),0);
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; new collection fences and historical read-only recovery"]
async fn inspection_collection_fences_current_source_assignment_binding_and_deadline() {
    for fence in [
        "version",
        "projection",
        "binding",
        "grant",
        "epoch",
        "cancel",
        "expired",
        "unassigned",
        "publisher",
    ] {
        let mut p = Projection::new().await;
        let (id, _, _) = requested(&mut p).await;
        let mut auth = p.reader.clone();
        match fence {
            "version" => p.new_version().await,
            "projection" => {
                p.f.db
                    .cancel_github_projection(&p.owner, &cmd("cancel-projection"), p.id)
                    .await
                    .unwrap();
            }
            "binding" => {
                p.f.db
                    .revoke_github(&p.owner, &cmd("revoke"), p.binding)
                    .await
                    .unwrap();
            }
            "grant" => {
                p.f.db
                    .set_repository_access(&p.f.scope, "bridge", &[P::HostPublish], "operator")
                    .await
                    .unwrap();
            }
            "epoch" => {
                connection(&p.f.schema)
                    .await
                    .execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
            "cancel" => {
                p.f.db
                    .cancel_github_inspection(&p.owner, &cmd("cancel-inspection"), id)
                    .await
                    .unwrap();
            }
            "expired" => {
                connection(&p.f.schema).await.batch_execute("ALTER TABLE th_github_inspections DISABLE TRIGGER th_github_inspection_update; UPDATE th_github_inspections SET created_at=clock_timestamp()-interval '2 hours',expires_at=clock_timestamp()-interval '1 hour'; ALTER TABLE th_github_inspections ENABLE TRIGGER th_github_inspection_update").await.unwrap();
            }
            "unassigned" => auth = actor(&mut p.f, "other", K::Bridge, &[P::HostRead]).await,
            _ => {
                let t = p
                    .f
                    .db
                    .issue_repository_token(&p.f.scope, "bridge", &[P::HostPublish], 60, "operator")
                    .await
                    .unwrap();
                auth =
                    p.f.db
                        .authorize(&t.secret, &p.f.scope, P::HostPublish)
                        .await
                        .unwrap();
            }
        }
        assert!(
            p.f.db
                .github_inspection_context(&auth, 1, id, true, true)
                .await
                .is_err(),
            "{fence}"
        );
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; expired publication can be inspected, but old captures never become current validation"]
async fn inspection_owns_its_deadline_and_recovers_capture_after_request_expiry_and_revocation() {
    let mut p = Projection::new().await;
    let pull = observed_pull(&mut p).await;
    let raw = connection(&p.f.schema).await;
    raw.batch_execute("ALTER TABLE th_github_projections DISABLE TRIGGER th_github_projection_update; UPDATE th_github_projections SET created_at=clock_timestamp()-interval '2 hours',expires_at=clock_timestamp()-interval '1 hour'; ALTER TABLE th_github_projections ENABLE TRIGGER th_github_projection_update").await.unwrap();
    let id = Uuid::new_v4();
    let mut s = spec(pull);
    s.ttl_seconds = 1;
    let v =
        p.f.db
            .request_github_inspection(&p.owner, &cmd("inspect-expired-publication"), id, &s)
            .await
            .unwrap();
    p.f.db
        .github_inspection_context(&p.reader, 1, id, true, true)
        .await
        .unwrap();
    let mut r = report(&p, &v).await;
    r.token_revoked = false;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    p.f.db
        .revoke_github(&p.owner, &cmd("revoke"), p.binding)
        .await
        .unwrap();
    p.new_version().await;
    let result =
        p.f.db
            .report_github_inspection(&p.reader, &cmd("delayed-report"), id, &r)
            .await
            .unwrap();
    assert_eq!(result["state"], "observed");
    assert!(result.get("is_current_version").is_none());
    let status =
        p.f.db
            .github_inspection_context(&p.reader, 1, id, true, false)
            .await
            .unwrap();
    assert_eq!(status["is_current_version"], false);
    assert_eq!(result["report"]["token_revoked"], false);
    assert_eq!(result["merge_authorized"], false);
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; foreign scope, forged success and capture outside requested window"]
async fn inspection_rejects_foreign_scope_and_invalid_reports() {
    let mut p = Projection::new().await;
    let (id, _, v) = requested(&mut p).await;
    let r = report(&p, &v).await;
    assert!(
        p.f.db
            .report_github_inspection(&p.owner, &cmd("human"), id, &r)
            .await
            .is_err()
    );
    for field in ["app", "sha", "repo", "number", "late", "old"] {
        let mut wrong = r.clone();
        match field {
            "app" => wrong.checks[0].runs[0].app_id += 1,
            "sha" => {
                wrong.checks[0].runs[0].head_revision = format!("git:sha1:{}", "33".repeat(20))
            }
            "repo" => wrong.before.repository_id += 1,
            "number" => wrong.before.number += 1,
            "late" => {
                wrong.started_at_unix_ms += 100000;
                wrong.finished_at_unix_ms += 100000;
            }
            _ => {
                wrong.started_at_unix_ms -= 100000;
                wrong.finished_at_unix_ms -= 100000;
            }
        }
        assert!(
            p.f.db
                .report_github_inspection(&p.reader, &cmd(field), id, &wrong)
                .await
                .is_err(),
            "{field}"
        );
    }
    let original = p.f.scope.clone();
    for scope in [
        Scope::new("other-tenant", original.repository()).unwrap(),
        Scope::new(original.tenant(), Uuid::new_v4()).unwrap(),
    ] {
        p.f.db
            .create_repository(&scope, Authority::Connected)
            .await
            .unwrap();
        p.f.scope = scope;
        let foreign = actor(&mut p.f, "foreign", K::Bridge, &[P::HostRead]).await;
        p.f.scope = original.clone();
        assert!(matches!(
            p.f.db
                .github_inspection_context(&foreign, 1, id, true, false)
                .await,
            Err(MetaError::NotFound)
        ));
    }
    let mut drift = r;
    drift.after.base_revision = format!("git:sha1:{}", "33".repeat(20));
    let result =
        p.f.db
            .report_github_inspection(&p.reader, &cmd("drift"), id, &drift)
            .await
            .unwrap();
    assert_eq!(result["capture_consistent"], false);
    assert_eq!(result["selected_checks_passed_at_capture"], false);
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; expiry during request commit rolls back snapshot identity and pin"]
async fn inspection_deadline_after_receipt_or_outbox_rolls_back_the_whole_request() {
    for table in ["th_operations", "th_outbox"] {
        let mut p = Projection::new().await;
        let pull = observed_pull(&mut p).await;
        let mut s = spec(pull);
        s.ttl_seconds = 1;
        let kind = if table == "th_operations" {
            "github.inspection.request.v1"
        } else {
            "github.inspection.requested"
        };
        let raw = connection(&p.f.schema).await;
        raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_inspect() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_inspect AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_inspect();")).await.unwrap();
        assert!(
            p.f.db
                .request_github_inspection(&p.owner, &cmd("late-inspection"), Uuid::new_v4(), &s)
                .await
                .is_err()
        );
        assert!(
            raw.query_one("SELECT is_called FROM delay_seen", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_inspections)+(SELECT count(*) FROM th_object_pins WHERE holder LIKE 'github-inspection:%')",&[]).await.unwrap().get::<_,i64>(0),0);
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; actual schema19 PR observation, publication and pins survive migration"]
async fn inspection_upgrade_preserves_published_pr_and_creates_exact_evidence_request() {
    use sha2::{Digest, Sha256};
    let mut p = Projection::new().await;
    let pull = observed_pull(&mut p).await;
    let before =
        p.f.db
            .github_pull_context(&p.bridge, 1, pull, true, None)
            .await
            .unwrap();
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_inspections", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    super::queue::drop_unpopulated_v21(&raw).await;
    raw.batch_execute(
        "DROP TABLE th_github_inspections; DROP FUNCTION th_github_inspection_immutable()",
    )
    .await
    .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "sql"))
        .collect();
    files.sort();
    let mut digest = Vec::new();
    for path in files.iter().take(19) {
        let mut h = Sha256::new();
        h.update(digest);
        h.update(std::fs::read(path).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=19,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    p.f.db.migrate().await.unwrap();
    p.f.db.check_schema().await.unwrap();
    assert_eq!(
        before,
        p.f.db
            .github_pull_context(&p.bridge, 1, pull, true, None)
            .await
            .unwrap()
    );
    let v =
        p.f.db
            .request_github_inspection(&p.owner, &cmd("after-upgrade"), Uuid::new_v4(), &spec(pull))
            .await
            .unwrap();
    assert_eq!(
        v["target"]["head_revision"],
        before["target"]["git_revision"]
    );
    assert_eq!(
        v["target"]["pull_number"],
        before["observation"]["pull"]["identity"]["number"]
    );
    cleanup(&p.f.schema).await;
}
