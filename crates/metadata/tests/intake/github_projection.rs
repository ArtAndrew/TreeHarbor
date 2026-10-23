use super::*;
use treeharbor_metadata::{
    access::{Authorization, Permission as P, PrincipalKind as K},
    github::{BindingSpec, Command, ProjectionSpec},
};

fn cmd(key: &str) -> Command {
    Command {
        key: key.into(),
        epoch: 1,
    }
}
async fn actor(f: &mut Fixture, name: &str, kind: K, permissions: &[P]) -> Authorization {
    f.db.provision_principal(f.scope.tenant(), name, kind, "operator")
        .await
        .unwrap();
    f.db.set_repository_access(&f.scope, name, permissions, "operator")
        .await
        .unwrap();
    let token =
        f.db.issue_repository_token(&f.scope, name, permissions, 3600, "operator")
            .await
            .unwrap();
    f.db.authorize(&token.secret, &f.scope, permissions[0])
        .await
        .unwrap()
}
struct Projection {
    f: Fixture,
    owner: Authorization,
    bridge: Authorization,
    reader: Authorization,
    binding: Uuid,
    id: Uuid,
    spec: ProjectionSpec,
}
impl Projection {
    async fn new() -> Self {
        let mut f = Fixture::new().await;
        let change = Uuid::new_v4();
        let prepared = f.prepare(b"accepted\0\xff").await;
        f.db.accept_change(
            &context(&f.scope, "accept"),
            &submission(change, f.run, &prepared),
        )
        .await
        .unwrap();
        // Trusted input fixture only: this is NOT authority migration evidence.
        connection(&f.schema)
            .await
            .execute("UPDATE th_repositories SET authority='connected'", &[])
            .await
            .unwrap();
        let owner = actor(&mut f, "human", K::Human, &[P::RepositoryWrite, P::Read]).await;
        let bridge = actor(&mut f, "bridge", K::Bridge, &[P::HostPublish, P::HostRead]).await;
        let token =
            f.db.issue_repository_token(&f.scope, "bridge", &[P::HostRead], 3600, "operator")
                .await
                .unwrap();
        let reader =
            f.db.authorize(&token.secret, &f.scope, P::HostRead)
                .await
                .unwrap();
        let spec: BindingSpec = serde_json::from_value(
            serde_json::from_str::<Value>(include_str!("../fixtures/github-bind-v1.json")).unwrap()
                ["specification"]
                .clone(),
        )
        .unwrap();
        let binding = Uuid::new_v4();
        f.db.bind_github(&owner, &cmd("bind"), binding, &spec)
            .await
            .unwrap();
        Self {
            f,
            owner,
            bridge,
            reader,
            binding,
            id: Uuid::new_v4(),
            spec: ProjectionSpec {
                changeset: change.to_string(),
                version: 1,
                title: "Update selected content".into(),
                ttl_seconds: 60,
            },
        }
    }
    async fn request(&mut self) -> Value {
        self.f
            .db
            .request_github_projection(
                &self.owner,
                &cmd("request"),
                self.id,
                self.binding,
                &self.spec,
            )
            .await
            .unwrap()
    }
    async fn reserve(&mut self) -> (PreparedObject, ObjectLease) {
        self.f
            .db
            .reserve_github_projection(&self.owner, 1, self.id, &self.f.depot)
            .await
            .unwrap()
    }
    async fn ready(&mut self) -> (Value, ObjectLease) {
        self.request().await;
        let (object, lease) = self.reserve().await;
        let receipt = self.f.depot.put(lease.key().generation(), &object).unwrap();
        let result = self
            .f
            .db
            .finish_github_projection(&self.owner, &cmd("prepare"), self.id, &lease, &receipt)
            .await
            .unwrap();
        (result, lease)
    }
    async fn new_version(&mut self) {
        let prepared = self.f.prepare(b"newer version").await;
        let mut submit = submission(self.spec.changeset.parse().unwrap(), self.f.run, &prepared);
        submit.previous_version = 1;
        let ctx = CommandContext::trusted(
            self.f.scope.clone(),
            "accept-v2".into(),
            "agent-owner".into(),
            Authority::Connected,
            1,
        );
        self.f.db.accept_change(&ctx, &submit).await.unwrap();
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact retained proposal projection and export"]
async fn projection_is_exact_replayable_pinned_and_never_a_base_or_merge_permission() {
    let mut p = Projection::new().await;
    let request = p.request().await;
    assert_eq!(request, p.request().await);
    let mut changed = p.spec.clone();
    changed.title = "Different immutable title".into();
    assert!(matches!(
        p.f.db
            .request_github_projection(&p.owner, &cmd("request"), p.id, p.binding, &changed)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    let (object, lease) = p.reserve().await;
    let (again, again_lease) = p.reserve().await;
    assert_eq!(object.id(), again.id());
    assert_eq!(lease.key(), again_lease.key());
    assert_eq!(lease.token(), again_lease.token());
    let receipt = p.f.depot.put(lease.key().generation(), &object).unwrap();
    let raw = connection(&p.f.schema).await;
    let original =
        p.f.db
            .change_version(&p.f.scope, p.spec.changeset.parse().unwrap(), 1)
            .await
            .unwrap();
    let Object::NativeCommit(c) =
        p.f.depot
            .get(lease.key().generation(), object.id())
            .unwrap()
    else {
        panic!()
    };
    let RevisionRef::Native(base) = p.f.revision else {
        panic!()
    };
    assert_eq!(c.parents, vec![base]);
    assert_eq!(c.snapshot.to_string(), original["result_snapshot"]);
    assert_eq!(c.author_name, "TreeHarbor");
    assert!(c.git_origin.is_none());
    let native=raw.query_one("SELECT operations,floor(extract(epoch from accepted_at))::bigint FROM th_change_versions",&[]).await.unwrap();
    assert_eq!(c.changes.as_bytes().to_vec(), native.get::<_, Vec<u8>>(0));
    assert_eq!(c.timestamp_seconds, native.get::<_, i64>(1));
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let command = cmd("prepare");
    let (a, b) = tokio::join!(
        p.f.db
            .finish_github_projection(&p.owner, &command, p.id, &lease, &receipt),
        other.finish_github_projection(&p.owner, &command, p.id, &lease, &receipt)
    );
    let a = a.unwrap();
    assert_eq!(a, b.unwrap());
    assert_eq!(a["publication_ready"], false);
    assert_eq!(a["state"], "ready");
    p.f.db = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    assert_eq!(
        p.f.db
            .github_projection_preparation_replay(&p.owner, &command, p.id)
            .await
            .unwrap(),
        Some(a)
    );
    assert!(
        p.f.db
            .github_projection_preparation_replay(&p.owner, &cmd("new-key"), p.id)
            .await
            .is_err()
    );
    let counts=raw.query_one("SELECT (SELECT count(*) FROM th_bases),(SELECT count(*) FROM th_native_refs),(SELECT count(*) FROM th_native_check_runs),(SELECT count(*) FROM th_outbox WHERE kind='github.projection.ready')",&[]).await.unwrap();
    assert_eq!(
        (
            counts.get::<_, i64>(0),
            counts.get::<_, i64>(1),
            counts.get::<_, i64>(2),
            counts.get::<_, i64>(3)
        ),
        (1, 0, 0, 1)
    );
    raw.execute(
        "UPDATE th_object_holds SET expires_at=clock_timestamp()-interval '1 hour'",
        &[],
    )
    .await
    .unwrap();
    assert!(
        p.f.db
            .try_retire_object(lease.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    let read =
        p.f.db
            .open_github_projection_export(&p.bridge, 1, p.id, 60)
            .await
            .unwrap();
    let objects =
        p.f.db
            .read_source_export(&read, &p.f.depot, &[read.root()], 1_000_000)
            .await
            .unwrap()
            .read()
            .unwrap();
    assert_eq!(objects[0].id(), object.id());
    assert!(
        p.f.db
            .open_github_projection_export(&p.reader, 1, p.id, 60)
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .open_github_projection_export(&p.owner, 1, p.id, 60)
            .await
            .is_err()
    );
    p.f.db.finish_source_export(read).await.unwrap();
    p.f.db
        .cancel_github_projection(&p.owner, &cmd("cancel"), p.id)
        .await
        .unwrap();
    assert!(
        p.f.db
            .try_retire_object(lease.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        p.f.db
            .github_projection_context(&p.bridge, 1, p.id)
            .await
            .is_err()
    );
    assert!(
        raw.execute("UPDATE th_github_projections SET title='changed'", &[])
            .await
            .is_err()
    );
    assert!(
        raw.execute("DELETE FROM th_github_projections", &[])
            .await
            .is_err()
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; changes after disk IO cannot authorize publication"]
async fn new_version_cancel_binding_and_grant_revocation_fence_preparation() {
    for fence in ["version", "cancel", "binding", "grant", "epoch"] {
        let mut p = Projection::new().await;
        p.request().await;
        let (object, lease) = p.reserve().await;
        let receipt = p.f.depot.put(lease.key().generation(), &object).unwrap();
        match fence {
            "version" => p.new_version().await,
            "cancel" => {
                p.f.db
                    .cancel_github_projection(&p.owner, &cmd("cancel"), p.id)
                    .await
                    .unwrap();
            }
            "binding" => {
                p.f.db
                    .revoke_github(&p.owner, &cmd("revoke"), p.binding)
                    .await
                    .unwrap();
            }
            "grant" => {
                p.f.db
                    .set_repository_access(&p.f.scope, "human", &[P::Read], "operator")
                    .await
                    .unwrap();
            }
            _ => {
                connection(&p.f.schema)
                    .await
                    .execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
        }
        assert!(
            p.f.db
                .finish_github_projection(&p.owner, &cmd("prepare"), p.id, &lease, &receipt)
                .await
                .is_err(),
            "{fence}"
        );
        let raw = connection(&p.f.schema).await;
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'github-projection:%'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.projection.ready'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; current purpose and assignment checked before final delivery"]
async fn old_ready_artifact_is_historical_and_export_rechecks_access_and_version() {
    let mut p = Projection::new().await;
    let (ready, _) = p.ready().await;
    let other = actor(&mut p.f, "unassigned", K::Bridge, &[P::HostPublish]).await;
    assert!(
        p.f.db
            .github_projection_context(&other, 1, p.id)
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .request_github_projection(
                &p.bridge,
                &cmd("bad-owner"),
                Uuid::new_v4(),
                p.binding,
                &p.spec
            )
            .await
            .is_err()
    );
    let foreign = Scope::new("foreign", p.f.scope.repository()).unwrap();
    p.f.db
        .create_repository(&foreign, Authority::Connected)
        .await
        .unwrap();
    p.f.db
        .provision_principal(foreign.tenant(), "foreign", K::Bridge, "operator")
        .await
        .unwrap();
    p.f.db
        .set_repository_access(&foreign, "foreign", &[P::HostPublish], "operator")
        .await
        .unwrap();
    let t =
        p.f.db
            .issue_repository_token(&foreign, "foreign", &[P::HostPublish], 60, "operator")
            .await
            .unwrap();
    let a =
        p.f.db
            .authorize(&t.secret, &foreign, P::HostPublish)
            .await
            .unwrap();
    assert!(p.f.db.github_projection_context(&a, 1, p.id).await.is_err());
    let read =
        p.f.db
            .open_github_projection_export(&p.bridge, 1, p.id, 60)
            .await
            .unwrap();
    p.new_version().await;
    assert!(
        p.f.db
            .read_source_export(&read, &p.f.depot, &[read.root()], 1_000_000)
            .await
            .is_err()
    );
    assert!(p.f.db.finish_source_export(read).await.is_err());
    let token =
        p.f.db
            .issue_repository_token(&p.f.scope, "human", &[P::Read], 60, "operator")
            .await
            .unwrap();
    let owner_read =
        p.f.db
            .authorize(&token.secret, &p.f.scope, P::Read)
            .await
            .unwrap();
    let status =
        p.f.db
            .github_projection_status(&owner_read, 1, p.id)
            .await
            .unwrap();
    assert_eq!(status["state"], "ready");
    assert_eq!(status["is_current_version"], false);
    assert_eq!(
        p.f.db
            .github_projection_preparation_replay(&p.owner, &cmd("prepare"), p.id)
            .await
            .unwrap(),
        Some(ready)
    );
    assert!(
        p.f.db
            .request_github_projection(&p.owner, &cmd("old"), Uuid::new_v4(), p.binding, &p.spec)
            .await
            .is_err()
    );
    p.spec.version = 2;
    p.id = Uuid::new_v4();
    p.f.db
        .request_github_projection(&p.owner, &cmd("new"), p.id, p.binding, &p.spec)
        .await
        .unwrap();
    let (obj, lease) = p.reserve().await;
    let receipt = p.f.depot.put(lease.key().generation(), &obj).unwrap();
    p.f.db
        .finish_github_projection(&p.owner, &cmd("new-prepare"), p.id, &lease, &receipt)
        .await
        .unwrap();
    let read =
        p.f.db
            .open_github_projection_export(&p.bridge, 1, p.id, 60)
            .await
            .unwrap();
    p.f.db
        .set_repository_access(&p.f.scope, "bridge", &[P::Read], "operator")
        .await
        .unwrap();
    assert!(p.f.db.finish_source_export(read).await.is_err());
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; expiry during completion must roll back receipt and graph pin"]
async fn expiry_after_receipt_writes_rolls_back_ready_and_request() {
    for (phase, table, kind) in [
        ("request", "th_operations", "github.projection.request.v1"),
        ("request", "th_outbox", "github.projection.requested"),
        ("prepare", "th_operations", "github.projection.prepare.v1"),
        ("prepare", "th_outbox", "github.projection.ready"),
    ] {
        let mut p = Projection::new().await;
        p.spec.ttl_seconds = 1;
        if phase == "prepare" {
            p.request().await;
        }
        let raw = connection(&p.f.schema).await;
        raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_projection() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_projection AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_projection();")).await.unwrap();
        let result = if phase == "request" {
            p.f.db
                .request_github_projection(&p.owner, &cmd("request"), p.id, p.binding, &p.spec)
                .await
        } else {
            let (obj, lease) = p.reserve().await;
            let receipt = p.f.depot.put(lease.key().generation(), &obj).unwrap();
            p.f.db
                .finish_github_projection(&p.owner, &cmd("prepare"), p.id, &lease, &receipt)
                .await
        };
        assert!(result.is_err(), "{phase}");
        assert!(
            raw.query_one("SELECT is_called FROM delay_seen", &[])
                .await
                .unwrap()
                .get::<_, bool>(0),
            "fault boundary was not reached"
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_github_projections WHERE state='ready'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'github-projection:%'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        cleanup(&p.f.schema).await;
    }
}
