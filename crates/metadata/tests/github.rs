mod common;
#[path = "github/events.rs"]
mod events;
#[path = "github/imports.rs"]
mod imports;
use common::{cleanup, connection, setup};
use serde_json::json;
use treeharbor_metadata::{
    Authority, Error, Metadata, Scope,
    access::{Authorization, IssuedToken, Permission as P, PrincipalKind as K},
    github::{BindingSpec, Command, Observation, Upstream},
};
use uuid::Uuid;

struct Fixture {
    schema: String,
    scope: Scope,
    db: Metadata,
    human: IssuedToken,
    bridge: IssuedToken,
    binding: Uuid,
    spec: BindingSpec,
}
async fn token(
    db: &mut Metadata,
    scope: &Scope,
    name: &str,
    kind: K,
    permissions: &[P],
) -> IssuedToken {
    db.provision_principal(scope.tenant(), name, kind, "operator")
        .await
        .unwrap();
    db.set_repository_access(scope, name, permissions, "operator")
        .await
        .unwrap();
    db.issue_repository_token(scope, name, permissions, 3600, "operator")
        .await
        .unwrap()
}
fn cmd(key: &str) -> Command {
    Command {
        key: key.into(),
        epoch: 1,
    }
}
fn spec() -> BindingSpec {
    BindingSpec {
        version: 1,
        app_id: 7,
        client_id: "Iv1_public_test".into(),
        bridge: "bridge".into(),
        upstream: Upstream {
            installation_id: 9,
            repository_id: 11,
            owner_id: 13,
            owner: "example-org".into(),
            name: "demo".into(),
        },
    }
}
fn observation() -> Observation {
    serde_json::from_slice(include_bytes!(
        "../../integrations-github/tests/fixtures/probe-v1.json"
    ))
    .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; populated schema14 upgrade without implicit host.read"]
async fn upgrade_preserves_existing_bridge_credentials_without_expanding_access() {
    use sha2::{Digest, Sha256};
    let schema = format!("th_test_{}", Uuid::new_v4().simple());
    connection("public")
        .await
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
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
        include_str!("../migrations/006_source_uploads.sql"),
        include_str!("../migrations/007_workspace_preparations.sql"),
        include_str!("../migrations/008_managed_runs.sql"),
        include_str!("../migrations/009_change_uploads.sql"),
        include_str!("../migrations/010_native_integration.sql"),
        include_str!("../migrations/011_check_execution.sql"),
        include_str!("../migrations/012_change_rebase.sql"),
        include_str!("../migrations/013_workspace_seals.sql"),
        include_str!("../migrations/014_workspace_cleanup.sql"),
    ] {
        raw.batch_execute(sql).await.unwrap();
        let mut h = Sha256::new();
        h.update(digest);
        h.update(sql.as_bytes());
        digest = h.finalize().to_vec();
    }
    raw.execute(
        "INSERT INTO th_schema_version(version,migration_digest) VALUES(14,$1)",
        &[&digest],
    )
    .await
    .unwrap();
    let mut db = Metadata::new(connection(&schema).await).await.unwrap();
    let scope = Scope::new("upgrade", Uuid::new_v4()).unwrap();
    db.create_repository(&scope, Authority::Connected)
        .await
        .unwrap();
    let existing = token(
        &mut db,
        &scope,
        "bridge",
        K::Bridge,
        &[P::Read, P::HostPublish],
    )
    .await;
    let snapshot_sql = "SELECT jsonb_build_object('principals',(SELECT jsonb_agg(to_jsonb(p)) FROM th_principals p),'access',(SELECT jsonb_agg(to_jsonb(a)) FROM th_repository_access a),'tokens',(SELECT jsonb_agg(to_jsonb(t)) FROM th_access_tokens t),'repositories',(SELECT jsonb_agg(to_jsonb(r)) FROM th_repositories r))";
    let before: serde_json::Value = raw.query_one(snapshot_sql, &[]).await.unwrap().get(0);
    assert!(matches!(
        db.check_schema().await,
        Err(Error::SchemaMismatch)
    ));
    db.migrate().await.unwrap();
    db.migrate().await.unwrap();
    db.check_schema().await.unwrap();
    let after: serde_json::Value = raw.query_one(snapshot_sql, &[]).await.unwrap().get(0);
    assert_eq!(before, after);
    db.authorize(&existing.secret, &scope, P::HostPublish)
        .await
        .unwrap();
    assert!(
        db.authorize(&existing.secret, &scope, P::HostRead)
            .await
            .is_err()
    );
    db.set_repository_access(
        &scope,
        "bridge",
        &[P::Read, P::HostRead, P::HostPublish],
        "operator",
    )
    .await
    .unwrap();
    // A new grant alone must not widen a previously issued credential.
    assert!(
        db.authorize(&existing.secret, &scope, P::HostRead)
            .await
            .is_err()
    );
    let new = db
        .issue_repository_token(&scope, "bridge", &[P::HostRead], 3600, "operator")
        .await
        .unwrap();
    db.authorize(&new.secret, &scope, P::HostRead)
        .await
        .unwrap();
    assert!(
        db.authorize(&new.secret, &scope, P::HostPublish)
            .await
            .is_err()
    );
    cleanup(&schema).await;
}
impl Fixture {
    async fn new() -> Self {
        let (schema, _, mut db) = setup().await;
        let scope = Scope::new("test-tenant", Uuid::new_v4()).unwrap();
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
        Self {
            schema,
            scope,
            db,
            human,
            bridge,
            binding: Uuid::new_v4(),
            spec: spec(),
        }
    }
    async fn owner(&mut self) -> Authorization {
        self.db
            .authorize(&self.human.secret, &self.scope, P::RepositoryWrite)
            .await
            .unwrap()
    }
    async fn reader(&mut self) -> Authorization {
        self.db
            .authorize(&self.human.secret, &self.scope, P::Read)
            .await
            .unwrap()
    }
    async fn bridge(&mut self) -> Authorization {
        self.db
            .authorize(&self.bridge.secret, &self.scope, P::HostRead)
            .await
            .unwrap()
    }
    async fn bind(&mut self) {
        let auth = self.owner().await;
        self.db
            .bind_github(&auth, &cmd("bind"), self.binding, &self.spec)
            .await
            .unwrap();
    }
    async fn begin(&mut self, id: Uuid) -> Authorization {
        let auth = self.bridge().await;
        self.db
            .begin_github_probe(&auth, &cmd(&format!("start-{id}")), self.binding, id)
            .await
            .unwrap();
        auth
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; real durable Connected contracts"]
async fn binding_and_observation_survive_lost_responses_without_granting_publication() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    let auth = f.begin(id).await;
    let first =
        f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, id, &observation())
            .await
            .unwrap();
    f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
    f.db.check_schema().await.unwrap();
    let auth = f.bridge().await;
    assert_eq!(
        f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, id, &observation())
            .await
            .unwrap(),
        first
    );
    let context =
        f.db.github_probe_context(&auth, 1, f.binding, id)
            .await
            .unwrap();
    assert_eq!(context["observation"], json!(observation()));
    assert_eq!(context["publication_ready"], false);
    let mut other = observation();
    other.archived = true;
    assert!(matches!(
        f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, id, &other)
            .await,
        Err(Error::IdempotencyConflict)
    ));
    assert!(matches!(
        f.db.finish_github_probe(&auth, &cmd("another-key"), f.binding, id, &other)
            .await,
        Err(Error::HostBindingState)
    ));
    let raw = connection(&f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='github.connection.observed'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_native_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert!(
        raw.execute("UPDATE th_github_bindings SET specification='{}'", &[])
            .await
            .is_err()
    );
    assert!(
        raw.execute("UPDATE th_github_probes SET observation='{}'", &[])
            .await
            .is_err()
    );
    assert!(
        raw.execute("DELETE FROM th_github_probes", &[])
            .await
            .is_err()
    );
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; real durable Connected contracts"]
async fn revocation_and_replacement_fence_pending_reports_and_keep_old_receipts() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    let bridge = f.begin(id).await;
    let owner = f.owner().await;
    f.db.revoke_github(&owner, &cmd("revoke"), f.binding)
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_github_probe(&bridge, &cmd("finish"), f.binding, id, &observation())
            .await,
        Err(Error::HostBindingState)
    ));
    assert!(matches!(
        f.db.github_probe_context(&bridge, 1, f.binding, id).await,
        Err(Error::HostBindingState)
    ));
    let reader = f.reader().await;
    assert_eq!(
        f.db.github_binding_status(&reader, 1, f.binding)
            .await
            .unwrap()["state"],
        "revoked"
    );
    let replay =
        f.db.bind_github(&owner, &cmd("bind"), f.binding, &f.spec)
            .await
            .unwrap();
    assert_eq!(replay["outcome"], "configured"); // old receipt; no reactivation
    assert_eq!(
        f.db.github_binding_status(&reader, 1, f.binding)
            .await
            .unwrap()["state"],
        "revoked"
    );
    let replacement = Uuid::new_v4();
    f.db.bind_github(&owner, &cmd("replacement"), replacement, &f.spec)
        .await
        .unwrap();
    assert!(matches!(
        f.db.bind_github(&owner, &cmd("old-id"), f.binding, &f.spec)
            .await,
        Err(Error::HostBindingState)
    ));
    assert!(matches!(
        f.db.github_probe_context(&bridge, 1, replacement, id).await,
        Err(Error::HostBindingState)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; real durable Connected contracts"]
async fn current_bridge_scope_credential_authority_and_epoch_are_required() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    let auth = f.begin(id).await;
    let other = token(&mut f.db, &f.scope, "other", K::Bridge, &[P::HostRead]).await;
    let other =
        f.db.authorize(&other.secret, &f.scope, P::HostRead)
            .await
            .unwrap();
    assert!(matches!(
        f.db.github_probe_context(&other, 1, f.binding, id).await,
        Err(Error::Forbidden)
    ));
    assert!(
        f.db.authorize(&f.bridge.secret, &f.scope, P::HostPublish)
            .await
            .is_err()
    );
    assert!(
        f.db.authorize(&f.bridge.secret, &f.scope, P::RepositoryWrite)
            .await
            .is_err()
    );
    f.db.provision_principal(f.scope.tenant(), "agent", K::Agent, "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.set_repository_access(&f.scope, "agent", &[P::HostRead], "operator")
            .await,
        Err(Error::Forbidden)
    ));
    let raw = connection(&f.schema).await;
    raw.execute(
        "UPDATE th_repositories SET epoch=2 WHERE tenant=$1 AND repository=$2",
        &[&f.scope.tenant(), &f.scope.repository()],
    )
    .await
    .unwrap();
    assert!(matches!(
        f.db.finish_github_probe(&auth, &cmd("stale"), f.binding, id, &observation())
            .await,
        Err(Error::Fenced)
    ));
    assert!(matches!(
        f.db.github_probe_context(&auth, 2, f.binding, id).await,
        Err(Error::HostBindingState)
    ));
    raw.execute("UPDATE th_repositories SET epoch=1,authority='standalone' WHERE tenant=$1 AND repository=$2",&[&f.scope.tenant(),&f.scope.repository()]).await.unwrap();
    assert!(matches!(
        f.db.github_probe_context(&auth, 1, f.binding, id).await,
        Err(Error::Fenced)
    ));
    raw.execute(
        "UPDATE th_repositories SET authority='connected' WHERE tenant=$1 AND repository=$2",
        &[&f.scope.tenant(), &f.scope.repository()],
    )
    .await
    .unwrap();
    f.db.set_repository_access(&f.scope, "bridge", &[], "operator")
        .await
        .unwrap();
    assert!(matches!(
        f.db.finish_github_probe(&auth, &cmd("revoked"), f.binding, id, &observation())
            .await,
        Err(Error::Forbidden)
    ));
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; real durable Connected contracts"]
async fn concurrent_bindings_and_reports_have_one_committed_effect() {
    let mut f = Fixture::new().await;
    let auth = f.owner().await;
    let mut second = Metadata::new(connection(&f.schema).await).await.unwrap();
    let id = Uuid::new_v4();
    let a = cmd("a");
    let b = cmd("b");
    let (left, right) = tokio::join!(
        f.db.bind_github(&auth, &a, f.binding, &f.spec),
        second.bind_github(&auth, &b, id, &f.spec)
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    if right.is_ok() {
        f.binding = id;
    }
    let probe = Uuid::new_v4();
    let auth = f.begin(probe).await;
    let observation = observation();
    let a = cmd("finish-a");
    let b = cmd("finish-b");
    let (left, right) = tokio::join!(
        f.db.finish_github_probe(&auth, &a, f.binding, probe, &observation),
        second.finish_github_probe(&auth, &b, f.binding, probe, &observation)
    );
    assert_eq!(usize::from(left.is_ok()) + usize::from(right.is_ok()), 1);
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; real durable Connected contracts"]
async fn incorrect_observations_do_not_change_pending_attempt_or_policy() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let id = Uuid::new_v4();
    let auth = f.begin(id).await;
    let mut cases = vec![];
    let mut v = observation();
    v.binding.repository_id += 1;
    cases.push(v);
    let mut v = observation();
    v.app_id += 1;
    cases.push(v);
    let mut v = observation();
    v.publication_ready = true;
    cases.push(v);
    let mut v = observation();
    v.queue_configuration_verified = true;
    cases.push(v);
    let mut v = observation();
    v.probe_token_revoked = false;
    cases.push(v);
    for v in cases {
        assert!(matches!(
            f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, id, &v)
                .await,
            Err(Error::Invalid(_))
        ));
    }
    assert_eq!(
        f.db.github_probe_context(&auth, 1, f.binding, id)
            .await
            .unwrap()["state"],
        "pending"
    );
    f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, id, &observation())
        .await
        .unwrap();
    cleanup(&f.schema).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL; exact post-write deadline contracts"]
async fn expiration_during_observation_receipt_or_outbox_write_rolls_back_every_effect() {
    for target in ["observation", "receipt", "outbox", "begin"] {
        let mut f = Fixture::new().await;
        f.bind().await;
        let raw = connection(&f.schema).await;
        raw.batch_execute("CREATE FUNCTION short_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN NEW.expires_at=clock_timestamp()+interval '150 milliseconds'; RETURN NEW; END; $$; CREATE TRIGGER short_probe BEFORE INSERT ON th_github_probes FOR EACH ROW EXECUTE FUNCTION short_probe();").await.unwrap();
        let id = Uuid::new_v4();
        let auth = f.bridge().await;
        if target != "begin" {
            f.db.begin_github_probe(&auth, &cmd("start"), f.binding, id)
                .await
                .unwrap();
        }
        let (table, event, condition) = match target {
            "observation" => ("th_github_probes", "UPDATE", "NEW.state='observed'"),
            "receipt" => (
                "th_operations",
                "INSERT",
                "NEW.kind='github.probe.finish.v1'",
            ),
            "outbox" => (
                "th_outbox",
                "INSERT",
                "NEW.kind='github.connection.observed'",
            ),
            _ => (
                "th_operations",
                "INSERT",
                "NEW.kind='github.probe.begin.v1'",
            ),
        };
        raw.batch_execute(&format!("CREATE FUNCTION delay_probe() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF {condition} THEN PERFORM pg_sleep(0.25); END IF; RETURN NEW; END; $$; CREATE TRIGGER delay_probe AFTER {event} ON {table} FOR EACH ROW EXECUTE FUNCTION delay_probe();")).await.unwrap();
        let result = if target == "begin" {
            f.db.begin_github_probe(&auth, &cmd("start"), f.binding, id)
                .await
        } else {
            f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, id, &observation())
                .await
        };
        assert!(
            matches!(result, Err(Error::HostBindingState)),
            "{target}: {result:?}"
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_github_probes WHERE state='observed'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_operations WHERE kind='github.probe.finish.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.connection.observed'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        if target == "begin" {
            assert_eq!(
                raw.query_one("SELECT count(*) FROM th_github_probes", &[])
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
#[ignore = "requires disposable PostgreSQL; exact scope and bounded lifecycle contracts"]
async fn pending_attempt_budget_and_cross_repository_access_are_enforced() {
    let mut f = Fixture::new().await;
    f.bind().await;
    let mut ids = vec![];
    for _ in 0..4 {
        let id = Uuid::new_v4();
        f.begin(id).await;
        ids.push(id);
    }
    let auth = f.bridge().await;
    assert!(matches!(
        f.db.begin_github_probe(&auth, &cmd("fifth"), f.binding, Uuid::new_v4())
            .await,
        Err(Error::HostBindingState)
    ));
    let foreign = Scope::new("other-tenant", Uuid::new_v4()).unwrap();
    f.db.create_repository(&foreign, Authority::Connected)
        .await
        .unwrap();
    let issued = token(
        &mut f.db,
        &foreign,
        "bridge",
        K::Bridge,
        &[P::Read, P::HostRead],
    )
    .await;
    let foreign_auth =
        f.db.authorize(&issued.secret, &foreign, P::HostRead)
            .await
            .unwrap();
    assert!(matches!(
        f.db.github_probe_context(&foreign_auth, 1, f.binding, ids[0])
            .await,
        Err(Error::NotFound)
    ));
    f.db.finish_github_probe(&auth, &cmd("finish"), f.binding, ids[0], &observation())
        .await
        .unwrap();
    f.db.begin_github_probe(&auth, &cmd("new-slot"), f.binding, Uuid::new_v4())
        .await
        .unwrap();
    cleanup(&f.schema).await;
}
