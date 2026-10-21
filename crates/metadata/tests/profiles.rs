mod common;
use common::{cleanup, connection, setup};
use treeharbor_core::{RepoPath, SourceProfile};
use treeharbor_metadata::{
    Authority, Error, Scope,
    access::{Permission as P, PrincipalKind as K},
    intake::CommandContext,
};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn profile_definitions_are_immutable_scoped_and_authorized_on_replay() {
    let (schema, scope, mut db) = setup().await;
    db.provision_principal(scope.tenant(), "agent", K::Agent, "operator")
        .await
        .unwrap();
    db.set_repository_access(&scope, "agent", &[P::Read, P::WorkspaceWrite], "operator")
        .await
        .unwrap();
    let token = db
        .issue_repository_token(
            &scope,
            "agent",
            &[P::Read, P::WorkspaceWrite],
            3600,
            "operator",
        )
        .await
        .unwrap();
    let write = db
        .authorize(&token.secret, &scope, P::WorkspaceWrite)
        .await
        .unwrap();
    let read = db.authorize(&token.secret, &scope, P::Read).await.unwrap();
    let context = write.context("selection".into(), Authority::Standalone, 1);
    let sparse = SourceProfile::sparse(vec![
        RepoPath::new(b"src".to_vec()).unwrap(),
        RepoPath::new(b"docs/\xff".to_vec()).unwrap(),
    ])
    .unwrap();
    let receipt = db.put_source_profile(&context, &sparse).await.unwrap();
    assert_eq!(receipt["profile_digest"], sparse.id().to_string());
    assert_eq!(
        db.authorized_source_profile(&read, sparse.id())
            .await
            .unwrap(),
        sparse
    );
    assert_eq!(
        db.put_source_profile(&context, &sparse).await.unwrap(),
        receipt
    );
    assert!(matches!(
        db.put_source_profile(&context, &SourceProfile::full())
            .await,
        Err(Error::IdempotencyConflict)
    ));
    assert!(matches!(
        db.put_source_profile(
            &read.context("bad".into(), Authority::Standalone, 1),
            &SourceProfile::full()
        )
        .await,
        Err(Error::Forbidden)
    ));
    let other = Scope::new(scope.tenant(), Uuid::new_v4()).unwrap();
    db.create_repository(&other, Authority::Standalone)
        .await
        .unwrap();
    db.set_repository_access(&other, "agent", &[P::Read], "operator")
        .await
        .unwrap();
    let other_token = db
        .issue_repository_token(&other, "agent", &[P::Read], 3600, "operator")
        .await
        .unwrap();
    let other_auth = db
        .authorize(&other_token.secret, &other, P::Read)
        .await
        .unwrap();
    assert!(matches!(
        db.authorized_source_profile(&other_auth, sparse.id()).await,
        Err(Error::NotFound)
    ));
    let raw = connection(&schema).await;
    assert!(
        raw.execute("UPDATE th_source_profiles SET definition=definition", &[])
            .await
            .is_err()
    );
    db.revoke_repository_token(&scope, token.id, "operator")
        .await
        .unwrap();
    assert!(matches!(
        db.put_source_profile(&context, &sparse).await,
        Err(Error::Unauthenticated)
    ));
    assert!(matches!(
        db.authorized_source_profile(&read, sparse.id()).await,
        Err(Error::Unauthenticated)
    ));
    cleanup(&schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn migration_from_published_v4_preserves_credentials_and_does_not_create_profiles() {
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
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut hash = Sha256::new();
        hash.update(digest);
        hash.update(sql.as_bytes());
        digest = hash.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(4,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let mut db = treeharbor_metadata::Metadata::new(connection(&schema).await)
        .await
        .unwrap();
    let scope = Scope::new("upgrade-profile", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Standalone)
        .await
        .unwrap();
    db.provision_principal(scope.tenant(), "agent", K::Agent, "operator")
        .await
        .unwrap();
    db.set_repository_access(&scope, "agent", &[P::Read, P::WorkspaceWrite], "operator")
        .await
        .unwrap();
    let token = db
        .issue_repository_token(
            &scope,
            "agent",
            &[P::Read, P::WorkspaceWrite],
            3600,
            "operator",
        )
        .await
        .unwrap();
    assert!(db.check_schema().await.is_err());
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    db.check_schema().await.unwrap();
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_source_profiles", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    let auth = db
        .authorize(&token.secret, &scope, P::WorkspaceWrite)
        .await
        .unwrap();
    db.put_source_profile(
        &auth.context("new-profile".into(), Authority::Standalone, 1),
        &SourceProfile::full(),
    )
    .await
    .unwrap();
    let count: i64 = raw
        .query_one("SELECT count(*) FROM th_access_tokens", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    // An arbitrary caller-chosen digest cannot be silently replaced by a profile.
    let bad = CommandContext::trusted(
        scope,
        "different".into(),
        "operator".into(),
        Authority::Connected,
        1,
    );
    assert!(matches!(
        db.put_source_profile(&bad, &SourceProfile::full()).await,
        Err(Error::Fenced)
    ));
    cleanup(&schema).await;
}
