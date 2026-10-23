use super::*;
use treeharbor_metadata::github::{PullIdentity, PullReport, PullSnapshot, PullSpec, PullTarget};

async fn published(p: &mut Projection) -> Uuid {
    let (id, _, value) = publication::requested(p).await;
    let mapping = publication::mapping(&value);
    p.f.db
        .prepare_github_publication(&p.bridge, &cmd("pub-map"), id, &mapping)
        .await
        .unwrap();
    let attempt = Uuid::new_v4();
    p.f.db
        .begin_github_publication(&p.bridge, &cmd("pub-begin"), id, attempt)
        .await
        .unwrap();
    let report = publication::report(p, attempt, &mapping).await;
    p.f.db
        .report_github_publication(&p.bridge, &cmd("pub-report"), id, &report)
        .await
        .unwrap();
    id
}
fn spec(publication: Uuid) -> PullSpec {
    PullSpec {
        publication: publication.to_string(),
        base: "main".into(),
        title: "Agent change".into(),
        body: "Requested change".into(),
        draft: true,
        previous: None,
        ttl_seconds: 60,
    }
}
pub(super) async fn requested(p: &mut Projection) -> (Uuid, PullSpec, Value) {
    let publication = published(p).await;
    let id = Uuid::new_v4();
    let spec = spec(publication);
    let result =
        p.f.db
            .request_github_pull(&p.owner, &cmd("pull-create"), id, &spec)
            .await
            .unwrap();
    (id, spec, result)
}
pub(super) async fn report(p: &Projection, attempt: Uuid, value: &Value) -> PullReport {
    let t: PullTarget = serde_json::from_value(value["target"].clone()).unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../fixtures/github-bind-v1.json")).unwrap();
    let repository = fixture["specification"]["upstream"]["repository_id"]
        .as_u64()
        .unwrap();
    let now = connection(&p.f.schema)
        .await
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    PullReport {
        attempt: attempt.to_string(),
        binding: p.binding.to_string(),
        git_revision: t.git_revision.clone(),
        observed_at_unix_ms: now,
        token_revoked: true,
        pull: Some(PullSnapshot {
            identity: PullIdentity {
                number: 17,
                node_id: "PR_test_17".into(),
            },
            head_repository: repository,
            base_repository: repository,
            head_ref: format!("treeharbor/{}", t.branch_id.simple()),
            base_ref: t.base,
            head_revision: t.git_revision,
            base_revision: format!("git:sha1:{}", "22".repeat(20)),
            title: t.title,
            body: t.body,
            draft: t.draft,
            state: "open".into(),
            merged: false,
        }),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; PR owner intent, graph pin, concurrent dispatch, immutable identity and update lineage"]
async fn pull_preserves_exact_lineage_and_one_shot_claim_without_authorizing_merge() {
    let mut p = Projection::new().await;
    let (id, spec, value) = requested(&mut p).await;
    assert_eq!(
        value,
        p.f.db
            .request_github_pull(&p.owner, &cmd("pull-create"), id, &spec)
            .await
            .unwrap()
    );
    let mut changed = spec.clone();
    changed.title = "Changed".into();
    assert!(matches!(
        p.f.db
            .request_github_pull(&p.owner, &cmd("pull-create"), id, &changed)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    assert!(
        p.f.db
            .request_github_pull(&p.owner, &cmd("competing"), Uuid::new_v4(), &spec)
            .await
            .is_err()
    );
    assert_eq!(value["merge_authorized"], false);
    assert!(
        value["target"]["body"]
            .as_str()
            .unwrap()
            .ends_with(&format!("<!-- treeharbor-pull:{id} -->"))
    );
    let raw = connection(&p.f.schema).await;
    let pin=raw.query_one("SELECT p.digest,q.native_commit FROM th_github_pulls q JOIN th_object_pins p ON p.tenant=q.tenant AND p.repository=q.repository AND p.holder=q.pin_holder",&[]).await.unwrap();
    assert_eq!(pin.get::<_, Vec<u8>>(0), pin.get::<_, Vec<u8>>(1));
    for sql in [
        "UPDATE th_github_pulls SET target='{}'",
        "DELETE FROM th_github_pulls",
    ] {
        assert!(raw.execute(sql, &[]).await.is_err());
    }
    let attempt = Uuid::new_v4();
    let command = cmd("pull-begin");
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        p.f.db.begin_github_pull(&p.bridge, &command, id, attempt),
        other.begin_github_pull(&p.bridge, &command, id, attempt)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a["dispatch_granted"], b["dispatch_granted"]);
    assert_eq!(a["receipt"], b["receipt"]);
    assert_eq!(
        p.f.db
            .begin_github_pull(&p.bridge, &command, id, attempt)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    assert!(
        p.f.db
            .begin_github_pull(&p.bridge, &cmd("different-key"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    let eligibility =
        p.f.db
            .github_pull_context(&p.bridge, 1, id, true, Some(attempt))
            .await
            .unwrap();
    assert!(eligibility.get("dispatch_granted").is_none());
    let r = report(&p, attempt, &value).await;
    let observed =
        p.f.db
            .report_github_pull(&p.bridge, &cmd("pull-report"), id, &r)
            .await
            .unwrap();
    assert_eq!(observed["state"], "observed");
    assert_eq!(observed["merge_authorized"], false);
    assert_eq!(
        observed,
        p.f.db
            .report_github_pull(&p.bridge, &cmd("pull-report"), id, &r)
            .await
            .unwrap()
    );
    let mut update = spec.clone();
    update.previous = Some(id.to_string());
    update.title = "Updated owner title".into();
    let next =
        p.f.db
            .request_github_pull(&p.owner, &cmd("update"), Uuid::new_v4(), &update)
            .await
            .unwrap();
    assert_eq!(
        next["target"]["previous"]["identity"],
        json!(r.pull.as_ref().unwrap().identity)
    );
    assert_eq!(
        next["target"]["previous"]["body"],
        r.pull.as_ref().unwrap().body
    );
    let counts=raw.query_one("SELECT (SELECT count(*) FROM th_native_refs),(SELECT count(*) FROM th_native_check_runs),(SELECT count(*) FROM th_outbox WHERE kind='github.pull.attempting')",&[]).await.unwrap();
    assert_eq!(
        (
            counts.get::<_, i64>(0),
            counts.get::<_, i64>(1),
            counts.get::<_, i64>(2)
        ),
        (0, 0, 1)
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; scope, assignment, current version and cancellation fence PR dispatch"]
async fn pull_fences_begin_and_rechecks_eligibility() {
    for recheck in [false, true] {
        for fence in [
            "version",
            "projection",
            "binding",
            "grant",
            "epoch",
            "cancel",
            "reader",
            "unassigned",
        ] {
            let mut p = Projection::new().await;
            let (id, _, _) = requested(&mut p).await;
            let attempt = Uuid::new_v4();
            if recheck {
                p.f.db
                    .begin_github_pull(&p.bridge, &cmd("begin"), id, attempt)
                    .await
                    .unwrap();
            }
            let mut auth = p.bridge.clone();
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
                        .cancel_github_pull(&p.owner, &cmd("cancel"), id)
                        .await
                        .unwrap();
                }
                "reader" => auth = p.reader.clone(),
                _ => auth = actor(&mut p.f, "another-bridge", K::Bridge, &[P::HostPublish]).await,
            }
            let result = if recheck {
                p.f.db
                    .github_pull_context(&auth, 1, id, true, Some(attempt))
                    .await
            } else {
                p.f.db
                    .begin_github_pull(&auth, &cmd("begin"), id, attempt)
                    .await
            };
            assert!(result.is_err(), "{fence}, recheck={recheck}");
            cleanup(&p.f.schema).await;
        }
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; recovery retains uncertain provider identity after expiry, cancellation and binding revocation"]
async fn pull_reports_reject_wrong_scope_and_keep_observed_identity_across_recovery() {
    let mut p = Projection::new().await;
    let (id, _, value) = requested(&mut p).await;
    let attempt = Uuid::new_v4();
    p.f.db
        .begin_github_pull(&p.bridge, &cmd("begin"), id, attempt)
        .await
        .unwrap();
    let r = report(&p, attempt, &value).await;
    for field in [
        "attempt",
        "binding",
        "git",
        "head_repo",
        "base_repo",
        "head_ref",
        "base_ref",
        "time",
    ] {
        let mut wrong = r.clone();
        match field {
            "attempt" => wrong.attempt = Uuid::new_v4().to_string(),
            "binding" => wrong.binding = Uuid::new_v4().to_string(),
            "git" => wrong.git_revision = format!("git:sha1:{}", "33".repeat(20)),
            "head_repo" => wrong.pull.as_mut().unwrap().head_repository += 1,
            "base_repo" => wrong.pull.as_mut().unwrap().base_repository += 1,
            "head_ref" => wrong.pull.as_mut().unwrap().head_ref = "other".into(),
            "base_ref" => wrong.pull.as_mut().unwrap().base_ref = "other".into(),
            _ => wrong.observed_at_unix_ms += 60000,
        };
        assert!(
            p.f.db
                .report_github_pull(&p.bridge, &cmd(field), id, &wrong)
                .await
                .is_err(),
            "{field}"
        );
    }
    let mut moved = r.clone();
    moved.pull.as_mut().unwrap().head_revision = format!("git:sha1:{}", "44".repeat(20));
    assert_eq!(
        p.f.db
            .report_github_pull(&p.bridge, &cmd("moved"), id, &moved)
            .await
            .unwrap()["state"],
        "uncertain"
    );
    let mut other = r.clone();
    other.pull.as_mut().unwrap().identity.number += 1;
    assert!(
        p.f.db
            .report_github_pull(&p.bridge, &cmd("other-number"), id, &other)
            .await
            .is_err()
    );
    p.f.db
        .cancel_github_pull(&p.owner, &cmd("cancel"), id)
        .await
        .unwrap();
    p.f.db
        .revoke_github(&p.owner, &cmd("revoke"), p.binding)
        .await
        .unwrap();
    // Expiry fixture changes only the clock boundary under a disabled test trigger.
    let raw = connection(&p.f.schema).await;
    raw.batch_execute("ALTER TABLE th_github_pulls DISABLE TRIGGER th_github_pull_update; UPDATE th_github_pulls SET created_at=clock_timestamp()-interval '2 hours',expires_at=clock_timestamp()-interval '1 hour'; ALTER TABLE th_github_pulls ENABLE TRIGGER th_github_pull_update").await.unwrap();
    assert!(
        p.f.db
            .github_pull_context(&p.bridge, 1, id, true, Some(attempt))
            .await
            .is_err()
    );
    let mut final_report = report(&p, attempt, &value).await;
    final_report.token_revoked = false;
    let snapshot = final_report.pull.as_mut().unwrap();
    snapshot.state = "closed".into();
    snapshot.merged = true;
    let observed =
        p.f.db
            .report_github_pull(&p.bridge, &cmd("closed"), id, &final_report)
            .await
            .unwrap();
    assert_eq!(observed["state"], "observed");
    assert_eq!(observed["cancel_requested"], true);
    assert_eq!(observed["observation"]["token_revoked"], false);
    assert_eq!(observed["merge_authorized"], false);
    p.f.db
        .set_repository_access(&p.f.scope, "bridge", &[P::HostRead], "operator")
        .await
        .unwrap();
    assert!(
        p.f.db
            .github_pull_context(&p.bridge, 1, id, true, None)
            .await
            .is_err()
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; cross-tenant PR access and owner input restrictions"]
async fn pull_scope_and_owner_inputs_cannot_select_another_repository_or_retarget() {
    let mut p = Projection::new().await;
    let (id, spec, _) = requested(&mut p).await;
    let original = p.f.scope.clone();
    for scope in [
        Scope::new("foreign", original.repository()).unwrap(),
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
                .github_pull_context(&foreign, 1, id, true, None)
                .await,
            Err(MetaError::NotFound)
        ));
    }
    assert!(
        p.f.db
            .request_github_pull(&p.bridge, &cmd("bridge-request"), Uuid::new_v4(), &spec)
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .begin_github_pull(&p.owner, &cmd("owner-begin"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    p.f.db
        .cancel_github_pull(&p.owner, &cmd("cancel"), id)
        .await
        .unwrap();
    for base in ["../main", "main?x=1", "main.lock", "main//other"] {
        let mut wrong = spec.clone();
        wrong.base = base.into();
        assert!(
            p.f.db
                .request_github_pull(&p.owner, &cmd("bad-base"), Uuid::new_v4(), &wrong)
                .await
                .is_err()
        );
    }
    let mut wrong = spec;
    wrong.body = "<!-- treeharbor-pull:forged -->".into();
    assert!(
        p.f.db
            .request_github_pull(&p.owner, &cmd("forged"), Uuid::new_v4(), &wrong)
            .await
            .is_err()
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; expiry at receipt/outbox boundary rolls back PR dispatch"]
async fn pull_deadline_after_writes_cannot_leave_dispatch_authorization() {
    for table in ["th_operations", "th_outbox"] {
        let mut p = Projection::new().await;
        let publication = published(&mut p).await;
        let id = Uuid::new_v4();
        let mut s = spec(publication);
        s.ttl_seconds = 1;
        p.f.db
            .request_github_pull(&p.owner, &cmd("pull-request"), id, &s)
            .await
            .unwrap();
        let raw = connection(&p.f.schema).await;
        let kind = if table == "th_operations" {
            "github.pull.begin.v1"
        } else {
            "github.pull.attempting"
        };
        raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_pull() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_pull AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_pull();")).await.unwrap();
        assert!(
            p.f.db
                .begin_github_pull(&p.bridge, &cmd("late-begin"), id, Uuid::new_v4())
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
            raw.query_one("SELECT state FROM th_github_pulls", &[])
                .await
                .unwrap()
                .get::<_, String>(0),
            "requested"
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
#[ignore = "requires PostgreSQL; populated schema18 publication, mapping and graph survive PR migration"]
async fn pull_upgrade_preserves_observed_publication_and_export() {
    use sha2::{Digest, Sha256};
    let mut p = Projection::new().await;
    let publication = published(&mut p).await;
    let before =
        p.f.db
            .github_publication_status(&p.bridge, 1, publication, true)
            .await
            .unwrap();
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_pulls", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
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
    raw.batch_execute("DROP TABLE th_github_pulls; DROP FUNCTION th_github_pull_immutable()")
        .await
        .unwrap();
    let mut files: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "sql"))
        .collect();
    files.sort();
    let mut digest = Vec::new();
    for path in files.iter().take(18) {
        let mut h = Sha256::new();
        h.update(digest);
        h.update(std::fs::read(path).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=18,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    p.f.db.migrate().await.unwrap();
    p.f.db.check_schema().await.unwrap();
    let after =
        p.f.db
            .github_publication_status(&p.bridge, 1, publication, true)
            .await
            .unwrap();
    assert_eq!(before, after);
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
    assert_eq!(
        p.f.db
            .request_github_pull(
                &p.owner,
                &cmd("post-upgrade"),
                Uuid::new_v4(),
                &spec(publication)
            )
            .await
            .unwrap()["state"],
        "requested"
    );
    cleanup(&p.f.schema).await;
}
