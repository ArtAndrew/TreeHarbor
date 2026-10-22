use super::*;
use treeharbor_metadata::exports::ExportFence;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; authenticated live-source export"]
async fn export_requires_current_read_scope_membership_epoch_and_unexpired_hold() {
    let (schema, s, mut db) = setup().await;
    let (token, writer) = actor(&mut db, &s, "owner").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &writer).await;
    let objects = graph();
    for o in &objects {
        put(&mut db, &writer, upload, &store, o).await;
    }
    let root = objects.last().unwrap().id().unwrap();
    db.finish_source_upload(
        &writer.context("finish".into(), Authority::Standalone, 1),
        upload,
        root,
    )
    .await
    .unwrap();
    let auth = db
        .authorize(&token.secret, &s, Permission::Read)
        .await
        .unwrap();
    let fence = ExportFence {
        authority: Authority::Standalone,
        epoch: 1,
        revision: root,
    };
    assert!(matches!(
        db.open_source_export(&writer, fence, 60).await,
        Err(Error::Forbidden)
    ));
    assert!(matches!(
        db.open_source_export(
            &auth,
            ExportFence {
                revision: objects[0].id().unwrap(),
                ..fence
            },
            60
        )
        .await,
        Err(Error::NotFound)
    ));
    let other = Scope::new(s.tenant(), Uuid::new_v4()).unwrap();
    db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    let (other_token, _) = actor(&mut db, &other, "other-reader").await;
    let other_auth = db
        .authorize(&other_token.secret, &other, Permission::Read)
        .await
        .unwrap();
    assert!(matches!(
        db.open_source_export(&other_auth, fence, 60).await,
        Err(Error::NotFound)
    ));

    let read = db.open_source_export(&auth, fence, 60).await.unwrap();
    let node = read.root();
    assert_eq!(
        db.read_source_export(&read, &store, &[node], 128 * 1024 * 1024)
            .await
            .unwrap()
            .read()
            .unwrap()[0]
            .object(),
        objects.last().unwrap()
    );
    connection(&schema)
        .await
        .execute(
            "UPDATE th_repositories SET epoch=2 WHERE tenant=$1 AND repository=$2",
            &[&s.tenant(), &s.repository()],
        )
        .await
        .unwrap();
    assert!(matches!(
        db.read_source_export(&read, &store, &[read.root()], 128 * 1024 * 1024)
            .await,
        Err(Error::Fenced)
    ));
    assert!(matches!(
        db.finish_source_export(read).await,
        Err(Error::Fenced)
    ));
    let fence = ExportFence { epoch: 2, ..fence };
    let read = db.open_source_export(&auth, fence, 60).await.unwrap();
    connection(&schema)
        .await
        .execute(
            "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    assert!(matches!(
        db.read_source_export(&read, &store, &[read.root()], 128 * 1024 * 1024)
            .await,
        Err(Error::StaleLease)
    ));
    db.finish_source_export(read).await.unwrap();
    let read = db.open_source_export(&auth, fence, 60).await.unwrap();
    db.revoke_repository_token(&s, token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        db.read_source_export(&read, &store, &[read.root()], 128 * 1024 * 1024)
            .await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        db.finish_source_export(read).await,
        Err(Error::Unauthenticated)
    ));
    let live: i64 = connection(&schema)
        .await
        .query_one(
            "SELECT count(*) FROM th_object_holds WHERE expires_at>clock_timestamp()",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(live, 0);
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; retained export graph against GC"]
async fn export_hold_prevents_retirement_after_base_removal_until_fenced_close() {
    let (schema, s, mut db) = setup().await;
    let (token, writer) = actor(&mut db, &s, "owner").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &writer).await;
    let objects = graph();
    for o in &objects {
        put(&mut db, &writer, upload, &store, o).await;
    }
    let root = objects.last().unwrap().id().unwrap();
    db.finish_source_upload(
        &writer.context("finish".into(), Authority::Standalone, 1),
        upload,
        root,
    )
    .await
    .unwrap();
    let holder = format!("base:{}", root.hex());
    let probe = db
        .lease_pinned_root(&s, &holder, Uuid::new_v4(), 60)
        .await
        .unwrap();
    db.release_object_hold(&probe).await.unwrap();
    connection(&schema)
        .await
        .execute(
            "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    let auth = db
        .authorize(&token.secret, &s, Permission::Read)
        .await
        .unwrap();
    let read = db
        .open_source_export(
            &auth,
            ExportFence {
                authority: Authority::Standalone,
                epoch: 1,
                revision: root,
            },
            60,
        )
        .await
        .unwrap();
    connection(&schema)
        .await
        .execute("DELETE FROM th_bases", &[])
        .await
        .unwrap();
    let req = treeharbor_metadata::Request::new(
        s.clone(),
        "remove-base",
        "test",
        "test-remove",
        Authority::Standalone,
        1,
        b"remove",
    )
    .unwrap();
    let treeharbor_metadata::Begin::Fresh(tx) = db.begin(req).await.unwrap() else {
        panic!()
    };
    tx.unpin_object(&holder).await.unwrap();
    tx.complete(serde_json::json!({}), vec![]).await.unwrap();
    assert!(
        db.try_retire_object(probe.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        db.finish_source_export(read).await,
        Err(Error::NotFound)
    ));
    assert!(
        db.try_retire_object(probe.key(), 0)
            .await
            .unwrap()
            .is_some()
    );
    cleanup(&schema).await;
}
