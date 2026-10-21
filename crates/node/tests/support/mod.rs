#[path = "../../../metadata/tests/common/mod.rs"]
pub mod database;
use std::{fs, os::unix::fs::PermissionsExt};
use treeharbor_core::*;
use treeharbor_metadata::{
    Authority, Metadata, Scope,
    access::{Authorization, IssuedToken, Permission, PrincipalKind},
    intake::{CommandContext, WorkspaceFormat, WorkspaceSpec},
};
use treeharbor_node::Node;
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

pub fn temporary() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
pub struct Fixture {
    pub schema: String,
    pub scope: Scope,
    pub db: Metadata,
    pub token: IssuedToken,
    pub auth: Authorization,
    pub objects: tempfile::TempDir,
    pub store: ManagedStore,
    pub directory: tempfile::TempDir,
    pub node: Option<Node>,
    pub storage: Uuid,
    pub workspace: Uuid,
    pub base: RevisionRef,
}
impl Fixture {
    pub async fn new() -> Self {
        let (schema, scope, mut db) = database::setup().await;
        let objects = temporary();
        let store = ManagedStore::open(
            objects.path(),
            TenantId::new(scope.tenant()).unwrap(),
            "node-tests",
        )
        .unwrap();
        let blob = Object::Blob(b"base contents".to_vec());
        let directory = Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"a".to_vec()).unwrap(),
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
        let commit = Commit {
            parents: vec![],
            snapshot: snapshot.id().unwrap(),
            changes: changes.id().unwrap(),
            author_name: "Fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: "fixture".into(),
        };
        let native = Object::NativeCommit(commit.clone());
        let base = RevisionRef::Native(native.id().unwrap());
        let mut leases = Vec::new();
        for object in [blob, directory, snapshot, changes, native] {
            let prepared = PreparedObject::new(&object).unwrap();
            let lease = db
                .reserve_object(&store, &prepared, Uuid::new_v4(), 60)
                .await
                .unwrap();
            if lease.requires_upload() {
                db.publish_object(
                    &lease,
                    &store.put(lease.key().generation(), &prepared).unwrap(),
                )
                .await
                .unwrap();
            }
            leases.push(lease);
        }
        let ctx = |key: &str| {
            CommandContext::trusted(
                scope.clone(),
                key.into(),
                "owner".into(),
                Authority::Standalone,
                1,
            )
        };
        db.register_native_base(&ctx("base"), &commit, leases.last().unwrap())
            .await
            .unwrap();
        for lease in &leases {
            db.release_object_hold(lease).await.unwrap();
        }
        db.put_source_profile(&ctx("profile"), &SourceProfile::full())
            .await
            .unwrap();
        db.provision_principal(scope.tenant(), "node-1", PrincipalKind::Node, "operator")
            .await
            .unwrap();
        db.set_repository_access(&scope, "node-1", &[Permission::RuntimeControl], "operator")
            .await
            .unwrap();
        let token = db
            .issue_repository_token(
                &scope,
                "node-1",
                &[Permission::RuntimeControl],
                3600,
                "operator",
            )
            .await
            .unwrap();
        let auth = db
            .authorize(&token.secret, &scope, Permission::RuntimeControl)
            .await
            .unwrap();
        let directory = temporary();
        let node =
            Node::create(&directory.path().join("node"), scope.tenant(), "node-1", 4).unwrap();
        let storage = node.identity().storage();
        db.register_node_storage(scope.tenant(), "node-1", storage, "operator")
            .await
            .unwrap();
        let workspace = Uuid::new_v4();
        db.create_workspace(
            &ctx("workspace"),
            &WorkspaceSpec {
                id: workspace,
                base,
                profile: *SourceProfile::full().id().as_bytes(),
                format: WorkspaceFormat::Native,
            },
        )
        .await
        .unwrap();
        Self {
            schema,
            scope,
            db,
            token,
            auth,
            objects,
            store,
            directory,
            node: Some(node),
            storage,
            workspace,
            base,
        }
    }
    pub fn context(&self, key: &str) -> CommandContext {
        CommandContext::trusted(
            self.scope.clone(),
            key.into(),
            "owner".into(),
            Authority::Standalone,
            1,
        )
    }
    pub async fn metadata(&self) -> Metadata {
        Metadata::new(database::connection(&self.schema).await)
            .await
            .unwrap()
    }
    pub fn reopen(&self) -> Node {
        Node::open(
            &self.directory.path().join("node"),
            self.scope.tenant(),
            "node-1",
            self.storage,
            4,
        )
        .unwrap()
    }
    pub async fn expire(&self) {
        database::connection(&self.schema).await.execute("UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE kind='workspace.prepare' AND state='running'", &[]).await.unwrap();
    }
    pub async fn cleanup(self) {
        database::cleanup(&self.schema).await;
    }
}
