use super::*;
use treeharbor_core::{Changes, Commit, Directory, GitOid, GitOrigin, Object, RevisionRef};
use treeharbor_metadata::github::ImportSpec;
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
fn spec_import() -> ImportSpec {
    ImportSpec {
        git_revision: RevisionRef::Git(GitOid::Sha1([7; 20])).to_string(),
        max_objects: 100,
        max_bytes: 1024 * 1024,
        ttl_seconds: 3600,
    }
}
// Trusted converter fixture, not proof of a GitHub or actual Git transcript.
fn graph() -> Vec<Object> {
    let raw = Object::Blob(b"trusted converter fixture".to_vec());
    let dir = Object::Directory(Directory::new(vec![]).unwrap());
    let snapshot = Object::Snapshot {
        root: dir.id().unwrap(),
    };
    let changes = Object::ChangeOperations(Changes::new(vec![]).unwrap());
    let commit = Object::NativeCommit(Commit {
        git_origin: Some(Box::new(GitOrigin {
            oid: GitOid::Sha1([7; 20]),
            raw: raw.id().unwrap(),
        })),
        parents: vec![],
        snapshot: snapshot.id().unwrap(),
        changes: changes.id().unwrap(),
        author_name: "Test".into(),
        author_email: "test@example.invalid".into(),
        timestamp_seconds: 0,
        timezone_minutes: 0,
        message: "fixture".into(),
    });
    vec![raw, dir, snapshot, changes, commit]
}
fn store(f: &Fixture) -> (tempfile::TempDir, ManagedStore) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let store = ManagedStore::open(
        dir.path(),
        TenantId::new(f.scope.tenant()).unwrap(),
        "local-1",
    )
    .unwrap();
    (dir, store)
}
async fn request(f: &mut Fixture, id: Uuid, spec: &ImportSpec) -> serde_json::Value {
    let owner = f.owner().await;
    f.db.request_github_import(&owner, &cmd(&format!("request-{id}")), id, f.binding, spec)
        .await
        .unwrap()
}
async fn begin(f: &mut Fixture, id: Uuid, graph: &[Object]) -> Authorization {
    let bridge = f.bridge().await;
    f.db.begin_github_import_upload(
        &bridge,
        &cmd(&format!("begin-{id}")),
        id,
        "local-1",
        graph.last().unwrap().id().unwrap(),
    )
    .await
    .unwrap();
    bridge
}
async fn put(
    f: &mut Fixture,
    auth: &Authorization,
    id: Uuid,
    store: &ManagedStore,
    object: &Object,
) {
    let prepared =
        PreparedObject::from_frame(object.id().unwrap(), object.encode().unwrap()).unwrap();
    let ticket =
        f.db.reserve_github_source_object(auth, id, store, &prepared)
            .await
            .unwrap();
    let receipt = store.put(ticket.generation(), &prepared).unwrap();
    f.db.publish_github_source_object(auth, &ticket, &receipt)
        .await
        .unwrap();
}
#[tokio::test]
#[ignore = "requires PostgreSQL; exact GitHub import receipt, scope and durable graph"]
async fn github_import_completion_retains_exact_source_and_replays_without_writing_main() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    request(&mut f, id, &spec_import()).await;
    let graph = graph();
    let bridge = begin(&mut f, id, &graph).await;
    let (_dir, store) = store(&f);
    for object in &graph {
        put(&mut f, &bridge, id, &store, object).await;
    }
    let result =
        f.db.finish_github_import(&bridge, &cmd("finish-import"), id)
            .await
            .unwrap();
    assert_eq!(result["git_revision"], spec_import().git_revision);
    assert_eq!(result["publication_ready"], false);
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    let bridge = f.bridge().await;
    assert_eq!(
        f.db.finish_github_import(&bridge, &cmd("finish-import"), id)
            .await
            .unwrap(),
        result
    );
    assert_eq!(
        f.db.github_import_context(&bridge, 1, id).await.unwrap()["state"],
        "completed"
    );
    assert!(matches!(
        f.db.source_upload_status(&bridge, id).await,
        Err(Error::Forbidden)
    ));
    let owner = f.owner().await;
    assert!(f.db.source_upload_status(&owner, id).await.is_err());
    assert!(
        f.db.begin_github_import_upload(
            &owner,
            &cmd("wrong-role"),
            id,
            "local-1",
            graph.last().unwrap().id().unwrap()
        )
        .await
        .is_err()
    );
    let raw = connection(&f.schema).await;
    for (sql, count) in [
        ("SELECT count(*) FROM th_native_refs", 0),
        ("SELECT count(*) FROM th_bases", 1),
        (
            "SELECT count(*) FROM th_outbox WHERE kind='github.import.completed'",
            1,
        ),
    ] {
        assert_eq!(
            raw.query_one(sql, &[]).await.unwrap().get::<_, i64>(0),
            count
        );
    }
    // Finished base pin retains every reachable object after upload holds are gone.
    raw.execute("DELETE FROM th_object_holds", &[])
        .await
        .unwrap();
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'base:%'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    for object in &graph {
        let prepared = PreparedObject::new(object).unwrap();
        let lease =
            f.db.reserve_object(&store, &prepared, Uuid::new_v4(), 60)
                .await
                .unwrap();
        f.db.release_object_hold(&lease).await.unwrap();
        assert!(
            f.db.try_retire_object(lease.key(), 0)
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .get(lease.key().generation(), object.id().unwrap())
                .unwrap(),
            *object
        );
    }
    f.db.revoke_github(&owner, &cmd("revoke"), f.binding)
        .await
        .unwrap();
    assert!(f.db.github_import_context(&bridge, 1, id).await.is_err());
    assert_eq!(
        f.db.finish_github_import(&bridge, &cmd("finish-import"), id)
            .await
            .unwrap(),
        result
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; no acceptance of a substituted root Git identity"]
async fn wrong_git_origin_or_missing_graph_never_registers_a_source_base() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    let mut spec = spec_import();
    spec.git_revision = RevisionRef::Git(GitOid::Sha1([9; 20])).to_string();
    request(&mut f, id, &spec).await;
    let graph = graph();
    let bridge = begin(&mut f, id, &graph).await;
    let (_dir, store) = store(&f);
    assert!(matches!(
        f.db.finish_github_import(&bridge, &cmd("finish"), id).await,
        Err(Error::UploadMembership)
    ));
    let root = PreparedObject::new(graph.last().unwrap()).unwrap();
    assert!(matches!(
        f.db.reserve_github_source_object(&bridge, id, &store, &root)
            .await,
        Err(Error::UploadMembership)
    ));
    for object in &graph {
        put(&mut f, &bridge, id, &store, object).await;
    }
    assert!(matches!(
        f.db.finish_github_import(&bridge, &cmd("finish"), id).await,
        Err(Error::UploadMembership)
    ));
    assert_eq!(
        connection(&f.schema)
            .await
            .query_one("SELECT count(*) FROM th_bases", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&f.schema).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; object IO crosses revocation/cancellation/epoch fences"]
async fn publication_after_disk_write_checks_current_binding_import_and_credentials() {
    for fence in ["revoke", "cancel", "epoch", "grant"] {
        let mut f = Fixture::new().await;
        f.bind().await;
        let id = Uuid::new_v4();
        request(&mut f, id, &spec_import()).await;
        let graph = graph();
        let bridge = begin(&mut f, id, &graph).await;
        let (_dir, store) = store(&f);
        let object = PreparedObject::new(&graph[0]).unwrap();
        let ticket =
            f.db.reserve_github_source_object(&bridge, id, &store, &object)
                .await
                .unwrap();
        let receipt = store.put(ticket.generation(), &object).unwrap();
        let owner = f.owner().await;
        match fence {
            "revoke" => {
                f.db.revoke_github(&owner, &cmd("revoke"), f.binding)
                    .await
                    .unwrap();
            }
            "cancel" => {
                f.db.cancel_github_import(&owner, &cmd("cancel"), id)
                    .await
                    .unwrap();
            }
            "epoch" => {
                connection(&f.schema)
                    .await
                    .execute(
                        "UPDATE th_repositories SET epoch=epoch+1 WHERE repository=$1",
                        &[&f.scope.repository()],
                    )
                    .await
                    .unwrap();
            }
            _ => {
                f.db.set_repository_access(&f.scope, "bridge", &[], "operator")
                    .await
                    .unwrap();
            }
        }
        assert!(
            f.db.publish_github_source_object(&bridge, &ticket, &receipt)
                .await
                .is_err(),
            "{fence}"
        );
        assert!(
            f.db.finish_github_import(&bridge, &cmd("finish"), id)
                .await
                .is_err()
        );
        assert_eq!(
            connection(&f.schema)
                .await
                .query_one(
                    "SELECT count(*) FROM th_source_upload_items WHERE complete",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        cleanup(&f.schema).await;
    }
}
#[tokio::test]
#[ignore = "requires PostgreSQL; concurrent exact completion and request budget"]
async fn concurrent_import_finishes_have_one_effect_and_cancelled_ids_cannot_be_reused() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    request(&mut f, id, &spec_import()).await;
    let graph = graph();
    let bridge = begin(&mut f, id, &graph).await;
    let (_dir, store) = store(&f);
    for object in &graph {
        put(&mut f, &bridge, id, &store, object).await;
    }
    let mut other = Metadata::new(connection(&f.schema).await).await.unwrap();
    let command = cmd("finish");
    let (a, b) = tokio::join!(
        f.db.finish_github_import(&bridge, &command, id),
        other.finish_github_import(&bridge, &command, id)
    );
    assert_eq!(a.unwrap(), b.unwrap());
    let next = Uuid::new_v4();
    request(&mut f, next, &spec_import()).await;
    let owner = f.owner().await;
    f.db.cancel_github_import(&owner, &cmd("cancel"), next)
        .await
        .unwrap();
    assert!(matches!(
        f.db.request_github_import(&owner, &cmd("reuse"), next, f.binding, &spec_import())
            .await,
        Err(Error::UploadState)
    ));
    for _ in 0..4 {
        request(&mut f, Uuid::new_v4(), &spec_import()).await;
    }
    assert!(matches!(
        f.db.request_github_import(
            &owner,
            &cmd("over-budget"),
            Uuid::new_v4(),
            f.binding,
            &spec_import()
        )
        .await,
        Err(Error::UploadLimit)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; expiry after writes must roll back source acceptance"]
async fn expiry_during_import_request_begin_finish_receipt_and_outbox_rolls_back() {
    for target in ["request", "begin", "source", "receipt", "outbox"] {
        let mut f = Fixture::new().await;
        f.bind().await;
        let raw = connection(&f.schema).await;
        raw.batch_execute("CREATE FUNCTION short_import() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.expires_at=clock_timestamp()+interval '1 second'; RETURN NEW; END; $$; CREATE TRIGGER short_import BEFORE INSERT ON th_github_imports FOR EACH ROW EXECUTE FUNCTION short_import();").await.unwrap();
        let id = Uuid::new_v4();
        let graph = graph();
        let owner = f.owner().await;
        let bridge = f.bridge().await;
        let (_dir, store) = store(&f);
        if target != "request" {
            request(&mut f, id, &spec_import()).await;
        }
        if !matches!(target, "request" | "begin") {
            begin(&mut f, id, &graph).await;
            for object in &graph {
                put(&mut f, &bridge, id, &store, object).await;
            }
        }
        let (table, event, condition) = match target {
            "request" => (
                "th_operations",
                "INSERT",
                "NEW.kind='github.import.request.v1'",
            ),
            "begin" => (
                "th_operations",
                "INSERT",
                "NEW.kind='github.import.begin.v1'",
            ),
            "source" => ("th_github_imports", "UPDATE", "NEW.state='completed'"),
            "receipt" => (
                "th_operations",
                "INSERT",
                "NEW.kind='github.import.finish.v1'",
            ),
            _ => ("th_outbox", "INSERT", "NEW.kind='github.import.completed'"),
        };
        raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_import() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF {condition} THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END; $$; CREATE TRIGGER delay_import AFTER {event} ON {table} FOR EACH ROW EXECUTE FUNCTION delay_import();")).await.unwrap();
        let result = match target {
            "request" => {
                f.db.request_github_import(&owner, &cmd("deadline"), id, f.binding, &spec_import())
                    .await
            }
            "begin" => {
                f.db.begin_github_import_upload(
                    &bridge,
                    &cmd("deadline"),
                    id,
                    "local-1",
                    graph.last().unwrap().id().unwrap(),
                )
                .await
            }
            _ => {
                f.db.finish_github_import(&bridge, &cmd("deadline"), id)
                    .await
            }
        };
        assert!(
            matches!(result, Err(Error::UploadState)),
            "{target}: {result:?}"
        );
        assert!(
            raw.query_one("SELECT is_called FROM delay_seen", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        for sql in [
            "SELECT count(*) FROM th_bases",
            "SELECT count(*) FROM th_github_imports WHERE state='completed'",
            "SELECT count(*) FROM th_operations WHERE key='deadline'",
            "SELECT count(*) FROM th_outbox WHERE kind='github.import.completed'",
        ] {
            assert_eq!(
                raw.query_one(sql, &[]).await.unwrap().get::<_, i64>(0),
                0,
                "{target}: {sql}"
            );
        }
        if target == "request" {
            assert_eq!(
                raw.query_one("SELECT count(*) FROM th_github_imports", &[])
                    .await
                    .unwrap()
                    .get::<_, i64>(0),
                0
            );
        }
        cleanup(&f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated schema15 bindings and source uploads survive migration"]
async fn upgrade_preserves_binding_observation_and_in_progress_human_source_upload() {
    upgrade(15).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; populated schema16 import and credentials survive projection migration"]
async fn projection_upgrade_preserves_import_and_does_not_grant_host_publish() {
    upgrade(16).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; populated schema17 imports/bindings and scoped credentials survive publication migration"]
async fn publication_upgrade_preserves_imports_and_scoped_credentials() {
    upgrade(17).await;
}
#[tokio::test]
#[ignore = "requires PostgreSQL; schema18 imports and credentials survive pull migration"]
async fn pull_upgrade_preserves_imports_and_scoped_credentials() {
    upgrade(18).await;
}
async fn upgrade(version: i32) {
    use sha2::{Digest, Sha256};
    use treeharbor_metadata::uploads::UploadLimits;
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    let raw = connection(&schema).await;
    let mut digest = Vec::new();
    let migrations = [
        include_str!("../../migrations/001_transactions.sql"),
        include_str!("../../migrations/002_object_catalog.sql"),
        include_str!("../../migrations/003_change_intake.sql"),
        include_str!("../../migrations/004_access_control.sql"),
        include_str!("../../migrations/005_source_profiles.sql"),
        include_str!("../../migrations/006_source_uploads.sql"),
        include_str!("../../migrations/007_workspace_preparations.sql"),
        include_str!("../../migrations/008_managed_runs.sql"),
        include_str!("../../migrations/009_change_uploads.sql"),
        include_str!("../../migrations/010_native_integration.sql"),
        include_str!("../../migrations/011_check_execution.sql"),
        include_str!("../../migrations/012_change_rebase.sql"),
        include_str!("../../migrations/013_workspace_seals.sql"),
        include_str!("../../migrations/014_workspace_cleanup.sql"),
        include_str!("../../migrations/015_github_bindings.sql"),
        include_str!("../../migrations/016_github_imports.sql"),
        include_str!("../../migrations/017_github_projections.sql"),
        include_str!("../../migrations/018_github_publications.sql"),
    ];
    for sql in migrations.into_iter().take(version as usize) {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES($1,$2)",
        &[&version, &digest],
    )
    .await
    .unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Connected)
        .await
        .unwrap();
    let human = token(
        &mut db,
        &scope,
        "owner",
        K::Human,
        &[P::Read, P::RepositoryWrite],
    )
    .await;
    let bridge = token(
        &mut db,
        &scope,
        "bridge",
        K::Bridge,
        &[P::Read, P::HostRead],
    )
    .await;
    let mut f = Fixture {
        schema,
        scope,
        db,
        human,
        bridge,
        binding: Uuid::new_v4(),
        spec: spec(),
    };
    f.bind().await;
    let probe = Uuid::new_v4();
    let bridge = f.begin(probe).await;
    let observed =
        f.db.finish_github_probe(&bridge, &cmd("observed"), f.binding, probe, &observation())
            .await
            .unwrap();
    let owner = f.owner().await;
    let upload = Uuid::new_v4();
    let ctx = owner.context("human-begin".into(), Authority::Connected, 1);
    f.db.begin_source_upload(
        &ctx,
        "local-1",
        upload,
        &UploadLimits {
            objects: 100,
            bytes: 1 << 20,
            ttl_seconds: 3600,
        },
    )
    .await
    .unwrap();
    let (_dir, store) = store(&f);
    let graph = graph();
    for object in &graph[..2] {
        let prepared = PreparedObject::new(object).unwrap();
        let ticket =
            f.db.reserve_source_object(&owner, upload, &store, &prepared)
                .await
                .unwrap();
        let receipt = store.put(ticket.generation(), &prepared).unwrap();
        f.db.publish_source_object(&owner, &ticket, &receipt)
            .await
            .unwrap();
    }
    let pending = Uuid::new_v4();
    let before_import = if version == 16 {
        request(&mut f, pending, &spec_import()).await;
        begin(&mut f, pending, &graph).await;
        put(&mut f, &bridge, pending, &store, &graph[0]).await;
        Some(
            f.db.github_source_upload_status(&bridge, pending)
                .await
                .unwrap(),
        )
    } else {
        None
    };
    let snapshot = "SELECT jsonb_build_object('bindings',(SELECT jsonb_agg(to_jsonb(b)) FROM th_github_bindings b),'probes',(SELECT jsonb_agg(to_jsonb(p)) FROM th_github_probes p),'tokens',(SELECT jsonb_agg(to_jsonb(t)) FROM th_access_tokens t),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o))";
    let before: serde_json::Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    let before_upload = f.db.source_upload_status(&owner, upload).await.unwrap();
    assert!(matches!(
        f.db.check_schema().await,
        Err(Error::SchemaMismatch)
    ));
    f.db.migrate().await.unwrap();
    f.db.migrate().await.unwrap();
    f.db.check_schema().await.unwrap();
    if let Some(status) = before_import {
        assert_eq!(
            f.db.github_source_upload_status(&bridge, pending)
                .await
                .unwrap(),
            status
        );
        for object in &graph[1..] {
            put(&mut f, &bridge, pending, &store, object).await;
        }
        // Finish below the snapshot comparison so that new receipts are excluded.
    }
    assert!(
        f.db.authorize(&f.bridge.secret, &f.scope, P::HostPublish)
            .await
            .is_err()
    );
    let after: serde_json::Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    assert_eq!(before, after);
    if version == 16 {
        f.db.finish_github_import(&bridge, &cmd("old-import-finish"), pending)
            .await
            .unwrap();
    }
    assert_eq!(
        f.db.source_upload_status(&owner, upload).await.unwrap(),
        before_upload
    );
    assert_eq!(
        f.db.finish_github_probe(&bridge, &cmd("observed"), f.binding, probe, &observation())
            .await
            .unwrap(),
        observed
    );
    for object in &graph[2..] {
        let prepared = PreparedObject::new(object).unwrap();
        let ticket =
            f.db.reserve_source_object(&owner, upload, &store, &prepared)
                .await
                .unwrap();
        let receipt = store.put(ticket.generation(), &prepared).unwrap();
        f.db.publish_source_object(&owner, &ticket, &receipt)
            .await
            .unwrap();
    }
    f.db.finish_source_upload(
        &owner.context("human-finish".into(), Authority::Connected, 1),
        upload,
        graph.last().unwrap().id().unwrap(),
    )
    .await
    .unwrap();
    let id = Uuid::new_v4();
    request(&mut f, id, &spec_import()).await;
    begin(&mut f, id, &graph).await;
    assert!(
        f.db.github_source_upload_status(&bridge, upload)
            .await
            .is_err()
    );
    assert!(f.db.source_upload_status(&owner, id).await.is_err());
    for object in &graph {
        put(&mut f, &bridge, id, &store, object).await;
    }
    f.db.finish_github_import(&bridge, &cmd("bridge-finish"), id)
        .await
        .unwrap();
    cleanup(&f.schema).await;
}
