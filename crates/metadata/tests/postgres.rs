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
