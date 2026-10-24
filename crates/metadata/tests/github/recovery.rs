use super::*;
use serde_json::Value;
use treeharbor_github_contract::deliveries::{
    DeliveryAttempt, DeliveryDetails, DeliveryPageReport, payload_digest,
};
use treeharbor_metadata::github::{RecoverySpec, RedeliveryOutcome, RedeliveryReport};
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
async fn enable(f: &mut Fixture) {
    f.db.set_repository_access(
        &f.scope,
        "bridge",
        &[P::Read, P::HostRead, P::HostRecover],
        "operator",
    )
    .await
    .unwrap();
    f.bridge =
        f.db.issue_repository_token(
            &f.scope,
            "bridge",
            &[P::Read, P::HostRead, P::HostRecover],
            3600,
            "operator",
        )
        .await
        .unwrap();
}
fn spec(channel: Uuid) -> RecoverySpec {
    RecoverySpec {
        channel: channel.to_string(),
        callback_url: "https://hooks.example.com/github/events".into(),
        since_unix_ms: now() - 60000,
        ttl_seconds: 120,
        max_pages: 3,
        max_deliveries: 3,
    }
}
async fn plan(f: &mut Fixture, spec: &RecoverySpec) -> Uuid {
    let id = Uuid::new_v4();
    let a = f.owner().await;
    f.db.request_github_recovery(&a, &cmd(&format!("plan-{id}")), id, spec)
        .await
        .unwrap();
    id
}
fn delivery(id: u64) -> DeliveryAttempt {
    DeliveryAttempt {
        id,
        guid: Uuid::new_v4(),
        installation_id: 9,
        repository_id: 11,
        delivered_at_unix_ms: now() - 1000,
        redelivery: false,
        action: "checks_requested".into(),
        status_code: Some(503),
        throttled_at_unix_ms: None,
    }
}
fn page(a: &[DeliveryAttempt], cursor: Option<&str>, next: Option<&str>) -> DeliveryPageReport {
    DeliveryPageReport {
        version: 1,
        observed_at_unix_ms: now(),
        cursor: cursor.map(str::to_owned),
        next_cursor: next.map(str::to_owned),
        scanned: a.len() as u16,
        attempts: a.to_vec(),
    }
}
fn details(a: &DeliveryAttempt, name: &str) -> DeliveryDetails {
    let signed = events::verified(name, 7);
    let body: Value = serde_json::from_slice(signed.body()).unwrap();
    DeliveryDetails {
        version: 1,
        attempt: a.clone(),
        observed_at_unix_ms: now(),
        payload_sha256: payload_digest(&body).unwrap(),
    }
}
async fn recoverer(f: &mut Fixture) -> Authorization {
    f.db.authorize(&f.bridge.secret, &f.scope, P::HostRecover)
        .await
        .unwrap()
}
async fn prepare(f: &mut Fixture, id: Uuid, a: &DeliveryAttempt, name: &str) -> DeliveryDetails {
    let auth = recoverer(f).await;
    f.db.record_github_recovery_page(
        &auth,
        &cmd("page"),
        id,
        &page(std::slice::from_ref(a), None, None),
    )
    .await
    .unwrap();
    let d = details(a, name);
    f.db.record_github_recovery_details(&auth, &cmd("details"), id, &d)
        .await
        .unwrap();
    d
}
async fn status(f: &mut Fixture, id: Uuid) -> Value {
    let a = f.reader().await;
    f.db.github_recovery_context(&a, 1, id, 0, false)
        .await
        .unwrap()
}
#[tokio::test]
#[ignore = "requires PostgreSQL: durable recovery phases and actual signed-inbox correlation"]
async fn lost_response_replay_and_202_never_replace_received_payload_evidence() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    enable(&mut f).await;
    let id = plan(&mut f, &spec(channel)).await;
    let a = delivery(700);
    let d = prepare(&mut f, id, &a, "checks").await;
    let auth = recoverer(&mut f).await;
    let dispatch = Uuid::new_v4();
    let first =
        f.db.begin_github_redelivery(&auth, &cmd("begin"), id, 700, dispatch)
            .await
            .unwrap();
    assert_eq!(first["dispatch_granted"], true);
    assert_eq!(first["receipt"]["details"], json!(d));
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let auth = recoverer(&mut f).await;
    let replay =
        f.db.begin_github_redelivery(&auth, &cmd("begin"), id, 700, dispatch)
            .await
            .unwrap();
    assert_eq!(replay["dispatch_granted"], false);
    assert_eq!(replay["receipt"], first["receipt"]);
    assert!(
        f.db.begin_github_redelivery(&auth, &cmd("different-begin"), id, 700, Uuid::new_v4())
            .await
            .is_err()
    );
    assert_eq!(
        status(&mut f, id).await["deliveries"][0]["inbox"]["state"],
        "missing"
    );
    let report = RedeliveryReport {
        version: 1,
        observed_at_unix_ms: now(),
        outcome: RedeliveryOutcome::Accepted,
    };
    let receipt =
        f.db.report_github_redelivery(&auth, &cmd("accepted"), id, 700, dispatch, &report)
            .await
            .unwrap();
    assert_eq!(receipt["inbox"]["state"], "missing");
    assert_eq!(
        receipt,
        f.db.report_github_redelivery(&auth, &cmd("accepted"), id, 700, dispatch, &report)
            .await
            .unwrap()
    );
    let actual =
        f.db.accept_github_event(a.guid, &events::verified("checks", 7))
            .await
            .unwrap();
    let s = status(&mut f, id).await;
    assert_eq!(s["deliveries"][0]["inbox"]["state"], "received");
    assert_eq!(s["deliveries"][0]["inbox"]["event"], json!(actual.event));
    assert_eq!(s["coverage_complete"], false);
    assert_eq!(s["merge_verified"], false);
    let raw = connection(&f.schema).await;
    let counts=raw.query_one("SELECT (SELECT count(*) FROM th_github_event_recovery_attempts),(SELECT count(*) FROM th_outbox WHERE kind='github.recovery.dispatched'),(SELECT count(*) FROM th_native_refs)",&[]).await.unwrap();
    assert_eq!(
        (
            counts.get::<_, i64>(0),
            counts.get::<_, i64>(1),
            counts.get::<_, i64>(2)
        ),
        (1, 1, 0)
    );
    assert!(
        raw.execute(
            "UPDATE th_github_event_recovery_attempts SET details='{}'",
            &[]
        )
        .await
        .is_err()
    );
    assert!(
        raw.execute("DELETE FROM th_github_event_recovery_pages", &[])
            .await
            .is_err()
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: unsigned delivery header cannot misattribute an old signed body"]
async fn spoofed_delivery_guid_with_different_signed_content_never_confirms_recovery() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    enable(&mut f).await;
    let id = plan(&mut f, &spec(channel)).await;
    let mut a = delivery(701);
    a.action = "destroyed".into();
    prepare(&mut f, id, &a, "destroyed-merged").await;
    // This body is authentic, but the delivery header itself is not signed.
    f.db.accept_github_event(a.guid, &events::verified("checks", 7))
        .await
        .unwrap();
    let s = status(&mut f, id).await;
    assert_eq!(s["deliveries"][0]["inbox"]["state"], "payload_conflict");
    let auth = recoverer(&mut f).await;
    assert!(
        f.db.begin_github_redelivery(&auth, &cmd("blocked"), id, 701, Uuid::new_v4())
            .await
            .is_err()
    );
    assert_eq!(status(&mut f, id).await["dispatch_count"], 0);
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: overlapping pages, changed status, cursor cycles and scope bounds"]
async fn overlapping_pages_keep_one_attempt_and_fail_closed_on_cursor_cycles_and_substitution() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    enable(&mut f).await;
    let id = plan(&mut f, &spec(channel)).await;
    let auth = recoverer(&mut f).await;
    let a = delivery(702);
    let first = page(std::slice::from_ref(&a), None, Some("a"));
    f.db.record_github_recovery_page(&auth, &cmd("one"), id, &first)
        .await
        .unwrap();
    let mut b = a.clone();
    b.status_code = Some(200);
    let second = page(&[b.clone()], Some("a"), Some("b"));
    f.db.record_github_recovery_page(&auth, &cmd("two"), id, &second)
        .await
        .unwrap();
    assert!(
        f.db.record_github_recovery_page(&auth, &cmd("loop"), id, &page(&[], Some("b"), Some("a")))
            .await
            .is_err()
    );
    b.guid = Uuid::new_v4();
    assert!(
        f.db.record_github_recovery_page(&auth, &cmd("swapped"), id, &page(&[b], Some("b"), None))
            .await
            .is_err()
    );
    let mut foreign = a.clone();
    foreign.repository_id = 12;
    assert!(
        f.db.record_github_recovery_page(
            &auth,
            &cmd("foreign"),
            id,
            &page(&[foreign], Some("b"), None)
        )
        .await
        .is_err()
    );
    let mut future = page(&[], Some("b"), None);
    future.observed_at_unix_ms = now() + 60000;
    assert!(
        f.db.record_github_recovery_page(&auth, &cmd("future"), id, &future)
            .await
            .is_err()
    );
    f.db.record_github_recovery_page(&auth, &cmd("end"), id, &page(&[], Some("b"), None))
        .await
        .unwrap();
    let s = status(&mut f, id).await;
    assert_eq!(s["page_count"], 3);
    assert_eq!(s["scanned_count"], 2);
    assert_eq!(s["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(s["scan_exhausted"], true);
    assert_eq!(s["coverage_complete"], false);
    assert!(
        f.db.record_github_recovery_page(&auth, &cmd("restart-list"), id, &first)
            .await
            .is_err()
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: explicit role, channel revocation and epoch fencing"]
async fn recovery_needs_explicit_bridge_permission_and_current_channel_epoch() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    let id = plan(&mut f, &spec(channel)).await;
    let old = f.bridge().await;
    assert!(matches!(
        f.db.github_recovery_context(&old, 1, id, 0, true).await,
        Err(Error::Forbidden)
    ));
    enable(&mut f).await;
    let auth = recoverer(&mut f).await;
    let a = delivery(703);
    f.db.record_github_recovery_page(
        &auth,
        &cmd("page"),
        id,
        &page(std::slice::from_ref(&a), None, None),
    )
    .await
    .unwrap();
    let owner = f.owner().await;
    assert!(matches!(
        f.db.record_github_recovery_details(
            &owner,
            &cmd("owner-is-not-bridge"),
            id,
            &details(&a, "checks")
        )
        .await,
        Err(Error::Forbidden)
    ));
    f.db.revoke_github_event_channel(&owner, &cmd("revoke"), channel)
        .await
        .unwrap();
    assert!(
        f.db.record_github_recovery_details(
            &auth,
            &cmd("revoked-channel"),
            id,
            &details(&a, "checks")
        )
        .await
        .is_err()
    );
    assert!(status(&mut f, id).await["deliveries"][0]["details"].is_null());
    connection(&f.schema)
        .await
        .execute(
            "UPDATE th_repositories SET epoch=epoch+1 WHERE tenant=$1 AND repository=$2",
            &[&f.scope.tenant(), &f.scope.repository()],
        )
        .await
        .unwrap();
    assert!(matches!(
        f.db.github_recovery_context(&auth, 1, id, 0, true).await,
        Err(Error::Fenced)
    ));
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: cancellation stops effects but retains late dispatch receipts"]
async fn cancellation_and_expiry_preserve_unknown_dispatch_and_allow_historical_receipts() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    enable(&mut f).await;
    let id = plan(&mut f, &spec(channel)).await;
    let a = delivery(704);
    prepare(&mut f, id, &a, "checks").await;
    let auth = recoverer(&mut f).await;
    let dispatch = Uuid::new_v4();
    f.db.begin_github_redelivery(&auth, &cmd("begin"), id, 704, dispatch)
        .await
        .unwrap();
    let owner = f.owner().await;
    f.db.cancel_github_recovery(&owner, &cmd("cancel"), id)
        .await
        .unwrap();
    assert!(
        f.db.github_redelivery_context(&auth, 1, id, 704, dispatch)
            .await
            .is_err()
    );
    let report = RedeliveryReport {
        version: 1,
        observed_at_unix_ms: now(),
        outcome: RedeliveryOutcome::Unknown,
    };
    f.db.report_github_redelivery(&auth, &cmd("late"), id, 704, dispatch, &report)
        .await
        .unwrap();
    let s = status(&mut f, id).await;
    assert_eq!(s["cancelled"], true);
    assert_eq!(s["deliveries"][0]["report"]["outcome"], "unknown");
    f.db.accept_github_event(a.guid, &events::verified("checks", 7))
        .await
        .unwrap();
    assert_eq!(
        status(&mut f, id).await["deliveries"][0]["inbox"]["state"],
        "received"
    );
    let mut short = spec(channel);
    short.ttl_seconds = 1;
    let short_id = plan(&mut f, &short).await;
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    assert!(
        f.db.record_github_recovery_page(
            &auth,
            &cmd("expired-page"),
            short_id,
            &page(&[], None, None)
        )
        .await
        .is_err()
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL: same GUID different attempts receive at most one dispatch grant"]
async fn delivery_and_page_budgets_and_concurrent_dispatch_are_durable() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    enable(&mut f).await;
    let mut cfg = spec(channel);
    cfg.max_pages = 1;
    cfg.max_deliveries = 2;
    let id = plan(&mut f, &cfg).await;
    let a = delivery(705);
    let mut b = a.clone();
    b.id = 706;
    b.redelivery = true;
    let auth = recoverer(&mut f).await;
    f.db.record_github_recovery_page(
        &auth,
        &cmd("page"),
        id,
        &page(&[a.clone(), b.clone(), delivery(707)], None, Some("more")),
    )
    .await
    .unwrap();
    assert!(
        f.db.record_github_recovery_page(
            &auth,
            &cmd("over-page-budget"),
            id,
            &page(&[], Some("more"), None)
        )
        .await
        .is_err()
    );
    f.db.record_github_recovery_details(&auth, &cmd("a"), id, &details(&a, "checks"))
        .await
        .unwrap();
    f.db.record_github_recovery_details(&auth, &cmd("b"), id, &details(&b, "checks"))
        .await
        .unwrap();
    let d = status(&mut f, id).await["deliveries"][2]["observation"].clone();
    assert!(
        f.db.record_github_recovery_details(
            &auth,
            &cmd("over-delivery-budget"),
            id,
            &details(&serde_json::from_value(d).unwrap(), "checks")
        )
        .await
        .is_err()
    );
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let ca = cmd("send-a");
    let cb = cmd("send-b");
    let (one, two) = tokio::join!(
        f.db.begin_github_redelivery(&auth, &ca, id, 705, Uuid::new_v4()),
        other.begin_github_redelivery(&auth, &cb, id, 706, Uuid::new_v4())
    );
    assert_eq!(usize::from(one.is_ok()) + usize::from(two.is_ok()), 1);
    let s = status(&mut f, id).await;
    assert_eq!(s["dispatch_count"], 1);
    assert_eq!(s["scan_exhausted"], false);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL: populated schema23 migration retains signed inbox and old grants"]
async fn populated_schema23_upgrade_preserves_inbox_without_granting_recovery() {
    use sha2::{Digest, Sha256};
    let mut f = Fixture::new().await;
    events::ready(&mut f).await;
    let delivery = Uuid::new_v4();
    let signed = events::verified("checks", 7);
    let before = f.db.accept_github_event(delivery, &signed).await.unwrap();
    let raw = connection(&f.schema).await;
    let permissions: Vec<String> = raw
        .query_one(
            "SELECT permissions FROM th_access_tokens WHERE principal='bridge'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    remove_recovery_schema(&raw).await;
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let mut paths: Vec<_> = std::fs::read_dir(directory)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .get(..3)
                .and_then(|s| s.parse::<usize>().ok())
                .is_some_and(|n| n <= 23)
        })
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 23);
    let mut digest = Vec::new();
    for path in paths {
        let mut h = Sha256::new();
        h.update(&digest);
        h.update(std::fs::read(path).unwrap());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=23,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    assert!(f.db.check_schema().await.is_err());
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        24
    );
    assert_eq!(
        raw.query_one(
            "SELECT permissions FROM th_access_tokens WHERE principal='bridge'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, Vec<String>>(0),
        permissions
    );
    assert!(
        f.db.authorize(&f.bridge.secret, &f.scope, P::HostRecover)
            .await
            .is_err()
    );
    assert_eq!(
        before,
        f.db.accept_github_event(delivery, &signed).await.unwrap()
    );
    assert_eq!(
        raw.query_one("SELECT body FROM th_github_events", &[])
            .await
            .unwrap()
            .get::<_, Vec<u8>>(0),
        signed.body()
    );
    enable(&mut f).await;
    recoverer(&mut f).await;
    cleanup(&f.schema).await;
}

// Reconstruct only disposable pre-24 fixtures; this is not a product downgrade.
pub(super) async fn remove_recovery_schema(raw: &tokio_postgres::Client) {
    raw.batch_execute("DROP TABLE th_github_event_recovery_pages,th_github_event_recovery_attempts,th_github_event_recoveries; DROP FUNCTION th_github_recovery_immutable(); DROP FUNCTION th_github_recovery_attempt_immutable();").await.unwrap();
    let old_constraints = include_str!("../../migrations/021_github_queue.sql")
        .lines()
        .take(4)
        .collect::<Vec<_>>()
        .join("\n");
    raw.batch_execute(&old_constraints).await.unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL: expiration during outbox commit rolls back detail and budget"]
async fn expiry_during_detail_completion_rolls_back_receipt_and_budget() {
    let mut f = Fixture::new().await;
    let channel = events::ready(&mut f).await;
    enable(&mut f).await;
    let mut cfg = spec(channel);
    cfg.ttl_seconds = 5;
    let id = plan(&mut f, &cfg).await;
    let auth = recoverer(&mut f).await;
    let a = delivery(708);
    f.db.record_github_recovery_page(
        &auth,
        &cmd("page"),
        id,
        &page(std::slice::from_ref(&a), None, None),
    )
    .await
    .unwrap();
    connection(&f.schema).await.batch_execute("CREATE FUNCTION delay_recovery_receipt() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.recovery.detail' THEN PERFORM pg_sleep(5.1); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_recovery BEFORE INSERT ON th_outbox FOR EACH ROW EXECUTE FUNCTION delay_recovery_receipt();").await.unwrap();
    let started = std::time::Instant::now();
    assert!(
        f.db.record_github_recovery_details(
            &auth,
            &cmd("expire-during-commit"),
            id,
            &details(&a, "checks")
        )
        .await
        .is_err()
    );
    assert!(started.elapsed() >= std::time::Duration::from_secs(5));
    let s = status(&mut f, id).await;
    assert_eq!(s["detail_count"], 0);
    assert!(s["deliveries"][0]["details"].is_null());
    assert_eq!(
        connection(&f.schema)
            .await
            .query_one(
                "SELECT count(*) FROM th_operations WHERE key='expire-during-commit'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&f.schema).await;
}
