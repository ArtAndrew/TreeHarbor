mod common;
use common::{cleanup, connection, setup};
use treeharbor_core::{
    Changes, Commit, Directory, Entry, EntryMode, EntryRef, Name, Object, RevisionRef,
    SourceProfile,
};
use treeharbor_metadata::{
    Authority, Error, Metadata, Scope,
    access::{Authorization, IssuedToken, Permission, PrincipalKind},
    intake::{WorkspaceFormat, WorkspaceSpec},
    uploads::UploadLimits,
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn concurrent_duplicates_share_one_reservation_and_one_completed_base() {
    let (schema, s, mut db) = setup().await;
    let (_, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &auth).await;
    let graph = graph();
    let mut other = Metadata::new(connection(&schema).await).await.unwrap();
    for object in &graph {
        let prepared = PreparedObject::new(object).unwrap();
        let (a, b) = tokio::join!(
            db.reserve_source_object(&auth, upload, &store, &prepared),
            other.reserve_source_object(&auth, upload, &store, &prepared)
        );
        let a = a.unwrap();
        let b = b.unwrap();
        assert_eq!(a.generation(), b.generation());
        let receipt = store.put(a.generation(), &prepared).unwrap();
        let (a, b) = tokio::join!(
            db.publish_source_object(&auth, &a, &receipt),
            other.publish_source_object(&auth, &b, &receipt)
        );
        a.unwrap();
        b.unwrap();
    }
    let root = graph.last().unwrap().id().unwrap();
    let ctx = auth.context("one-finish".into(), Authority::Standalone, 1);
    let (a, b) = tokio::join!(
        db.finish_source_upload(&ctx, upload, root),
        other.finish_source_upload(&ctx, upload, root)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    assert_eq!(
        db.source_upload_status(&auth, upload).await.unwrap()["reserved_objects"],
        graph.len()
    );
    let raw = connection(&schema).await;
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_bases", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn backend_identity_cannot_be_remapped_or_adopt_unknown_legacy_objects() {
    let (schema, s, mut db) = setup().await;
    let identity = Uuid::new_v4();
    db.bind_storage_backend("local-1", identity).await.unwrap();
    db.bind_storage_backend("local-1", identity).await.unwrap();
    assert!(matches!(
        db.bind_storage_backend("local-1", Uuid::new_v4()).await,
        Err(Error::ReceiptMismatch)
    ));
    let (_dir, store) = store(&s);
    let object = PreparedObject::new(&Object::Blob(vec![])).unwrap();
    db.reserve_object(&store, &object, Uuid::new_v4(), 60)
        .await
        .unwrap();
    let raw = connection(&schema).await;
    raw.execute("DELETE FROM th_storage_backends", &[])
        .await
        .unwrap();
    assert!(matches!(
        db.bind_storage_backend("local-1", identity).await,
        Err(Error::ReceiptMismatch)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn migration_from_v5_preserves_profiles_credentials_and_enables_uploads() {
    use sha2::{Digest, Sha256};
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    let raw = connection("public").await;
    raw.batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let mut digest = Vec::new();
    for sql in [
        include_str!("../migrations/001_transactions.sql"),
        include_str!("../migrations/002_object_catalog.sql"),
        include_str!("../migrations/003_change_intake.sql"),
        include_str!("../migrations/004_access_control.sql"),
        include_str!("../migrations/005_source_profiles.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut hash = Sha256::new();
        hash.update(digest);
        hash.update(sql.as_bytes());
        digest = hash.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(5,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    let (token, _) = actor(&mut db, &scope, "human").await;
    let write = db
        .authorize(&token.secret, &scope, Permission::WorkspaceWrite)
        .await
        .unwrap();
    let profile = SourceProfile::full();
    let ctx = write.context("profile".into(), Authority::Standalone, 1);
    let receipt = db.put_source_profile(&ctx, &profile).await.unwrap();
    assert!(db.check_schema().await.is_err());
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    db.check_schema().await.unwrap();
    assert_eq!(
        receipt,
        db.put_source_profile(&ctx, &profile).await.unwrap()
    );
    let auth = db
        .authorize(&token.secret, &scope, Permission::RepositoryWrite)
        .await
        .unwrap();
    let id = start(&mut db, &auth).await;
    assert_eq!(
        db.source_upload_status(&auth, id).await.unwrap()["reserved_objects"],
        0
    );
    cleanup(&schema).await;
}

async fn actor(db: &mut Metadata, s: &Scope, name: &str) -> (IssuedToken, Authorization) {
    db.provision_principal(s.tenant(), name, PrincipalKind::Human, "operator")
        .await
        .unwrap();
    db.set_repository_access(
        s,
        name,
        &[
            Permission::RepositoryWrite,
            Permission::WorkspaceWrite,
            Permission::Read,
        ],
        "operator",
    )
    .await
    .unwrap();
    let token = db
        .issue_repository_token(
            s,
            name,
            &[
                Permission::RepositoryWrite,
                Permission::WorkspaceWrite,
                Permission::Read,
            ],
            3600,
            "operator",
        )
        .await
        .unwrap();
    let auth = db
        .authorize(&token.secret, s, Permission::RepositoryWrite)
        .await
        .unwrap();
    (token, auth)
}
fn store(s: &Scope) -> (tempfile::TempDir, ManagedStore) {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let store =
        ManagedStore::open(dir.path(), TenantId::new(s.tenant()).unwrap(), "local-1").unwrap();
    (dir, store)
}
fn limits() -> UploadLimits {
    UploadLimits {
        objects: 100,
        bytes: 1024 * 1024,
        ttl_seconds: 3600,
    }
}
async fn start(db: &mut Metadata, auth: &Authorization) -> Uuid {
    let id = Uuid::new_v4();
    db.begin_source_upload(
        &auth.context(format!("begin-{id}"), Authority::Standalone, 1),
        "local-1",
        id,
        &limits(),
    )
    .await
    .unwrap();
    id
}
async fn put(
    db: &mut Metadata,
    auth: &Authorization,
    id: Uuid,
    store: &ManagedStore,
    obj: &Object,
) {
    let frame = obj.encode().unwrap();
    let p = PreparedObject::from_frame(obj.id().unwrap(), frame).unwrap();
    let t = db.reserve_source_object(auth, id, store, &p).await.unwrap();
    let receipt = store.put(t.generation(), &p).unwrap();
    db.publish_source_object(auth, &t, &receipt).await.unwrap();
}
fn graph() -> Vec<Object> {
    let blob = Object::Blob(b"hello\0\xff".to_vec());
    let directory = Object::Directory(
        Directory::new(vec![Entry {
            name: Name::new(b"file".to_vec()).unwrap(),
            value: EntryRef {
                mode: EntryMode::File,
                target: blob.id().unwrap(),
            },
        }])
        .unwrap(),
    );
    let snapshot = Object::Snapshot {
        root: directory.id().unwrap(),
    };
    let changes = Object::ChangeOperations(Changes::new(vec![]).unwrap());
    let commit = Object::NativeCommit(Commit {
        parents: vec![],
        snapshot: snapshot.id().unwrap(),
        changes: changes.id().unwrap(),
        author_name: "Test".into(),
        author_email: "test@example.invalid".into(),
        timestamp_seconds: 0,
        timezone_minutes: 0,
        message: "import".into(),
    });
    vec![blob, directory, snapshot, changes, commit]
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn completed_upload_is_replayable_after_restart_and_retains_a_workspace_base() {
    let (schema, s, mut db) = setup().await;
    let (token, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &auth).await;
    let graph = graph();
    for object in &graph {
        put(&mut db, &auth, upload, &store, object).await;
    }
    let root = graph.last().unwrap().id().unwrap();
    let context = auth.context("finish".into(), Authority::Standalone, 1);
    let result = db
        .finish_source_upload(&context, upload, root)
        .await
        .unwrap();
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    assert_eq!(
        result,
        db.finish_source_upload(&context, upload, root)
            .await
            .unwrap()
    );
    assert_eq!(
        db.source_upload_status(&auth, upload).await.unwrap()["state"],
        "completed"
    );
    assert!(matches!(
        db.finish_source_upload(
            &auth.context("other-finish".into(), Authority::Standalone, 1),
            upload,
            root
        )
        .await,
        Err(Error::UploadState)
    ));
    let write = db
        .authorize(&token.secret, &s, Permission::WorkspaceWrite)
        .await
        .unwrap();
    db.put_source_profile(
        &write.context("profile".into(), Authority::Standalone, 1),
        &SourceProfile::full(),
    )
    .await
    .unwrap();
    db.create_workspace(
        &write.context("workspace".into(), Authority::Standalone, 1),
        &WorkspaceSpec {
            id: Uuid::new_v4(),
            base: RevisionRef::Native(root),
            profile: *SourceProfile::full().id().as_bytes(),
            format: WorkspaceFormat::Native,
        },
    )
    .await
    .unwrap();
    let lease = db
        .lease_pinned_root(&s, &format!("base:{}", root.hex()), Uuid::new_v4(), 60)
        .await
        .unwrap();
    db.release_object_hold(&lease).await.unwrap();
    connection(&schema)
        .await
        .execute(
            "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 second'",
            &[],
        )
        .await
        .unwrap();
    assert!(
        db.try_retire_object(lease.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store.get(lease.key().generation(), root).unwrap(),
        *graph.last().unwrap()
    );
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn tenant_dedup_does_not_grant_repository_or_session_membership() {
    let (schema, s, mut db) = setup().await;
    let (_, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let a = start(&mut db, &auth).await;
    let graph = graph();
    put(&mut db, &auth, a, &store, &graph[0]).await;
    let other = Scope::new(s.tenant(), Uuid::new_v4()).unwrap();
    db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    let (_, b_auth) = actor(&mut db, &other, "other").await;
    let b = start(&mut db, &b_auth).await;
    let parent = PreparedObject::new(&graph[1]).unwrap();
    assert!(matches!(
        db.reserve_source_object(&b_auth, b, &store, &parent).await,
        Err(Error::UploadMembership)
    ));
    assert!(matches!(
        db.source_upload_status(&b_auth, a).await,
        Err(Error::NotFound)
    ));
    // Complete bytes confer possession and permit physical dedup, not a digest probe.
    put(&mut db, &b_auth, b, &store, &graph[0]).await;
    put(&mut db, &b_auth, b, &store, &graph[1]).await;
    let raw = connection(&schema).await;
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_objects", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 2);
    let fresh = start(&mut db, &b_auth).await;
    assert!(matches!(
        db.reserve_source_object(&b_auth, fresh, &store, &parent)
            .await,
        Err(Error::UploadMembership)
    ));
    let (_, imposter) = actor(&mut db, &other, "imposter").await;
    assert!(matches!(
        db.source_upload_status(&imposter, b).await,
        Err(Error::NotFound)
    ));
    // Existing blob bytes cannot satisfy a reference that expects a directory.
    let wrong = PreparedObject::new(&Object::Snapshot {
        root: graph[0].id().unwrap(),
    })
    .unwrap();
    assert!(matches!(
        db.reserve_source_object(&b_auth, b, &store, &wrong).await,
        Err(Error::UploadMembership)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn interrupted_upload_retries_without_budget_growth_and_revocation_fences_publication() {
    let (schema, s, mut db) = setup().await;
    let (token, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let upload = start(&mut db, &auth).await;
    let object = PreparedObject::new(&graph()[0]).unwrap();
    let first = db
        .reserve_source_object(&auth, upload, &store, &object)
        .await
        .unwrap();
    let receipt = store.put(first.generation(), &object).unwrap();
    drop(db);
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let retry = db
        .reserve_source_object(&auth, upload, &store, &object)
        .await
        .unwrap();
    assert_eq!(first.generation(), retry.generation());
    assert_eq!(
        db.source_upload_status(&auth, upload).await.unwrap()["reserved_objects"],
        1
    );
    db.revoke_repository_token(&s, token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        db.publish_source_object(&auth, &retry, &receipt).await,
        Err(Error::Unauthenticated)
    ));
    let row = connection(&schema)
        .await
        .query_one("SELECT complete FROM th_source_upload_items", &[])
        .await
        .unwrap();
    assert!(!row.get::<_, bool>(0));
    let (new_token, new_auth) = actor(&mut db, &s, "human").await;
    let retry = db
        .reserve_source_object(&new_auth, upload, &store, &object)
        .await
        .unwrap();
    db.publish_source_object(&new_auth, &retry, &receipt)
        .await
        .unwrap();
    let weak = db
        .authorize(&new_token.secret, &s, Permission::Read)
        .await
        .unwrap();
    assert!(matches!(
        db.reserve_source_object(&weak, upload, &store, &object)
            .await,
        Err(Error::Forbidden)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn upload_deadlines_budgets_abort_and_authority_are_enforced() {
    let (schema, s, mut db) = setup().await;
    let (_, auth) = actor(&mut db, &s, "human").await;
    let (_dir, store) = store(&s);
    let id = Uuid::new_v4();
    let object = PreparedObject::new(&graph()[0]).unwrap();
    let context = auth.context("begin-small".into(), Authority::Standalone, 1);
    let limits = UploadLimits {
        objects: 1,
        bytes: object.length() as i64,
        ttl_seconds: 60,
    };
    let result = db
        .begin_source_upload(&context, "local-1", id, &limits)
        .await
        .unwrap();
    assert_eq!(
        result,
        db.begin_source_upload(&context, "local-1", id, &limits)
            .await
            .unwrap()
    );
    assert!(matches!(
        db.begin_source_upload(
            &auth.context("duplicate-uuid".into(), Authority::Standalone, 1),
            "local-1",
            id,
            &limits
        )
        .await,
        Err(Error::UploadState)
    ));
    let t = db
        .reserve_source_object(&auth, id, &store, &object)
        .await
        .unwrap();
    let receipt = store.put(t.generation(), &object).unwrap();
    let overflow = PreparedObject::new(&Object::Blob(vec![])).unwrap();
    assert!(matches!(
        db.reserve_source_object(&auth, id, &store, &overflow).await,
        Err(Error::UploadLimit)
    ));
    let raw = connection(&schema).await;
    let bounded:bool=raw.query_one("SELECT h.expires_at=u.expires_at FROM th_object_holds h JOIN th_source_uploads u ON h.tenant=u.tenant",&[]).await.unwrap().get(0);
    assert!(bounded);
    db.abort_source_upload(&auth.context("abort".into(), Authority::Standalone, 1), id)
        .await
        .unwrap();
    assert!(matches!(
        db.publish_source_object(&auth, &t, &receipt).await,
        Err(Error::UploadState)
    ));
    let id = start(&mut db, &auth).await;
    // A new session cannot steal an in-flight digest; use distinct bytes.
    let another = PreparedObject::new(&Object::Blob(b"other".to_vec())).unwrap();
    let t = db
        .reserve_source_object(&auth, id, &store, &another)
        .await
        .unwrap();
    let receipt = store.put(t.generation(), &another).unwrap();
    raw.execute("UPDATE th_source_uploads SET expires_at=clock_timestamp()-interval '1 second' WHERE upload=$1",&[&id]).await.unwrap();
    assert!(matches!(
        db.publish_source_object(&auth, &t, &receipt).await,
        Err(Error::UploadState)
    ));
    let id = start(&mut db, &auth).await;
    raw.execute("UPDATE th_repositories SET epoch=epoch+1", &[])
        .await
        .unwrap();
    assert!(matches!(
        db.reserve_source_object(&auth, id, &store, &overflow).await,
        Err(Error::Fenced)
    ));
    cleanup(&schema).await;
}
