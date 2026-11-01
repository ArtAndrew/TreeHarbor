mod common;
use common::{cleanup, connection, setup};
// Explicit integration suite. Requires a disposable PostgreSQL database.
// Run: TH_TEST_DATABASE_URL=... cargo test -p treeharbor-metadata -- --ignored
use serde_json::json;
use treeharbor_metadata::{Authority, Begin, Error, Event, Metadata, Request, Scope};
use uuid::Uuid;

fn request(scope: &Scope, key: &str, bytes: &[u8]) -> Request {
    Request::new(
        scope.clone(),
        key,
        "trusted-test-principal",
        "test-effect",
        Authority::Standalone,
        1,
        bytes,
    )
    .unwrap()
}
async fn effect(db: &mut Metadata, request: Request) -> serde_json::Value {
    match db.begin(request).await.unwrap() {
        Begin::Replay(value) => value,
        Begin::Fresh(tx) => {
            tx.transaction()
                .execute("INSERT INTO test_effects VALUES (1)", &[])
                .await
                .unwrap();
            tx.complete(
                json!({"accepted":"test-only"}),
                vec![Event {
                    kind: "test-notification".into(),
                    payload: json!({"id":"logical-effect"}),
                }],
            )
            .await
            .unwrap()
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn committed_response_replays_after_reconnect_without_duplicate_effect_or_outbox() {
    let (schema, scope, mut db) = setup().await;
    let req = request(&scope, "first", b"command-one");
    let first = effect(&mut db, req.clone()).await;
    drop(db); // Simulates losing the response/client state after commit.
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(effect(&mut db, req).await, first);
    let raw = connection(&schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM test_effects", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_outbox", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(matches!(
        db.begin(request(&scope, "first", b"changed")).await,
        Err(Error::IdempotencyConflict)
    ));
    let identity_change = Request::new(
        scope.clone(),
        "first",
        "other-principal",
        "test-effect",
        Authority::Standalone,
        1,
        b"command-one",
    )
    .unwrap();
    assert!(matches!(
        db.begin(identity_change).await,
        Err(Error::IdempotencyConflict)
    ));
    let method_change = Request::new(
        scope.clone(),
        "first",
        "trusted-test-principal",
        "different-method",
        Authority::Standalone,
        1,
        b"command-one",
    )
    .unwrap();
    assert!(matches!(
        db.begin(method_change).await,
        Err(Error::IdempotencyConflict)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn simultaneous_duplicate_requests_commit_once() {
    let (schema, scope, db) = setup().await;
    drop(db);
    let mut tasks = Vec::new();
    for _ in 0..12 {
        let schema = schema.clone();
        let req = request(&scope, "same", b"same-body");
        tasks.push(tokio::spawn(async move {
            let mut db = Metadata::new(connection(&schema).await).await.unwrap();
            effect(&mut db, req).await
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap(), json!({"accepted":"test-only"}));
    }
    let raw = connection(&schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM test_effects", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_operations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn backend_death_before_commit_rolls_back_all_effects_and_retry_succeeds() {
    let (schema, scope, mut db) = setup().await;
    let req = request(&scope, "crash", b"effect");
    let Begin::Fresh(tx) = db.begin(req.clone()).await.unwrap() else {
        panic!("unexpected replay")
    };
    tx.transaction()
        .execute("INSERT INTO test_effects VALUES (1)", &[])
        .await
        .unwrap();
    let pid: i32 = tx
        .transaction()
        .query_one("SELECT pg_backend_pid()", &[])
        .await
        .unwrap()
        .get(0);
    let admin = connection(&schema).await;
    let killed: bool = admin
        .query_one("SELECT pg_terminate_backend($1)", &[&pid])
        .await
        .unwrap()
        .get(0);
    assert!(killed);
    assert!(tx.complete(json!({"ok":true}), vec![]).await.is_err());
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    effect(&mut db, req).await;
    assert_eq!(
        admin
            .query_one("SELECT count(*) FROM test_effects", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn epoch_authority_freeze_and_tenant_boundaries_are_enforced() {
    let (schema, scope, mut db) = setup().await;
    let committed = request(&scope, "old-accepted", b"old");
    effect(&mut db, committed.clone()).await;
    let wrong = Scope::new("other-tenant", scope.repository()).unwrap();
    assert!(matches!(
        db.begin(request(&wrong, "new", b"x")).await,
        Err(Error::NotFound)
    ));
    let raw = connection(&schema).await;
    raw.execute(
        "UPDATE th_repositories SET state='frozen',epoch=2 WHERE tenant=$1 AND repository=$2",
        &[&scope.tenant(), &scope.repository()],
    )
    .await
    .unwrap();
    assert!(matches!(
        db.begin(request(&scope, "fresh", b"x")).await,
        Err(Error::Fenced)
    ));
    // Returning an already committed receipt is safe; it does not repeat its effect.
    assert!(matches!(db.begin(committed).await, Ok(Begin::Replay(_))));
    raw.execute("UPDATE th_repositories SET state='active',authority='connected' WHERE tenant=$1 AND repository=$2",&[&scope.tenant(),&scope.repository()]).await.unwrap();
    let wrong_mode = Request::new(
        scope.clone(),
        "fresh",
        "test",
        "test",
        Authority::Standalone,
        2,
        b"x",
    )
    .unwrap();
    assert!(matches!(db.begin(wrong_mode).await, Err(Error::Fenced)));
    let wrong_epoch = Request::new(
        scope.clone(),
        "fresh",
        "test",
        "test",
        Authority::Connected,
        1,
        b"x",
    )
    .unwrap();
    assert!(matches!(db.begin(wrong_epoch).await, Err(Error::Fenced)));
    let current = Request::new(
        scope,
        "fresh",
        "test",
        "test",
        Authority::Connected,
        2,
        b"x",
    )
    .unwrap();
    assert!(matches!(db.begin(current).await, Ok(Begin::Fresh(_))));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn outbox_reclaim_fences_stale_workers_and_never_crosses_tenants() {
    let (schema, scope, mut db) = setup().await;
    effect(&mut db, request(&scope, "job", b"x")).await;
    assert!(
        db.claim("other-tenant", Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    let old = db
        .claim(scope.tenant(), Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert!(
        db.claim(scope.tenant(), Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    db.renew(&old, 30).await.unwrap();
    // Force database-time expiration rather than relying on scheduling/sleeps.
    connection(&schema)
        .await
        .execute(
            "UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1",
            &[&old.id],
        )
        .await
        .unwrap();
    assert!(matches!(db.renew(&old, 30).await, Err(Error::StaleLease)));
    assert!(matches!(db.acknowledge(&old).await, Err(Error::StaleLease)));
    let current = db
        .claim(scope.tenant(), Uuid::new_v4(), 30)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(current.id, old.id);
    assert_eq!(current.generation, old.generation + 1);
    assert!(matches!(db.acknowledge(&old).await, Err(Error::StaleLease)));
    db.acknowledge(&current).await.unwrap();
    assert!(
        db.claim(scope.tenant(), Uuid::new_v4(), 30)
            .await
            .unwrap()
            .is_none()
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn outbox_freeze_retains_jobs_and_skips_locked_or_inactive_repositories() {
    let (schema, scope, mut db) = setup().await;
    let other = Scope::new(scope.tenant(), Uuid::new_v4()).unwrap();
    db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    effect(&mut db, request(&scope, "running", b"running")).await;
    let running = db
        .claim(scope.tenant(), Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    effect(&mut db, request(&scope, "pending", b"pending")).await;
    effect(&mut db, request(&other, "other", b"other")).await;
    let mut raw = connection(&schema).await;
    let before: serde_json::Value = raw
        .query_one(
            "SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM th_outbox o WHERE repository=$1",
            &[&scope.repository()],
        )
        .await
        .unwrap()
        .get(0);

    // A freeze not yet committed must exclude this repository from new claims,
    // without preventing progress for another repository of the same tenant.
    let freeze = raw.transaction().await.unwrap();
    freeze
        .execute(
            "UPDATE th_repositories SET state='frozen' WHERE repository=$1",
            &[&scope.repository()],
        )
        .await
        .unwrap();
    let next = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        db.claim(scope.tenant(), Uuid::new_v4(), 60),
    )
    .await
    .unwrap()
    .unwrap()
    .unwrap();
    assert_eq!(next.scope.tenant(), other.tenant());
    assert_eq!(next.scope.repository(), other.repository());
    db.acknowledge(&next).await.unwrap();
    freeze.commit().await.unwrap();

    for state in ["frozen", "retired"] {
        raw.execute(
            "UPDATE th_repositories SET state=$1 WHERE repository=$2",
            &[&state, &scope.repository()],
        )
        .await
        .unwrap();
        assert!(
            db.claim(scope.tenant(), Uuid::new_v4(), 60)
                .await
                .unwrap()
                .is_none()
        );
        assert!(matches!(db.renew(&running, 60).await, Err(Error::Fenced)));
        assert!(matches!(db.acknowledge(&running).await, Err(Error::Fenced)));
        let after: serde_json::Value = raw
            .query_one(
                "SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM th_outbox o WHERE repository=$1",
                &[&scope.repository()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            after, before,
            "inactive repository must retain exact jobs and leases"
        );
    }

    // Expired work is also retained while frozen. Explicitly thawing the same
    // authority permits reclaim with a new generation; it is not restore approval.
    raw.execute(
        "UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE id=$1",
        &[&running.id],
    )
    .await
    .unwrap();
    assert!(
        db.claim(scope.tenant(), Uuid::new_v4(), 60)
            .await
            .unwrap()
            .is_none()
    );
    raw.execute(
        "UPDATE th_repositories SET state='active' WHERE repository=$1",
        &[&scope.repository()],
    )
    .await
    .unwrap();
    let reclaimed = db
        .claim(scope.tenant(), Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.id, running.id);
    assert_eq!(reclaimed.generation, running.generation + 1);
    assert!(matches!(
        db.acknowledge(&running).await,
        Err(Error::StaleLease)
    ));
    db.acknowledge(&reclaimed).await.unwrap();
    let pending = db
        .claim(scope.tenant(), Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    assert_ne!(pending.id, running.id);
    db.acknowledge(&pending).await.unwrap();
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn outbox_completion_and_renewal_wait_for_concurrent_freeze() {
    let (schema, scope, mut db) = setup().await;
    effect(&mut db, request(&scope, "job", b"job")).await;
    let job = db
        .claim(scope.tenant(), Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    let mut admin = connection(&schema).await;
    let observer = connection(&schema).await;
    for acknowledge in [false, true] {
        admin
            .execute("UPDATE th_repositories SET state='active'", &[])
            .await
            .unwrap();
        let client = connection(&schema).await;
        let pid: i32 = client
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        let mut worker = Metadata::new(client).await.unwrap();
        let freeze = admin.transaction().await.unwrap();
        freeze
            .execute("UPDATE th_repositories SET state='frozen'", &[])
            .await
            .unwrap();
        let leased = job.clone();
        let pending = tokio::spawn(async move {
            if acknowledge {
                worker.acknowledge(&leased).await
            } else {
                worker.renew(&leased, 60).await
            }
        });
        // Observe the actual PostgreSQL lock wait, rather than assume a sleep
        // let the worker reach the critical section.
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let waiting: bool = observer.query_one(
                    "SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE pid=$1 AND wait_event_type='Lock')", &[&pid]
                ).await.unwrap().get(0);
                if waiting { break; }
                assert!(!pending.is_finished(), "worker must not bypass a concurrent freeze");
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }).await.unwrap();
        freeze.commit().await.unwrap();
        assert!(matches!(pending.await.unwrap(), Err(Error::Fenced)));
    }
    let state: String = observer
        .query_one("SELECT state FROM th_outbox WHERE id=$1", &[&job.id])
        .await
        .unwrap()
        .get(0);
    assert_eq!(state, "running");
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn rejected_completion_and_drop_leave_no_partial_effect() {
    let (schema, scope, mut db) = setup().await;
    let req = request(&scope, "bad-result", b"x");
    let Begin::Fresh(tx) = db.begin(req.clone()).await.unwrap() else {
        panic!()
    };
    tx.transaction()
        .execute("INSERT INTO test_effects VALUES (1)", &[])
        .await
        .unwrap();
    assert!(
        tx.complete(
            json!({"ok":true}),
            vec![Event {
                kind: "bad kind".into(),
                payload: json!({})
            }]
        )
        .await
        .is_err()
    );
    let Begin::Fresh(tx) = db.begin(req.clone()).await.unwrap() else {
        panic!()
    };
    tx.transaction()
        .execute("INSERT INTO test_effects VALUES (1)", &[])
        .await
        .unwrap();
    drop(tx);
    effect(&mut db, req).await;
    let raw = connection(&schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM test_effects", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_outbox", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs explicitly in database CI"]
async fn migration_checksum_mismatch_fails_closed() {
    let (schema, _scope, mut db) = setup().await;
    db.migrate().await.unwrap();
    connection(&schema)
        .await
        .execute(
            "UPDATE th_schema_version SET migration_digest=decode(repeat('00',32),'hex')",
            &[],
        )
        .await
        .unwrap();
    assert!(matches!(db.migrate().await, Err(Error::SchemaMismatch)));
    cleanup(&schema).await;
}

#[test]
fn invalid_identifiers_and_oversized_commands_are_rejected_before_sql() {
    assert!(Scope::new("../escape", Uuid::new_v4()).is_err());
    let scope = Scope::new("valid", Uuid::new_v4()).unwrap();
    assert!(
        Request::new(
            scope.clone(),
            "key",
            "p",
            "method",
            Authority::Standalone,
            0,
            b"x"
        )
        .is_err()
    );
    assert!(
        Request::new(
            scope.clone(),
            "key with spaces",
            "p",
            "method",
            Authority::Standalone,
            1,
            b"x"
        )
        .is_err()
    );
    assert!(
        Request::new(
            scope,
            "key",
            "p",
            "method",
            Authority::Standalone,
            1,
            &vec![0; 1024 * 1024 + 1]
        )
        .is_err()
    );
}
