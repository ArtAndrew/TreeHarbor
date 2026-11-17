use super::*;
use treeharbor_metadata::uploads::Purpose;

fn blobs() -> Vec<PreparedObject> {
    let mut objects: Vec<_> = [b"first".as_slice(), b"second", b"third"]
        .into_iter()
        .map(|bytes| PreparedObject::new(&Object::Blob(bytes.to_vec())).unwrap())
        .collect();
    objects.sort_unstable_by_key(PreparedObject::id);
    objects
}

async fn publication_counts(raw: &tokio_postgres::Client) -> (i64, i64) {
    let row = raw.query_one(
        "SELECT (SELECT count(*) FROM th_objects WHERE state='live'), (SELECT count(*) FROM th_source_upload_items WHERE complete)",
        &[],
    ).await.unwrap();
    (row.get(0), row.get(1))
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn mixed_blob_reservation_rolls_back_then_reuses_the_existing_generation() {
    let (schema, s, mut db) = setup().await;
    let (_, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let objects = blobs();
    let first = start(&mut db, &auth).await;
    let held = db
        .reserve_source_object(&auth, first, &store, &objects[1])
        .await
        .unwrap();
    let upload = start(&mut db, &auth).await;
    assert!(matches!(
        db.reserve_source_blob_batch(&auth, upload, &store, &objects, Purpose::Human)
            .await,
        Err(Error::ObjectBusy)
    ));
    let raw = connection(&schema).await;
    let row = raw
        .query_one(
            "SELECT (SELECT count(*) FROM th_objects), (SELECT count(*) FROM th_object_holds),
                (SELECT count(*) FROM th_source_upload_items WHERE upload=$1)",
            &[&upload],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, i64>(0),
            row.get::<_, i64>(1),
            row.get::<_, i64>(2)
        ),
        (1, 1, 0)
    );
    assert_eq!(
        db.source_upload_status(&auth, upload).await.unwrap()["reserved_objects"],
        0
    );

    let receipt = store.put(held.generation(), &objects[1]).unwrap();
    db.publish_source_object(&auth, &held, &receipt)
        .await
        .unwrap();
    let tickets = db
        .reserve_source_blob_batch(&auth, upload, &store, &objects, Purpose::Human)
        .await
        .unwrap();
    assert_eq!(tickets[1].generation(), held.generation());
    let receipts: Vec<_> = tickets
        .iter()
        .zip(&objects)
        .map(|(ticket, object)| store.put(ticket.generation(), object).unwrap())
        .collect();
    db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
        .await
        .unwrap();
    let replay = db
        .reserve_source_blob_batch(&auth, upload, &store, &objects, Purpose::Human)
        .await
        .unwrap();
    db.publish_source_blob_batch(&auth, &replay, &receipts, Purpose::Human)
        .await
        .unwrap();
    assert_eq!(publication_counts(&raw).await, (3, 4));
    let holds_match: i64 = raw.query_one(
        "SELECT count(*) FROM th_source_upload_items i
         JOIN th_source_uploads u USING (tenant,repository,upload)
         JOIN th_object_holds h ON (h.tenant,h.token,h.digest,h.generation)=(i.tenant,i.hold_token,i.digest,i.generation)
         WHERE i.upload=$1 AND h.expires_at=u.expires_at", &[&upload],
    ).await.unwrap().get(0);
    assert_eq!(holds_match, 3);
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn bulk_blobs_validate_the_whole_batch_and_replay_after_rollback_and_restart() {
    let (schema, s, mut db) = setup().await;
    let (token, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &auth).await;
    let objects = blobs();
    let tickets = db
        .reserve_source_blob_batch(&auth, upload, &store, &objects, Purpose::Human)
        .await
        .unwrap();
    let mut receipts: Vec<_> = tickets
        .iter()
        .zip(&objects)
        .map(|(ticket, object)| store.put(ticket.generation(), object).unwrap())
        .collect();
    let raw = connection(&schema).await;

    receipts.swap(1, 2);
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::ReceiptMismatch)
    ));
    receipts.swap(1, 2);
    assert_eq!(publication_counts(&raw).await, (0, 0));

    let last = objects[2].id().as_bytes().to_vec();
    raw.execute(
        "UPDATE th_objects SET length=length+1 WHERE digest=$1",
        &[&last],
    )
    .await
    .unwrap();
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::ReceiptMismatch)
    ));
    raw.execute(
        "UPDATE th_objects SET length=length-1 WHERE digest=$1",
        &[&last],
    )
    .await
    .unwrap();

    raw.execute(
        "UPDATE th_objects SET state='retiring' WHERE digest=$1",
        &[&last],
    )
    .await
    .unwrap();
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::ObjectNotLive)
    ));
    raw.execute(
        "UPDATE th_objects SET state='uploading' WHERE digest=$1",
        &[&last],
    )
    .await
    .unwrap();

    raw.execute("UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second' WHERE digest=$1", &[&last]).await.unwrap();
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::StaleLease)
    ));
    raw.execute("UPDATE th_object_holds h SET expires_at=u.expires_at FROM th_source_uploads u WHERE u.upload=$1 AND h.tenant=u.tenant AND h.digest=$2", &[&upload,&last]).await.unwrap();
    assert_eq!(publication_counts(&raw).await, (0, 0));

    raw.batch_execute(
        "
        CREATE FUNCTION reject_batch_complete() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.complete THEN RAISE EXCEPTION 'injected batch publication failure'; END IF;
            RETURN NEW;
        END $$;
        CREATE TRIGGER reject_batch_complete BEFORE UPDATE ON th_source_upload_items
        FOR EACH ROW EXECUTE FUNCTION reject_batch_complete();
    ",
    )
    .await
    .unwrap();
    assert!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await
            .is_err()
    );
    assert_eq!(publication_counts(&raw).await, (0, 0));
    raw.batch_execute("DROP TRIGGER reject_batch_complete ON th_source_upload_items; DROP FUNCTION reject_batch_complete()").await.unwrap();

    db.revoke_repository_token(&s, token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::Unauthenticated)
    ));
    assert_eq!(publication_counts(&raw).await, (0, 0));
    let (_, auth) = actor(&mut db, &s, "human").await;
    // A valid batch can mix an already published object with new objects.
    db.publish_source_object(&auth, &tickets[0], &receipts[0])
        .await
        .unwrap();
    assert_eq!(publication_counts(&raw).await, (1, 1));
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let mut other = Metadata::new(connection(&schema).await).await.unwrap();
    let (a, b) = tokio::join!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human),
        other.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
    );
    a.unwrap();
    b.unwrap();
    assert_eq!(publication_counts(&raw).await, (3, 3));
    assert_eq!(
        db.source_upload_status(&auth, upload).await.unwrap()["reserved_objects"],
        3
    );
    for object in objects {
        assert_eq!(
            store.get(1, object.id()).unwrap().id().unwrap(),
            object.id()
        );
    }
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn bulk_blobs_cannot_publish_a_retired_and_replaced_generation() {
    let (schema, s, mut db) = setup().await;
    let (_, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &auth).await;
    let objects = blobs();
    let tickets = db
        .reserve_source_blob_batch(&auth, upload, &store, &objects, Purpose::Human)
        .await
        .unwrap();
    let receipts: Vec<_> = tickets
        .iter()
        .zip(&objects)
        .map(|(ticket, object)| store.put(ticket.generation(), object).unwrap())
        .collect();
    db.publish_source_object(&auth, &tickets[2], &receipts[2])
        .await
        .unwrap();
    let last = objects[2].id().as_bytes().to_vec();
    let held = db
        .reserve_object(&store, &objects[2], Uuid::new_v4(), 60)
        .await
        .unwrap();
    db.release_object_hold(&held).await.unwrap();
    let raw = connection(&schema).await;
    raw.execute("UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second' WHERE digest=$1", &[&last]).await.unwrap();
    let retired = db.try_retire_object(held.key(), 0).await.unwrap().unwrap();
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::ObjectNotLive)
    ));
    db.finish_retirement(&store, &retired).await.unwrap();
    let second = start(&mut db, &auth).await;
    let newer = db
        .reserve_source_object(&auth, second, &store, &objects[2])
        .await
        .unwrap();
    assert_eq!(newer.generation(), tickets[2].generation() + 1);
    let newer_receipt = store.put(newer.generation(), &objects[2]).unwrap();
    db.publish_source_object(&auth, &newer, &newer_receipt)
        .await
        .unwrap();
    assert!(matches!(
        db.publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
            .await,
        Err(Error::ReceiptMismatch)
    ));
    // The rejected batch cannot revive its two previously unpublished objects.
    assert_eq!(publication_counts(&raw).await, (1, 2));
    assert!(store.get(newer.generation(), objects[2].id()).is_ok());
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn bulk_blobs_in_different_repositories_contend_without_losing_membership() {
    use std::time::Duration;
    let (schema, scope, mut db) = setup().await;
    let (_, auth) = actor(&mut db, &scope, "human").await;
    let (_dir, store) = store(&scope);
    let objects = blobs();
    let seed = start(&mut db, &auth).await;
    for object in &objects {
        let ticket = db
            .reserve_source_object(&auth, seed, &store, object)
            .await
            .unwrap();
        let receipt = store.put(ticket.generation(), object).unwrap();
        db.publish_source_object(&auth, &ticket, &receipt)
            .await
            .unwrap();
    }
    // Different repositories avoid the intentional per-repository writer lock.
    // Their tenant-scoped object catalog still overlaps on the middle blob.
    let other_scope = Scope::new(scope.tenant(), Uuid::new_v4()).unwrap();
    db.create_repository(&other_scope, Authority::Standalone)
        .await
        .unwrap();
    let (_, other_auth) = actor(&mut db, &other_scope, "human").await;
    let mut writers = Vec::new();
    for (auth, objects) in [(auth, &objects[..2]), (other_auth, &objects[1..])] {
        let upload = start(&mut db, &auth).await;
        let tickets = db
            .reserve_source_blob_batch(&auth, upload, &store, objects, Purpose::Human)
            .await
            .unwrap();
        let receipts: Vec<_> = tickets
            .iter()
            .zip(objects)
            .map(|(ticket, object)| store.put(ticket.generation(), object).unwrap())
            .collect();
        let raw = connection(&schema).await;
        let pid: i32 = raw
            .query_one("SELECT pg_backend_pid()", &[])
            .await
            .unwrap()
            .get(0);
        writers.push((
            pid,
            Metadata::new(raw).await.unwrap(),
            auth,
            tickets,
            receipts,
        ));
    }
    let mut blocker = connection(&schema).await;
    let gate = blocker.transaction().await.unwrap();
    gate.query_one(
        "SELECT digest FROM th_objects WHERE tenant=$1 AND digest=$2 FOR UPDATE",
        &[&scope.tenant(), &objects[1].id().as_bytes().to_vec()],
    )
    .await
    .unwrap();
    let mut tasks = Vec::new();
    let mut pids = Vec::new();
    for (pid, mut writer, auth, tickets, receipts) in writers {
        pids.push(pid);
        tasks.push(tokio::spawn(async move {
            writer
                .publish_source_blob_batch(&auth, &tickets, &receipts, Purpose::Human)
                .await
        }));
    }
    let observer = connection(&schema).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let blocked: bool = observer.query_one(
                "SELECT bool_and(cardinality(pg_blocking_pids(pid))>0) FROM unnest($1::integer[]) AS p(pid)",
                &[&pids],
            ).await.unwrap().get(0);
            if blocked { break; }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("both publishers must reach object-lock contention");
    gate.commit().await.unwrap();
    for task in tasks {
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
    assert_eq!(publication_counts(&observer).await, (3, 7));
    let memberships: Vec<(Uuid, i64)> = observer.query(
        "SELECT repository,count(*) FROM th_source_upload_items WHERE complete GROUP BY repository ORDER BY repository",
        &[],
    ).await.unwrap().into_iter().map(|row| (row.get(0), row.get(1))).collect();
    assert_eq!(
        memberships
            .iter()
            .find(|(r, _)| *r == scope.repository())
            .unwrap()
            .1,
        5
    );
    assert_eq!(
        memberships
            .iter()
            .find(|(r, _)| *r == other_scope.repository())
            .unwrap()
            .1,
        2
    );
    cleanup(&schema).await;
}
