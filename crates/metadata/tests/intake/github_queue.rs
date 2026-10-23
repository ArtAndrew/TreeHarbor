use super::*;
use treeharbor_github_contract::queue::*;
use treeharbor_metadata::github::QueueSpec;
struct QueueFixture {
    p: Projection,
    owner: Authorization,
    bridge: Authorization,
    inspection: Uuid,
    spec: QueueSpec,
    id: Uuid,
    target: QueueTarget,
}
impl QueueFixture {
    async fn new() -> Self {
        let mut p = Projection::new().await;
        let (inspection, _, v) = inspections::requested(&mut p).await;
        let report = inspections::report(&p, &v).await;
        p.f.db
            .report_github_inspection(&p.reader, &cmd("inspect-report"), inspection, &report)
            .await
            .unwrap();
        let owner = actor(
            &mut p.f,
            "human",
            K::Human,
            &[P::HostQueue, P::Read, P::RepositoryWrite],
        )
        .await;
        let bridge = actor(
            &mut p.f,
            "bridge",
            K::Bridge,
            &[P::HostMerge, P::HostPublish, P::HostRead],
        )
        .await;
        let target = QueueTarget {
            pull: serde_json::from_value(v["target"].clone()).unwrap(),
            expected_base_revision: report.before.base_revision,
            expected_rules: report.rules_before,
        };
        Self {
            p,
            owner,
            bridge,
            inspection,
            spec: QueueSpec {
                inspection: inspection.to_string(),
                ttl_seconds: 60,
            },
            id: Uuid::new_v4(),
            target,
        }
    }
    async fn request(&mut self) -> Value {
        self.p
            .f
            .db
            .request_github_queue(&self.owner, &cmd("queue-request"), self.id, &self.spec)
            .await
            .unwrap()
    }
    async fn begin(&mut self) -> Uuid {
        let a = Uuid::new_v4();
        self.p
            .f
            .db
            .begin_github_queue(&self.bridge, &cmd("queue-begin"), self.id, a)
            .await
            .unwrap();
        a
    }
    async fn report(&self) -> QueueObservation {
        let now = connection(&self.p.f.schema)
            .await
            .query_one(
                "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        QueueObservation {
            version: 1,
            started_at_unix_ms: now,
            finished_at_unix_ms: now,
            snapshot: None,
            receipt: None,
            async_result: None,
            token_revoked: false,
        }
    }
}
pub(super) async fn drop_unpopulated_v21(raw: &tokio_postgres::Client) {
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_queue)+(SELECT count(*) FROM th_github_queue_reports)",&[]).await.unwrap().get::<_,i64>(0),0);
    raw.batch_execute("DROP TABLE th_github_queue_reports; DROP TABLE th_github_queue; DROP FUNCTION th_github_queue_immutable(); ALTER TABLE th_repository_access DROP CONSTRAINT th_repository_access_permissions_check; ALTER TABLE th_repository_access ADD CHECK(permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','review','policy.write','integration.control']::text[]); ALTER TABLE th_access_tokens DROP CONSTRAINT th_access_tokens_permissions_check; ALTER TABLE th_access_tokens ADD CHECK(cardinality(permissions)>0 AND permissions <@ ARRAY['read','workspace.write','change.write','queue.request','repository.write','runtime.control','validation.report','host.read','host.publish','review','policy.write','integration.control']::text[])").await.unwrap();
}
#[tokio::test]
#[ignore = "requires PostgreSQL; explicit queue capabilities, one-shot dispatch, retained graph and immutable reports"]
async fn queue_requires_explicit_grants_and_never_replays_dispatch() {
    let mut f = QueueFixture::new().await;
    assert!(matches!(
        f.p.f
            .db
            .request_github_queue(&f.p.owner, &cmd("not-owner"), f.id, &f.spec)
            .await,
        Err(MetaError::Forbidden)
    ));
    for (kind, perm) in [
        (K::Agent, P::HostQueue),
        (K::Agent, P::HostMerge),
        (K::Human, P::HostMerge),
        (K::Bridge, P::HostQueue),
    ] {
        let name = format!("wrong-{}-{}", kind.as_str(), perm.as_str());
        f.p.f
            .db
            .provision_principal(f.p.f.scope.tenant(), &name, kind, "operator")
            .await
            .unwrap();
        assert!(
            f.p.f
                .db
                .set_repository_access(&f.p.f.scope, &name, &[perm], "operator")
                .await
                .is_err()
        );
    }
    let v = f.request().await;
    assert!(v.get("unexpired").is_none());
    assert_eq!(v["merge_verified"], false);
    assert_eq!(v["target"], json!(f.target));
    assert_eq!(
        v,
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("queue-request"), f.id, &f.spec)
            .await
            .unwrap()
    );
    assert!(
        f.p.f
            .db
            .begin_github_queue(
                &f.p.bridge,
                &cmd("publisher-cannot-merge"),
                f.id,
                Uuid::new_v4()
            )
            .await
            .is_err()
    );
    let attempt = Uuid::new_v4();
    let mut other = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    let c = cmd("race-begin");
    let (a, b) = tokio::join!(
        f.p.f.db.begin_github_queue(&f.bridge, &c, f.id, attempt),
        other.begin_github_queue(&f.bridge, &c, f.id, attempt)
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    assert_ne!(a["dispatch_granted"], b["dispatch_granted"]);
    assert_eq!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &c, f.id, attempt)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    assert!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("different-begin"), f.id, Uuid::new_v4())
            .await
            .is_err()
    );
    let r = f.report().await;
    let receipt =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("queue-report"), f.id, attempt, &r)
            .await
            .unwrap();
    assert_eq!(receipt["state"], "uncertain");
    assert_eq!(receipt["report_sequence"], 1);
    assert!(receipt.get("is_current_version").is_none());
    assert_eq!(
        receipt,
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("queue-report"), f.id, attempt, &r)
            .await
            .unwrap()
    );
    let raw = connection(&f.p.f.schema).await;
    let pin=raw.query_one("SELECT p.digest,q.native_commit FROM th_github_queue q JOIN th_object_pins p ON p.tenant=q.tenant AND p.repository=q.repository AND p.holder=q.pin_holder",&[]).await.unwrap();
    assert_eq!(pin.get::<_, Vec<u8>>(0), pin.get::<_, Vec<u8>>(1));
    for sql in [
        "UPDATE th_github_queue SET target='{}'",
        "UPDATE th_github_queue SET attempt=NULL,started_at=NULL",
        "DELETE FROM th_github_queue_reports",
        "UPDATE th_github_queue_reports SET report='{}'",
    ] {
        assert!(raw.batch_execute(sql).await.is_err(), "{sql}");
    }
    let v =
        f.p.f
            .db
            .cancel_github_queue(&f.owner, &cmd("cancel"), f.id)
            .await
            .unwrap();
    assert_eq!(v["cancel_requested"], true);
    assert_ne!(v["state"], "cancelled");
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("duplicate-request"), Uuid::new_v4(), &f.spec)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; source, binding, current authority and deadline fences"]
async fn queue_fences_new_effects_but_retains_historical_observations() {
    for fence in [
        "version",
        "projection",
        "binding",
        "grant",
        "epoch",
        "cancel",
        "expiry",
        "unassigned",
    ] {
        let mut f = QueueFixture::new().await;
        f.request().await;
        let a = f.begin().await;
        let r = f.report().await;
        let mut bridge = f.bridge.clone();
        match fence {
            "version" => f.p.new_version().await,
            "projection" => {
                f.p.f
                    .db
                    .cancel_github_projection(&f.p.owner, &cmd("cancel-p"), f.p.id)
                    .await
                    .unwrap();
            }
            "binding" => {
                f.p.f
                    .db
                    .revoke_github(&f.p.owner, &cmd("revoke"), f.p.binding)
                    .await
                    .unwrap();
            }
            "grant" => {
                f.p.f
                    .db
                    .set_repository_access(&f.p.f.scope, "bridge", &[P::HostPublish], "operator")
                    .await
                    .unwrap();
            }
            "epoch" => {
                connection(&f.p.f.schema)
                    .await
                    .execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
            "cancel" => {
                f.p.f
                    .db
                    .cancel_github_queue(&f.owner, &cmd("cancel"), f.id)
                    .await
                    .unwrap();
            }
            "expiry" => {
                connection(&f.p.f.schema).await.batch_execute("ALTER TABLE th_github_queue DISABLE TRIGGER th_github_queue_update; UPDATE th_github_queue SET created_at=clock_timestamp()-interval '2 hours',expires_at=clock_timestamp()-interval '1 hour'; ALTER TABLE th_github_queue ENABLE TRIGGER th_github_queue_update").await.unwrap();
            }
            _ => bridge = actor(&mut f.p.f, "other", K::Bridge, &[P::HostMerge]).await,
        }
        assert!(
            f.p.f
                .db
                .github_queue_context(&bridge, 1, f.id, true, Some(a))
                .await
                .is_err(),
            "{fence}"
        );
        let report =
            f.p.f
                .db
                .report_github_queue(&bridge, &cmd("recover"), f.id, a, &r)
                .await;
        assert_eq!(
            report.is_ok(),
            !matches!(fence, "grant" | "epoch" | "unassigned"),
            "{fence}"
        );
        cleanup(&f.p.f.schema).await;
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL; report scope and retained host merge facts"]
async fn queue_rejects_wrong_attempt_and_time_without_losing_known_host_merge() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let mut r = f.report().await;
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("wrong-attempt"), f.id, Uuid::new_v4(), &r)
            .await
            .is_err()
    );
    r.finished_at_unix_ms += 60_000;
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("future"), f.id, a, &r)
            .await
            .is_err()
    );
    let capture: Value = connection(&f.p.f.schema)
        .await
        .query_one(
            "SELECT report FROM th_github_inspections WHERE inspection=$1",
            &[&f.inspection],
        )
        .await
        .unwrap()
        .get(0);
    let mut pull: treeharbor_github_contract::PullState =
        serde_json::from_value(capture["before"].clone()).unwrap();
    pull.state = "closed".into();
    pull.merged = true;
    r = f.report().await;
    r.snapshot = Some(QueueSnapshot {
        pull,
        queue_enabled: true,
        configuration: None,
        entry: None,
        final_commit: Some(GitCommitObservation {
            revision: "git:sha1:".to_owned() + &"55".repeat(20),
            tree: "git:sha1:".to_owned() + &"66".repeat(20),
        }),
    });
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("merged-fact"), f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["state"], "host_merged_unverified");
    assert_eq!(v["merge_verified"], false);
    let empty = f.report().await;
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("later-network-loss"), f.id, a, &empty)
            .await
            .unwrap();
    assert_eq!(v["state"], "host_merged_unverified");
    assert!(v["retained_merge"].is_object());
    r.started_at_unix_ms = empty.started_at_unix_ms;
    r.finished_at_unix_ms = empty.finished_at_unix_ms;
    r.snapshot
        .as_mut()
        .unwrap()
        .final_commit
        .as_mut()
        .unwrap()
        .tree = "git:sha1:".to_owned() + &"77".repeat(20);
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("substitute-merge"), f.id, a, &r)
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; expiry in receipt/outbox rolls back intent and dispatch"]
async fn queue_rolls_back_when_request_or_dispatch_expires_inside_commit() {
    for stage in ["request", "begin"] {
        for table in ["th_operations", "th_outbox"] {
            let mut f = QueueFixture::new().await;
            f.spec.ttl_seconds = 1;
            if stage == "begin" {
                f.request().await;
            }
            let kind = match (stage, table) {
                ("request", "th_operations") => "github.queue.request.v1",
                ("request", _) => "github.queue.requested",
                (_, "th_operations") => "github.queue.begin.v1",
                _ => "github.queue.dispatched",
            };
            let raw = connection(&f.p.f.schema).await;
            raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_queue() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_queue AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_queue();")).await.unwrap();
            let result = if stage == "request" {
                f.p.f
                    .db
                    .request_github_queue(&f.owner, &cmd("queue-request"), f.id, &f.spec)
                    .await
            } else {
                f.p.f
                    .db
                    .begin_github_queue(&f.bridge, &cmd("queue-begin"), f.id, Uuid::new_v4())
                    .await
            };
            assert!(result.is_err());
            assert!(
                raw.query_one("SELECT is_called FROM delay_seen", &[])
                    .await
                    .unwrap()
                    .get::<_, bool>(0)
            );
            if stage == "request" {
                assert_eq!(
                    raw.query_one("SELECT count(*) FROM th_github_queue", &[])
                        .await
                        .unwrap()
                        .get::<_, i64>(0),
                    0
                );
            } else {
                assert!(
                    raw.query_one("SELECT attempt IS NULL FROM th_github_queue", &[])
                        .await
                        .unwrap()
                        .get::<_, bool>(0)
                );
            }
            cleanup(&f.p.f.schema).await;
        }
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL; populated schema20 upgrade without silently expanding existing grants"]
async fn queue_upgrade_preserves_inspections_and_requires_new_explicit_capabilities() {
    use sha2::{Digest, Sha256};
    let mut p = Projection::new().await;
    let (inspection, _, v) = inspections::requested(&mut p).await;
    let r = inspections::report(&p, &v).await;
    let before =
        p.f.db
            .report_github_inspection(&p.reader, &cmd("report-before-upgrade"), inspection, &r)
            .await
            .unwrap();
    let raw = connection(&p.f.schema).await;
    drop_unpopulated_v21(&raw).await;
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(20) {
        let mut h = Sha256::new();
        h.update(digest);
        h.update(std::fs::read(path).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=20,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    p.f.db.migrate().await.unwrap();
    p.f.db.check_schema().await.unwrap();
    assert_eq!(
        before,
        p.f.db
            .report_github_inspection(&p.reader, &cmd("report-before-upgrade"), inspection, &r)
            .await
            .unwrap()
    );
    assert!(
        p.f.db
            .issue_repository_token(&p.f.scope, "human", &[P::HostQueue], 60, "operator")
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .issue_repository_token(&p.f.scope, "bridge", &[P::HostMerge], 60, "operator")
            .await
            .is_err()
    );
    let owner = actor(
        &mut p.f,
        "human",
        K::Human,
        &[P::HostQueue, P::Read, P::RepositoryWrite],
    )
    .await;
    let v =
        p.f.db
            .request_github_queue(
                &owner,
                &cmd("after-upgrade"),
                Uuid::new_v4(),
                &QueueSpec {
                    inspection: inspection.to_string(),
                    ttl_seconds: 60,
                },
            )
            .await
            .unwrap();
    assert_eq!(v["target"]["pull"], before["target"]);
    assert_eq!(v["state"], "requested");
    assert_eq!(v["merge_verified"], false);
    cleanup(&p.f.schema).await;
}

fn pending_receipt(f: &QueueFixture, provider: Uuid) -> QueueReceipt {
    QueueReceipt {
        http_status: 202,
        status: "pending".into(),
        request_uuid: Some(provider.to_string()),
        expected_head_revision: Some(f.target.pull.head_revision.clone()),
        merge_action: Some("merge_queue".into()),
        bypass_rules: Some(false),
        final_revision: None,
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL; late UUID recovery, unavailable vs failed and explicit reauthorization"]
async fn queue_late_uuid_is_retained_and_only_confirmed_failure_allows_new_owner_intent() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let provider = Uuid::new_v4();
    let recent = f.report().await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("newer-observation"), f.id, a, &recent)
        .await
        .unwrap();
    let mut late = recent.clone();
    late.started_at_unix_ms -= 1000;
    late.finished_at_unix_ms -= 1000;
    late.receipt = Some(pending_receipt(&f, provider));
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("late-provider-uuid"), f.id, a, &late)
            .await
            .unwrap();
    assert_eq!(v["provider_request"], provider.to_string());
    assert_eq!(v["observation"], json!(recent));
    assert_eq!(v["recorded_observation"], json!(late));
    assert_eq!(v["report_sequence"], 2);
    let mut r = f.report().await;
    r.async_result = Some(QueueAsyncResult {
        request_uuid: Uuid::new_v4().to_string(),
        http_status: 404,
        outcome: QueueAsyncOutcome::Unavailable,
    });
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("foreign-result"), f.id, a, &r)
            .await
            .is_err()
    );
    r.async_result.as_mut().unwrap().request_uuid = provider.to_string();
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("unavailable-result"), f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["state"], "result_unavailable");
    assert_eq!(v["dispatch_failed"], false);
    f.p.f
        .db
        .cancel_github_queue(&f.owner, &cmd("cancel-pending"), f.id)
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .request_github_queue(
                &f.owner,
                &cmd("not-proof-of-failure"),
                Uuid::new_v4(),
                &f.spec
            )
            .await
            .is_err()
    );
    r = f.report().await;
    r.async_result = Some(QueueAsyncResult {
        request_uuid: provider.to_string(),
        http_status: 200,
        outcome: QueueAsyncOutcome::Failed,
    });
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("failed-result"), f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["dispatch_failed"], true);
    assert_eq!(v["state"], "request_failed");
    assert_eq!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("failed-result"), f.id, a, &r)
            .await
            .unwrap(),
        v
    );
    let empty = f.report().await;
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("unknown-after-failure"), f.id, a, &empty)
            .await
            .unwrap();
    assert_eq!(v["state"], "request_failed");
    assert_eq!(v["provider_result"]["outcome"]["status"], "failed");
    assert_eq!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("queue-begin"), f.id, a)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    assert!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("never-resend-old"), f.id, Uuid::new_v4())
            .await
            .is_err()
    );
    let new =
        f.p.f
            .db
            .request_github_queue(
                &f.owner,
                &cmd("explicit-new-intent"),
                Uuid::new_v4(),
                &f.spec,
            )
            .await
            .unwrap();
    assert_eq!(new["state"], "requested");
    assert!(new["attempt"].is_null());
    let raw = connection(&f.p.f.schema).await;
    assert!(
        raw.batch_execute("UPDATE th_github_queue SET dispatch_failed=false WHERE dispatch_failed")
            .await
            .is_err()
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_queue_reports", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        5
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; result UUID provenance and immutable terminal outcome"]
async fn queue_terminal_result_cannot_be_substituted_or_erased_by_expiry() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let provider = Uuid::new_v4();
    let mut r = f.report().await;
    r.async_result = Some(QueueAsyncResult {
        request_uuid: provider.to_string(),
        http_status: 200,
        outcome: QueueAsyncOutcome::Enqueued,
    });
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("unretained-uuid"), f.id, a, &r)
            .await
            .is_err()
    );
    r.async_result = None;
    r.receipt = Some(pending_receipt(&f, provider));
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("provider-uuid"), f.id, a, &r)
        .await
        .unwrap();
    r = f.report().await;
    r.async_result = Some(QueueAsyncResult {
        request_uuid: provider.to_string(),
        http_status: 200,
        outcome: QueueAsyncOutcome::Enqueued,
    });
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("enqueued"), f.id, a, &r)
        .await
        .unwrap();
    let retained = r.async_result.clone();
    r.async_result.as_mut().unwrap().outcome = QueueAsyncOutcome::Failed;
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("substitute-failed"), f.id, a, &r)
            .await
            .is_err()
    );
    r.async_result.as_mut().unwrap().outcome = QueueAsyncOutcome::Merged {
        final_revision: "git:sha1:".to_owned() + &"33".repeat(20),
    };
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("enqueue-is-not-final-merge"), f.id, a, &r)
            .await
            .is_err()
    );
    r = f.report().await;
    r.async_result = Some(QueueAsyncResult {
        request_uuid: provider.to_string(),
        http_status: 404,
        outcome: QueueAsyncOutcome::Unavailable,
    });
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("expired-result"), f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["provider_result"], json!(retained));
    assert_eq!(
        v["observation"]["async_result"]["outcome"]["status"],
        "unavailable"
    );
    assert_eq!(v["dispatch_failed"], false);
    assert_eq!(v["merge_verified"], false);
    assert!(
        f.p.f
            .db
            .request_github_queue(
                &f.owner,
                &cmd("no-requeue-after-expiry"),
                Uuid::new_v4(),
                &f.spec
            )
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}
