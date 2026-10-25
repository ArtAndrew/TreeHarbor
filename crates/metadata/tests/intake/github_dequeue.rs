use super::*;
use treeharbor_github_contract::dequeue::*;
use treeharbor_metadata::github::DequeueSpec;

fn spec(f: &QueueFixture) -> DequeueSpec {
    DequeueSpec {
        queue: f.id.to_string(),
        ttl_seconds: 60,
        scope: "pull_request".into(),
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; repeated real-shaped enqueue/removal cycles retain old fences"]
async fn dequeue_successor_is_not_blocked_by_a_settled_predecessor() {
    for no_send in [false, true] {
        let mut f = QueueFixture::new().await;
        let mut predecessor = None;
        for cycle in 0..3 {
            f.id = Uuid::new_v4();
            let key = |step: &str| cmd(&format!("cycle-{cycle}-{step}"));
            f.p.f
                .db
                .request_github_queue(&f.owner, &key("request"), f.id, &f.spec)
                .await
                .unwrap();
            let enqueue = Uuid::new_v4();
            f.p.f
                .db
                .begin_github_queue(&f.bridge, &key("begin"), f.id, enqueue)
                .await
                .unwrap();
            let mut sent = f.report().await;
            sent.version = 3;
            sent.enqueue_receipt = Some(EnqueueReceipt {
                request_id: enqueue.to_string(),
                repository_id: f.target.pull.repository_id,
                pull_node: f.target.pull.pull_node.clone(),
                pull_number: f.target.pull.pull_number,
                queue_id: "MQ_one".into(),
                entry_id: "MQE_one".into(),
                head_revision: f.target.pull.head_revision.clone(),
            });
            f.p.f
                .db
                .report_github_queue(&f.bridge, &key("sent"), f.id, enqueue, &sent)
                .await
                .unwrap();
            if let Some(old) = predecessor {
                let old_spec = DequeueSpec {
                    queue: old,
                    ttl_seconds: 60,
                    scope: "pull_request".into(),
                };
                // A new removal of the predecessor must not remove this live successor.
                assert!(
                    f.p.f
                        .db
                        .request_github_dequeue(
                            &f.owner,
                            &key("old-remove"),
                            Uuid::new_v4(),
                            &old_spec
                        )
                        .await
                        .is_err()
                );
            }
            let id = Uuid::new_v4();
            let removal = spec(&f);
            let requested =
                f.p.f
                    .db
                    .request_github_dequeue(&f.owner, &key("remove"), id, &removal)
                    .await
                    .unwrap();
            assert_eq!(requested["enqueue_settled"], true);
            let attempt = Uuid::new_v4();
            assert_eq!(
                f.p.f
                    .db
                    .begin_github_dequeue(&f.bridge, &key("remove-begin"), id, attempt)
                    .await
                    .unwrap()["dispatch_granted"],
                true
            );
            let mut absent = report(&f).await;
            absent.after = Some(snapshot(&f).await);
            f.p.f
                .db
                .report_github_dequeue(&f.bridge, &key("absent"), id, attempt, &absent)
                .await
                .unwrap();
            // Absence alone cannot release an unknown PR-scoped remote mutation.
            assert!(
                f.p.f
                    .db
                    .request_github_queue(
                        &f.owner,
                        &key("unknown-enqueue"),
                        Uuid::new_v4(),
                        &f.spec
                    )
                    .await
                    .is_err()
            );
            assert!(
                f.p.f
                    .db
                    .request_github_dequeue(
                        &f.owner,
                        &key("unknown-remove"),
                        Uuid::new_v4(),
                        &removal
                    )
                    .await
                    .is_err()
            );
            let mut settled = report(&f).await;
            settled.after = Some(snapshot(&f).await);
            if no_send {
                settled.mutation_not_sent = true;
            } else {
                settled.before = Some(queued(snapshot(&f).await));
                settled.receipt = Some(receipt(&f, id));
            }
            let result =
                f.p.f
                    .db
                    .report_github_dequeue(&f.bridge, &key("settled"), id, attempt, &settled)
                    .await
                    .unwrap();
            assert_eq!(result["state"], "removal_observed");
            assert_eq!(
                result,
                f.p.f
                    .db
                    .report_github_dequeue(&f.bridge, &key("settled"), id, attempt, &settled)
                    .await
                    .unwrap()
            );
            assert!(!result["merge_verified"].as_bool().unwrap());
            predecessor = Some(f.id.to_string());
        }
        cleanup(&f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; positive no-send recovery, conflicting receipts and cross-intent dispatch fences"]
async fn dequeue_no_send_recovery_unblocks_cancel_but_never_overlaps_a_new_enqueue() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let id = request(&mut f).await;
    assert!(
        f.p.f
            .db
            .begin_github_dequeue(&f.bridge, &cmd("blocked"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    let mut r = f.report().await;
    r.version = 2;
    r.mutation_not_sent = true;
    let v =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("queue-not-sent"), f.id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["mutation_not_sent"], true);
    assert_eq!(v["state"], "not_sent");
    let mut conflicting = f.report().await;
    conflicting.receipt = Some(pending_receipt(&f, Uuid::new_v4()));
    assert!(
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("contradict-no-send"), f.id, a, &conflicting)
            .await
            .is_err()
    );
    // A new enqueue must wait until the explicit pending removal is resolved.
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("new-queue-blocked"), Uuid::new_v4(), &f.spec)
            .await
            .is_err()
    );
    let d = begin(&mut f, id).await;
    let mut report = report(&f).await;
    report.mutation_not_sent = true;
    f.p.f
        .db
        .report_github_dequeue(&f.bridge, &cmd("dequeue-not-sent"), id, d, &report)
        .await
        .unwrap();
    f.p.f
        .db
        .request_github_queue(
            &f.owner,
            &cmd("new-queue-authorized"),
            Uuid::new_v4(),
            &f.spec,
        )
        .await
        .unwrap();
    // The old PR-scoped removal cannot silently race this newer owner intent.
    assert!(
        f.p.f
            .db
            .request_github_dequeue(
                &f.owner,
                &cmd("old-queue-removal"),
                Uuid::new_v4(),
                &spec(&f)
            )
            .await
            .is_err()
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; async merged receipt survives reads and may only be enriched with a matching tree"]
async fn dequeue_keeps_partial_merge_evidence_until_exact_tree_is_observed() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let provider = Uuid::new_v4();
    let mut r = f.report().await;
    r.receipt = Some(pending_receipt(&f, provider));
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("pending"), f.id, a, &r)
        .await
        .unwrap();
    r = f.report().await;
    let revision = "git:sha1:".to_owned() + &"55".repeat(20);
    r.async_result = Some(QueueAsyncResult {
        request_uuid: provider.to_string(),
        http_status: 200,
        outcome: QueueAsyncOutcome::Merged {
            final_revision: revision.clone(),
        },
    });
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("merged-result"), f.id, a, &r)
        .await
        .unwrap();
    let id = request(&mut f).await;
    let v =
        f.p.f
            .db
            .github_dequeue_context(&f.bridge, 1, id, true, None)
            .await
            .unwrap();
    assert_eq!(v["state"], "host_merged_unverified");
    assert_eq!(v["retained_merge"]["commit"]["revision"], revision);
    assert!(v["retained_merge"]["commit"]["tree"].is_null());
    let a = begin(&mut f, id).await;
    let mut r = report(&f).await;
    let mut s = snapshot(&f).await;
    s.pull.merged = true;
    s.pull.state = "closed".into();
    s.final_commit = Some(GitCommitObservation {
        revision,
        tree: "git:sha1:".to_owned() + &"66".repeat(20),
    });
    r.after = Some(s.clone());
    let v =
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("enrich-tree"), id, a, &r)
            .await
            .unwrap();
    assert_eq!(v["retained_merge"]["commit"], json!(s.final_commit));
    assert_eq!(v["merge_verified"], false);
    cleanup(&f.p.f.schema).await;
}
async fn request(f: &mut QueueFixture) -> Uuid {
    let id = Uuid::new_v4();
    f.p.f
        .db
        .request_github_dequeue(&f.owner, &cmd("dequeue-request"), id, &spec(f))
        .await
        .unwrap();
    id
}
async fn begin(f: &mut QueueFixture, id: Uuid) -> Uuid {
    let attempt = Uuid::new_v4();
    assert_eq!(
        f.p.f
            .db
            .begin_github_dequeue(&f.bridge, &cmd("dequeue-begin"), id, attempt)
            .await
            .unwrap()["dispatch_granted"],
        true
    );
    attempt
}
async fn report(f: &QueueFixture) -> DequeueObservation {
    let q = f.report().await;
    DequeueObservation {
        version: 1,
        started_at_unix_ms: q.started_at_unix_ms,
        finished_at_unix_ms: q.finished_at_unix_ms,
        before: None,
        receipt: None,
        after: None,
        mutation_not_sent: false,
        token_revoked: false,
    }
}
async fn snapshot(f: &QueueFixture) -> QueueSnapshot {
    let capture: Value = connection(&f.p.f.schema)
        .await
        .query_one(
            "SELECT report FROM th_github_inspections WHERE inspection=$1",
            &[&f.inspection],
        )
        .await
        .unwrap()
        .get(0);
    QueueSnapshot {
        queue_id: None,
        pull: serde_json::from_value(capture["before"].clone()).unwrap(),
        queue_enabled: true,
        configuration: None,
        entry: None,
        final_commit: None,
    }
}
fn queued(mut s: QueueSnapshot) -> QueueSnapshot {
    s.configuration = Some(QueueConfiguration {
        id: "MQ_one".into(),
        merge_method: "SQUASH".into(),
        merging_strategy: "ALLGREEN".into(),
        maximum_entries_to_build: 5,
        maximum_entries_to_merge: 5,
        minimum_entries_to_merge: 1,
        minimum_entries_wait_minutes: 0,
        check_timeout_minutes: 60,
    });
    s.entry = Some(QueueEntryObservation {
        id: "MQE_one".into(),
        state: "QUEUED".into(),
        base: None,
        head: None,
    });
    s
}
fn receipt(f: &QueueFixture, id: Uuid) -> DequeueReceipt {
    DequeueReceipt {
        request_id: id.to_string(),
        repository_id: f.target.pull.repository_id,
        pull_node: f.target.pull.pull_node.clone(),
        pull_number: f.target.pull.pull_number,
        queue_id: "MQ_one".into(),
        entry_id: "MQE_one".into(),
        head_revision: f.target.pull.head_revision.clone(),
    }
}
pub(super) async fn drop_unpopulated_v22(raw: &tokio_postgres::Client) {
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_queue_policies)+(SELECT count(*) FROM th_github_policy_refs)+(SELECT count(*) FROM th_github_check_policies)",&[]).await.unwrap().get::<_,i64>(0),0);
    crate::common::drop_unpopulated_v26(raw).await;
    raw.batch_execute("DROP TABLE th_github_queue_policies,th_github_policy_refs,th_github_check_policies; DROP FUNCTION th_github_policy_ref_update();").await.unwrap();
    // Historical fixtures predate event recovery. Remove only their empty
    // additive tables before reconstructing the older schema under test.
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_event_recoveries", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    raw.batch_execute("DROP TABLE th_github_event_recovery_pages,th_github_event_recovery_attempts,th_github_event_recoveries; DROP FUNCTION th_github_recovery_immutable(); DROP FUNCTION th_github_recovery_attempt_immutable();").await.unwrap();
    let constraints = include_str!("../../migrations/021_github_queue.sql")
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    raw.batch_execute(&constraints).await.unwrap();
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_dequeues)+(SELECT count(*) FROM th_github_dequeue_reports)",&[]).await.unwrap().get::<_,i64>(0),0);
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_events)+(SELECT count(*) FROM th_github_event_channels)",&[]).await.unwrap().get::<_,i64>(0),0);
    raw.batch_execute("DROP TABLE th_github_event_deliveries; DROP TABLE th_github_events; DROP TABLE th_github_event_channels; DROP FUNCTION th_github_event_channel_immutable()").await.unwrap();
    raw.batch_execute("DROP TABLE th_github_dequeue_reports; DROP TABLE th_github_dequeues; DROP FUNCTION th_github_dequeue_immutable(); DROP TRIGGER th_github_queue_no_send_update ON th_github_queue; DROP FUNCTION th_github_queue_no_send_immutable(); ALTER TABLE th_github_queue DROP COLUMN mutation_not_sent;").await.unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL; explicit owner intent, atomic local cancellation and one-shot concurrency"]
async fn dequeue_authorization_is_explicit_and_only_one_worker_receives_dispatch() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let id = Uuid::new_v4();
    assert!(
        f.p.f
            .db
            .request_github_dequeue(&f.p.owner, &cmd("unauthorized"), id, &spec(&f))
            .await
            .is_err()
    );
    let mut wrong = spec(&f);
    wrong.scope = "exact_head".into();
    assert!(
        f.p.f
            .db
            .request_github_dequeue(&f.owner, &cmd("wrong-scope"), id, &wrong)
            .await
            .is_err()
    );
    let s = spec(&f);
    let v =
        f.p.f
            .db
            .request_github_dequeue(&f.owner, &cmd("dequeue"), id, &s)
            .await
            .unwrap();
    assert_eq!(v["enqueue_settled"], true);
    assert_eq!(v["scope"], "pull_request");
    assert!(v.get("unexpired").is_none());
    assert_eq!(
        v,
        f.p.f
            .db
            .request_github_dequeue(&f.owner, &cmd("dequeue"), id, &s)
            .await
            .unwrap()
    );
    assert!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("too-late-enqueue"), f.id, Uuid::new_v4())
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .request_github_dequeue(&f.owner, &cmd("overlap"), Uuid::new_v4(), &s)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .begin_github_dequeue(&f.p.reader, &cmd("read-cannot-remove"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    let mut other = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    let attempt = Uuid::new_v4();
    let c = cmd("race-dequeue");
    let (a, b) = tokio::join!(
        f.p.f.db.begin_github_dequeue(&f.bridge, &c, id, attempt),
        other.begin_github_dequeue(&f.bridge, &c, id, attempt)
    );
    assert_ne!(
        a.unwrap()["dispatch_granted"],
        b.unwrap()["dispatch_granted"]
    );
    assert_eq!(
        f.p.f
            .db
            .begin_github_dequeue(&f.bridge, &c, id, attempt)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    assert!(
        f.p.f
            .db
            .begin_github_dequeue(&f.bridge, &cmd("another-attempt"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    let r = report(&f).await;
    let v =
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("report"), id, attempt, &r)
            .await
            .unwrap();
    assert_eq!(v["state"], "uncertain");
    assert_eq!(
        v,
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("report"), id, attempt, &r)
            .await
            .unwrap()
    );
    assert!(
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("wrong-attempt"), id, Uuid::new_v4(), &r)
            .await
            .is_err()
    );
    let raw = connection(&f.p.f.schema).await;
    for sql in [
        "UPDATE th_github_dequeues SET queue=gen_random_uuid()",
        "UPDATE th_github_dequeues SET attempt=NULL,started_at=NULL",
        "DELETE FROM th_github_dequeue_reports",
        "UPDATE th_github_dequeue_reports SET report='{}'",
    ] {
        assert!(raw.batch_execute(sql).await.is_err(), "{sql}");
    }
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; pending/unknown enqueue never becomes cancellation proof"]
async fn dequeue_waits_for_terminal_enqueue_not_absence_or_expiry() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let provider = Uuid::new_v4();
    let mut q = f.report().await;
    q.receipt = Some(pending_receipt(&f, provider));
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("pending"), f.id, a, &q)
        .await
        .unwrap();
    let id = request(&mut f).await;
    let attempt = Uuid::new_v4();
    for unavailable in [false, true] {
        q = f.report().await;
        q.snapshot = Some(snapshot(&f).await);
        if unavailable {
            q.async_result = Some(QueueAsyncResult {
                request_uuid: provider.to_string(),
                http_status: 404,
                outcome: QueueAsyncOutcome::Unavailable,
            });
        }
        f.p.f
            .db
            .report_github_queue(
                &f.bridge,
                &cmd(&format!("absent-{unavailable}")),
                f.id,
                a,
                &q,
            )
            .await
            .unwrap();
        let c =
            f.p.f
                .db
                .github_dequeue_context(&f.bridge, 1, id, true, None)
                .await
                .unwrap();
        assert_eq!(c["enqueue_settled"], false);
        assert_eq!(c["state"], "enqueue_unresolved");
        assert!(
            f.p.f
                .db
                .begin_github_dequeue(&f.bridge, &cmd("blocked-begin"), id, attempt)
                .await
                .is_err()
        );
    }
    q = f.report().await;
    q.async_result = Some(QueueAsyncResult {
        request_uuid: provider.to_string(),
        http_status: 200,
        outcome: QueueAsyncOutcome::Enqueued,
    });
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("terminal-enqueued"), f.id, a, &q)
        .await
        .unwrap();
    let attempt = begin(&mut f, id).await;
    let mut r = report(&f).await;
    r.after = Some(snapshot(&f).await);
    let v =
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("absent-after-terminal"), id, attempt, &r)
            .await
            .unwrap();
    assert_eq!(v["state"], "removal_observed");
    assert_eq!(v["merge_verified"], false);
    // Absence after an unknown mutation does not prove that mutation is finished.
    assert!(
        f.p.f
            .db
            .request_github_dequeue(&f.owner, &cmd("unknown-overlap"), Uuid::new_v4(), &spec(&f))
            .await
            .is_err()
    );
    r = report(&f).await;
    r.after = Some(queued(snapshot(&f).await));
    assert_eq!(
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("external-requeue"), id, attempt, &r)
            .await
            .unwrap()["state"],
        "host_queued"
    );
    // A settled, acknowledged removal plus later absence permits a new explicit
    // enqueue. This cannot replay either of the original mutations.
    r = report(&f).await;
    r.before = Some(queued(snapshot(&f).await));
    r.receipt = Some(receipt(&f, id));
    f.p.f
        .db
        .report_github_dequeue(&f.bridge, &cmd("acknowledged-removal"), id, attempt, &r)
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("still-queued"), Uuid::new_v4(), &f.spec)
            .await
            .is_err()
    );
    r = report(&f).await;
    r.after = Some(snapshot(&f).await);
    f.p.f
        .db
        .report_github_dequeue(&f.bridge, &cmd("final-absence"), id, attempt, &r)
        .await
        .unwrap();
    f.p.f
        .db
        .request_github_queue(
            &f.owner,
            &cmd("explicit-reenqueue"),
            Uuid::new_v4(),
            &f.spec,
        )
        .await
        .unwrap();
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; delayed receipt and explicit no-send evidence preserve safe reauthorization"]
async fn dequeue_retains_late_receipt_and_never_infers_no_send_from_absence() {
    for no_send in [false, true] {
        let mut f = QueueFixture::new().await;
        f.request().await;
        let id = request(&mut f).await;
        let attempt = begin(&mut f, id).await;
        let mut r = report(&f).await;
        r.after = Some(snapshot(&f).await);
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("recent-absence"), id, attempt, &r)
            .await
            .unwrap();
        let mut late = r.clone();
        late.started_at_unix_ms -= 1000;
        late.finished_at_unix_ms -= 1000;
        late.after = None;
        if no_send {
            late.mutation_not_sent = true;
        } else {
            late.before = Some(queued(snapshot(&f).await));
            late.receipt = Some(receipt(&f, id));
        }
        let v =
            f.p.f
                .db
                .report_github_dequeue(&f.bridge, &cmd("late-receipt"), id, attempt, &late)
                .await
                .unwrap();
        assert_eq!(v["observation"], json!(r));
        assert_eq!(v["recorded_observation"], json!(late));
        assert_eq!(v["mutation_not_sent"], no_send);
        assert_eq!(v["retained_receipt"], json!(late.receipt));
        assert!(
            f.p.f
                .db
                .github_dequeue_context(&f.bridge, 1, id, true, Some(attempt))
                .await
                .is_err()
        );
        let mut conflicting = report(&f).await;
        if no_send {
            conflicting.before = Some(queued(snapshot(&f).await));
            conflicting.receipt = Some(receipt(&f, id));
        } else {
            conflicting.mutation_not_sent = true;
        }
        assert!(
            f.p.f
                .db
                .report_github_dequeue(&f.bridge, &cmd("contradiction"), id, attempt, &conflicting)
                .await
                .is_err()
        );
        f.p.f
            .db
            .request_github_dequeue(
                &f.owner,
                &cmd("new-explicit-intent"),
                Uuid::new_v4(),
                &spec(&f),
            )
            .await
            .unwrap();
        cleanup(&f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; source drift permits removal but authority/credential/deadline fences remain"]
async fn dequeue_fences_writes_but_accepts_historical_recovery_under_current_authority() {
    for fence in [
        "source",
        "binding",
        "grant",
        "epoch",
        "expiry",
        "unassigned",
    ] {
        let mut f = QueueFixture::new().await;
        f.request().await;
        let id = request(&mut f).await;
        let a = begin(&mut f, id).await;
        let r = report(&f).await;
        let mut bridge = f.bridge.clone();
        match fence {
            "source" => f.p.new_version().await,
            "binding" => {
                f.p.f
                    .db
                    .revoke_github(&f.p.owner, &cmd("revoke-binding"), f.p.binding)
                    .await
                    .unwrap();
            }
            "grant" => {
                f.p.f
                    .db
                    .set_repository_access(&f.p.f.scope, "bridge", &[P::HostRead], "operator")
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
            "expiry" => {
                connection(&f.p.f.schema).await.batch_execute("ALTER TABLE th_github_dequeues DISABLE TRIGGER th_github_dequeue_update; UPDATE th_github_dequeues SET created_at=clock_timestamp()-interval '2 hours',expires_at=clock_timestamp()-interval '1 hour'; ALTER TABLE th_github_dequeues ENABLE TRIGGER th_github_dequeue_update").await.unwrap();
            }
            _ => bridge = actor(&mut f.p.f, "other-bridge", K::Bridge, &[P::HostMerge]).await,
        }
        assert_eq!(
            f.p.f
                .db
                .github_dequeue_context(&bridge, 1, id, true, Some(a))
                .await
                .is_ok(),
            fence == "source",
            "{fence}"
        );
        assert_eq!(
            f.p.f
                .db
                .report_github_dequeue(&bridge, &cmd("historical"), id, a, &r)
                .await
                .is_ok(),
            !matches!(fence, "grant" | "epoch" | "unassigned"),
            "{fence}"
        );
        cleanup(&f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; merge racing removal retains exact revision/tree and never becomes cancelled"]
async fn dequeue_retains_merge_facts_through_missing_reads_and_rejects_conflicting_trees() {
    for other_head in [false, true] {
        let mut f = QueueFixture::new().await;
        f.request().await;
        let id = request(&mut f).await;
        let a = begin(&mut f, id).await;
        let mut s = snapshot(&f).await;
        s.pull.merged = true;
        s.pull.state = "closed".into();
        if other_head {
            s.pull.head_revision = "git:sha1:".to_owned() + &"77".repeat(20);
        }
        s.final_commit = Some(GitCommitObservation {
            revision: "git:sha1:".to_owned() + &"55".repeat(20),
            tree: "git:sha1:".to_owned() + &"66".repeat(20),
        });
        let mut r = report(&f).await;
        r.after = Some(s.clone());
        f.p.f
            .db
            .report_github_dequeue(&f.bridge, &cmd("merged"), id, a, &r)
            .await
            .unwrap();
        r = report(&f).await;
        let v =
            f.p.f
                .db
                .report_github_dequeue(&f.bridge, &cmd("missing"), id, a, &r)
                .await
                .unwrap();
        assert_eq!(
            v["state"],
            if other_head {
                "other_head_merged"
            } else {
                "host_merged_unverified"
            }
        );
        assert_eq!(v["retained_merge"]["commit"], json!(s.final_commit));
        assert_eq!(v["merge_verified"], false);
        s.final_commit.as_mut().unwrap().tree = "git:sha1:".to_owned() + &"88".repeat(20);
        r.after = Some(s);
        assert!(
            f.p.f
                .db
                .report_github_dequeue(&f.bridge, &cmd("conflicting-tree"), id, a, &r)
                .await
                .is_err()
        );
        cleanup(&f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; deadlines checked after receipt/outbox insert, transaction rolls back"]
async fn dequeue_expiry_during_commit_rolls_back_owner_intent_or_dispatch() {
    for stage in ["request", "begin"] {
        for table in ["th_operations", "th_outbox"] {
            let mut f = QueueFixture::new().await;
            f.request().await;
            let id = Uuid::new_v4();
            let mut s = spec(&f);
            s.ttl_seconds = 1;
            if stage == "begin" {
                f.p.f
                    .db
                    .request_github_dequeue(&f.owner, &cmd("intent"), id, &s)
                    .await
                    .unwrap();
            }
            let kind = match (stage, table) {
                ("request", "th_operations") => "github.dequeue.request.v1",
                ("request", _) => "github.dequeue.requested",
                (_, "th_operations") => "github.dequeue.begin.v1",
                _ => "github.dequeue.dispatched",
            };
            let raw = connection(&f.p.f.schema).await;
            raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_dequeue() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_dequeue AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_dequeue();")).await.unwrap();
            let result = if stage == "request" {
                f.p.f
                    .db
                    .request_github_dequeue(&f.owner, &cmd("intent"), id, &s)
                    .await
            } else {
                f.p.f
                    .db
                    .begin_github_dequeue(&f.bridge, &cmd("begin"), id, Uuid::new_v4())
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
                    raw.query_one("SELECT count(*) FROM th_github_dequeues", &[])
                        .await
                        .unwrap()
                        .get::<_, i64>(0),
                    0
                );
                assert!(
                    !raw.query_one("SELECT cancel_requested FROM th_github_queue", &[])
                        .await
                        .unwrap()
                        .get::<_, bool>(0)
                );
            } else {
                assert!(
                    raw.query_one("SELECT attempt IS NULL FROM th_github_dequeues", &[])
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
#[ignore = "requires PostgreSQL; populated schema21 local cancellation never acquires remote authority"]
async fn dequeue_upgrade_preserves_local_cancel_without_inventing_remote_intent() {
    use sha2::{Digest, Sha256};
    let mut f = QueueFixture::new().await;
    f.request().await;
    let a = f.begin().await;
    let r = f.report().await;
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("old-report"), f.id, a, &r)
        .await
        .unwrap();
    let before =
        f.p.f
            .db
            .cancel_github_queue(&f.owner, &cmd("old-local-cancel"), f.id)
            .await
            .unwrap();
    let raw = connection(&f.p.f.schema).await;
    drop_unpopulated_v22(&raw).await;
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|s| s == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(21) {
        let mut h = Sha256::new();
        h.update(digest);
        h.update(std::fs::read(path).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=21,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    f.p.f.db.migrate().await.unwrap();
    f.p.f.db.check_schema().await.unwrap();
    assert_eq!(
        before,
        f.p.f
            .db
            .cancel_github_queue(&f.owner, &cmd("old-local-cancel"), f.id)
            .await
            .unwrap()
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_dequeues", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let id = request(&mut f).await;
    assert_eq!(
        f.p.f
            .db
            .github_dequeue_context(&f.bridge, 1, id, true, None)
            .await
            .unwrap()["state"],
        "enqueue_unresolved"
    );
    cleanup(&f.p.f.schema).await;
}
