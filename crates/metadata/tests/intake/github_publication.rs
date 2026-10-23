use super::*;
use treeharbor_metadata::github::{
    PublicationMapping, PublicationOutcome, PublicationReport, PublicationSpec,
};

pub(super) async fn requested(p: &mut Projection) -> (Uuid, PublicationSpec, Value) {
    p.ready().await;
    let id = Uuid::new_v4();
    let spec = PublicationSpec {
        projection: p.id.to_string(),
        expected_head: None,
        ttl_seconds: 60,
    };
    let value =
        p.f.db
            .request_github_publication(&p.owner, &cmd("pub-request"), id, &spec)
            .await
            .unwrap();
    (id, spec, value)
}
pub(super) fn mapping(value: &Value) -> PublicationMapping {
    PublicationMapping {
        branch_id: value["branch_id"].as_str().unwrap().into(),
        native_revision: value["native_revision"].as_str().unwrap().into(),
        git_revision: format!("git:sha1:{}", "11".repeat(20)),
    }
}
pub(super) async fn report(
    p: &Projection,
    attempt: Uuid,
    m: &PublicationMapping,
) -> PublicationReport {
    let now: i64 = connection(&p.f.schema)
        .await
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    PublicationReport {
        attempt: attempt.to_string(),
        branch_id: m.branch_id.clone(),
        binding: p.binding.to_string(),
        native_revision: m.native_revision.clone(),
        prepared_git: m.git_revision.clone(),
        remote_revision: Some(m.git_revision.clone()),
        outcome: PublicationOutcome::Observed,
        observed_at_unix_ms: now,
        token_revoked: true,
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; immutable publication intent, exact mapping, pin and receipt replay"]
async fn publication_binds_owner_source_and_preserves_facts_without_authorizing_main() {
    let mut p = Projection::new().await;
    let (id, spec, value) = requested(&mut p).await;
    assert_eq!(
        value,
        p.f.db
            .request_github_publication(&p.owner, &cmd("pub-request"), id, &spec)
            .await
            .unwrap()
    );
    let mut other = spec.clone();
    other.expected_head = Some(format!("git:sha1:{}", "22".repeat(20)));
    assert!(matches!(
        p.f.db
            .request_github_publication(&p.owner, &cmd("pub-request"), id, &other)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    assert!(matches!(
        p.f.db
            .request_github_publication(&p.owner, &cmd("pub-another"), Uuid::new_v4(), &spec)
            .await,
        Err(MetaError::HostPublicationState)
    ));
    let raw = connection(&p.f.schema).await;
    let pin=raw.query_one("SELECT p.digest,q.native_commit FROM th_github_publications q JOIN th_object_pins p ON p.tenant=q.tenant AND p.repository=q.repository AND p.holder=q.pin_holder",&[]).await.unwrap();
    assert_eq!(pin.get::<_, Vec<u8>>(0), pin.get::<_, Vec<u8>>(1));
    let m = mapping(&value);
    assert!(
        p.f.db
            .prepare_github_publication(&p.reader, &cmd("read-only"), id, &m)
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .prepare_github_publication(&p.owner, &cmd("human"), id, &m)
            .await
            .is_err()
    );
    let mut wrong = m.clone();
    wrong.native_revision = format!("native:sha256:{}", "33".repeat(32));
    assert!(
        p.f.db
            .prepare_github_publication(&p.bridge, &cmd("wrong-root"), id, &wrong)
            .await
            .is_err()
    );
    p.f.db
        .prepare_github_publication(&p.bridge, &cmd("pub-map"), id, &m)
        .await
        .unwrap();
    let attempt = Uuid::new_v4();
    let first =
        p.f.db
            .begin_github_publication(&p.bridge, &cmd("pub-begin"), id, attempt)
            .await
            .unwrap();
    assert_eq!(first["dispatch_granted"], true);
    let current =
        p.f.db
            .github_publication_dispatch_context(&p.bridge, 1, id, attempt)
            .await
            .unwrap();
    assert_eq!(current["state"], "attempting");
    assert!(current.get("dispatch_granted").is_none());
    for invalid in [Uuid::nil(), Uuid::new_v4()] {
        assert!(
            p.f.db
                .github_publication_dispatch_context(&p.bridge, 1, id, invalid)
                .await
                .is_err()
        );
    }
    assert!(
        p.f.db
            .github_publication_dispatch_context(&p.reader, 1, id, attempt)
            .await
            .is_err()
    );
    let retry =
        p.f.db
            .begin_github_publication(&p.bridge, &cmd("pub-begin"), id, attempt)
            .await
            .unwrap();
    assert_eq!(retry["dispatch_granted"], false);
    assert_eq!(retry["receipt"], first["receipt"]);
    assert!(
        p.f.db
            .begin_github_publication(&p.bridge, &cmd("another-begin"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    let mut observed = report(&p, attempt, &m).await;
    for field in [
        "attempt",
        "branch_id",
        "binding",
        "native_revision",
        "prepared_git",
    ] {
        let mut forged = serde_json::to_value(&observed).unwrap();
        forged[field] = json!(match field {
            "native_revision" => format!("native:sha256:{}", "77".repeat(32)),
            "prepared_git" => format!("git:sha1:{}", "77".repeat(20)),
            _ => Uuid::new_v4().to_string(),
        });
        let forged = serde_json::from_value(forged).unwrap();
        assert!(
            p.f.db
                .report_github_publication(&p.bridge, &cmd(&format!("forged-{field}")), id, &forged)
                .await
                .is_err()
        );
    }
    observed.token_revoked = false;
    let done =
        p.f.db
            .report_github_publication(&p.bridge, &cmd("pub-report"), id, &observed)
            .await
            .unwrap();
    assert_eq!(done["state"], "observed");
    assert!(
        p.f.db
            .github_publication_dispatch_context(&p.bridge, 1, id, attempt)
            .await
            .is_err()
    );
    assert_eq!(done["merge_authorized"], false);
    assert_eq!(done["observation"]["token_revoked"], false);
    assert_eq!(
        done,
        p.f.db
            .report_github_publication(&p.bridge, &cmd("pub-report"), id, &observed)
            .await
            .unwrap()
    );
    assert!(
        raw.execute(
            "UPDATE th_github_publications SET prepared_git=$1",
            &[&format!("git:sha1:{}", "44".repeat(20))]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute("DELETE FROM th_github_publications", &[])
            .await
            .is_err()
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_native_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='github.publication.attempting'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; competing claims get one fresh dispatch and retain uncertain cancelled effects"]
async fn publication_claim_is_one_shot_and_cancel_does_not_erase_an_inflight_effect() {
    let mut p = Projection::new().await;
    let (id, spec, value) = requested(&mut p).await;
    let m = mapping(&value);
    p.f.db
        .prepare_github_publication(&p.bridge, &cmd("map"), id, &m)
        .await
        .unwrap();
    let mut a = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let mut b = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let attempt = Uuid::new_v4();
    let c = cmd("claim");
    let (a, b) = tokio::join!(
        a.begin_github_publication(&p.bridge, &c, id, attempt),
        b.begin_github_publication(&p.bridge, &c, id, attempt)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a["dispatch_granted"], b["dispatch_granted"]);
    assert_eq!(a["receipt"], b["receipt"]);
    let cancelled =
        p.f.db
            .cancel_github_publication(&p.owner, &cmd("pub-cancel"), id)
            .await
            .unwrap();
    assert_eq!(cancelled["state"], "attempting");
    assert_eq!(cancelled["cancel_requested"], true);
    let mut observation = report(&p, attempt, &m).await;
    observation.outcome = PublicationOutcome::Unknown;
    observation.remote_revision = None;
    p.f.db
        .report_github_publication(&p.bridge, &cmd("unknown"), id, &observation)
        .await
        .unwrap();
    assert!(
        p.f.db
            .request_github_publication(&p.owner, &cmd("blocked"), Uuid::new_v4(), &spec)
            .await
            .is_err()
    );
    observation.outcome = PublicationOutcome::NotSent;
    assert!(
        p.f.db
            .report_github_publication(&p.bridge, &cmd("false-no-effect"), id, &observation)
            .await
            .is_err()
    );
    p.new_version().await;
    p.f.db
        .cancel_github_projection(&p.owner, &cmd("projection-cancel"), p.id)
        .await
        .unwrap();
    p.f.db
        .revoke_github(&p.owner, &cmd("binding-revoke"), p.binding)
        .await
        .unwrap();
    let observation = report(&p, attempt, &m).await;
    let recovered =
        p.f.db
            .report_github_publication(&p.bridge, &cmd("recovered"), id, &observation)
            .await
            .unwrap();
    assert_eq!(recovered["state"], "observed");
    assert_eq!(recovered["cancel_requested"], true);
    assert_eq!(
        p.f.db
            .begin_github_publication(&p.bridge, &c, id, attempt)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; current version, cancellation, assignment, credentials and epoch fence dispatch"]
async fn publication_dispatch_rejects_changed_context() {
    for recheck in [false, true] {
        for fence in [
            "version",
            "projection",
            "binding",
            "grant",
            "epoch",
            "cancel",
            "assignment",
            "credential",
        ] {
            let mut p = Projection::new().await;
            let (id, _, value) = requested(&mut p).await;
            let m = mapping(&value);
            p.f.db
                .prepare_github_publication(&p.bridge, &cmd("map"), id, &m)
                .await
                .unwrap();
            let attempt = Uuid::new_v4();
            if recheck {
                p.f.db
                    .begin_github_publication(&p.bridge, &cmd("original-dispatch"), id, attempt)
                    .await
                    .unwrap();
            }
            let auth = if fence == "assignment" {
                actor(&mut p.f, "other", K::Bridge, &[P::HostPublish]).await
            } else if fence == "credential" {
                let token = p
                    .f
                    .db
                    .issue_repository_token(&p.f.scope, "bridge", &[P::HostPublish], 60, "operator")
                    .await
                    .unwrap();
                let auth =
                    p.f.db
                        .authorize(&token.secret, &p.f.scope, P::HostPublish)
                        .await
                        .unwrap();
                p.f.db
                    .revoke_repository_token(&p.f.scope, token.id, "operator")
                    .await
                    .unwrap();
                auth
            } else {
                p.bridge.clone()
            };
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
                        .revoke_github(&p.owner, &cmd("revoke-binding"), p.binding)
                        .await
                        .unwrap();
                }
                "grant" => {
                    p.f.db
                        .set_repository_access(&p.f.scope, "bridge", &[P::HostRead], "operator")
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
                        .cancel_github_publication(&p.owner, &cmd("cancel-publication"), id)
                        .await
                        .unwrap();
                }
                _ => {}
            }
            let rejected = if recheck {
                p.f.db
                    .github_publication_dispatch_context(&auth, 1, id, attempt)
                    .await
                    .is_err()
            } else {
                p.f.db
                    .begin_github_publication(&auth, &cmd("begin"), id, Uuid::new_v4())
                    .await
                    .is_err()
            };
            assert!(rejected, "{fence}, recheck={recheck}");
            assert_eq!(
                connection(&p.f.schema)
                    .await
                    .query_one(
                        "SELECT count(*) FROM th_github_publications WHERE attempt IS NOT NULL",
                        &[]
                    )
                    .await
                    .unwrap()
                    .get::<_, i64>(0),
                i64::from(recheck)
            );
            cleanup(&p.f.schema).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; deadline after receipt/outbox writes rolls back one-shot dispatch"]
async fn publication_deadline_after_writes_cannot_leave_a_dispatch_receipt() {
    for table in ["th_operations", "th_outbox"] {
        let mut p = Projection::new().await;
        p.spec.ttl_seconds = 3;
        let (id, _, value) = requested(&mut p).await;
        let m = mapping(&value);
        p.f.db
            .prepare_github_publication(&p.bridge, &cmd("map"), id, &m)
            .await
            .unwrap();
        let raw = connection(&p.f.schema).await;
        let kind = if table == "th_operations" {
            "github.publication.begin.v1"
        } else {
            "github.publication.attempting"
        };
        raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_pub() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(3.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_pub AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_pub();")).await.unwrap();
        assert!(
            p.f.db
                .begin_github_publication(&p.bridge, &cmd("late-begin"), id, Uuid::new_v4())
                .await
                .is_err()
        );
        assert!(
            raw.query_one("SELECT is_called FROM delay_seen", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(
            raw.query_one("SELECT state FROM th_github_publications", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            "prepared"
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_operations WHERE key='late-begin'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; publication identities are not readable or writable from a different tenant"]
async fn publication_scope_rejects_foreign_tenant_and_repository() {
    let mut p = Projection::new().await;
    let (id, _, value) = requested(&mut p).await;
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
        let foreign = actor(&mut p.f, "foreign", K::Bridge, &[P::HostPublish]).await;
        p.f.scope = original.clone();
        assert!(matches!(
            p.f.db
                .github_publication_status(&foreign, 1, id, true)
                .await,
            Err(MetaError::NotFound)
        ));
        assert!(
            p.f.db
                .prepare_github_publication(&foreign, &cmd("foreign-map"), id, &mapping(&value))
                .await
                .is_err()
        );
    }
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; schema17 ready projection and graph pins survive additive publication upgrade"]
async fn publication_upgrade_retains_a_ready_projection_and_its_export() {
    use sha2::{Digest, Sha256};
    let mut p = Projection::new().await;
    let (ready, _) = p.ready().await;
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_publications", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_pulls", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    raw.batch_execute("DROP TABLE th_github_pulls; DROP FUNCTION th_github_pull_immutable()")
        .await
        .unwrap();
    // Undo only the empty additive v18 schema to reconstruct the actual v17
    // tables, including a real accepted projection and its retained graph.
    raw.batch_execute(
        "DROP TABLE th_github_publications; DROP FUNCTION th_github_publication_immutable()",
    )
    .await
    .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    files.sort();
    let mut digest = Vec::new();
    for path in files.iter().take(17) {
        let mut hash = Sha256::new();
        hash.update(digest);
        hash.update(std::fs::read(path).unwrap());
        digest = hash.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=17,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    p.f.db.migrate().await.unwrap();
    p.f.db.check_schema().await.unwrap();
    let context =
        p.f.db
            .github_projection_context(&p.bridge, 1, p.id)
            .await
            .unwrap();
    assert_eq!(context["revision"], ready["revision"]);
    let export =
        p.f.db
            .open_github_projection_export(&p.bridge, 1, p.id, 60)
            .await
            .unwrap();
    p.f.db
        .read_source_export(&export, &p.f.depot, &[export.root()], 1_000_000)
        .await
        .unwrap();
    p.f.db.finish_source_export(export).await.unwrap();
    let pubspec = PublicationSpec {
        projection: p.id.to_string(),
        expected_head: None,
        ttl_seconds: 60,
    };
    assert_eq!(
        p.f.db
            .request_github_publication(&p.owner, &cmd("after-upgrade"), Uuid::new_v4(), &pubspec)
            .await
            .unwrap()["state"],
        "requested"
    );
    cleanup(&p.f.schema).await;
}
