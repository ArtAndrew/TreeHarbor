#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::PathBuf,
    process::{Child, Command, Output},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use treeharbor_core::{Changes, Commit, Directory, Object, RevisionRef};
use treeharbor_metadata::{Authority, Scope, intake::CommandContext};
use treeharbor_service::{
    client::{self, Credential},
    config::Config,
    database::Database,
    files,
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
struct Fixture {
    dir: tempfile::TempDir,
    config_path: PathBuf,
    credential_path: PathBuf,
    config: Config,
    scope: Scope,
    sql: tokio_postgres::Client,
    driver: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.driver.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let database_url = std::env::var("TH_SERVICE_TEST_DATABASE_URL").expect(
            "set TH_SERVICE_TEST_DATABASE_URL to disposable PostgreSQL over a trusted Unix socket",
        );
        // Short pathname for macOS sockaddr_un; unique, owner-private directory.
        let dir = tempfile::Builder::new()
            .prefix("ths-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let schema = format!("th_test_{}", Uuid::new_v4().simple());
        let config = Config {
            database_url,
            schema,
            socket: dir.path().join("service.sock"),
            pool_size: 1,
            max_connections: 8,
            request_timeout_ms: 2000,
        };
        let (sql, connection) = config
            .database_config()
            .unwrap()
            .connect(tokio_postgres::NoTls)
            .await
            .unwrap();
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        sql.batch_execute(&format!("CREATE SCHEMA {}", config.schema))
            .await
            .unwrap();
        sql.query_one(
            "SELECT set_config('search_path',$1,false)",
            &[&config.schema],
        )
        .await
        .unwrap();
        let scope = Scope::new("transport-test", Uuid::new_v4()).unwrap();
        let f = Self {
            config_path: dir.path().join("config.json"),
            credential_path: dir.path().join("agent.json"),
            dir,
            config,
            scope,
            sql,
            driver,
        };
        f.save_config();
        f.admin_ok(&["migrate"]);
        f.admin_ok(&[
            "repository-create",
            "--tenant",
            f.scope.tenant(),
            "--repository",
            &f.scope.repository().to_string(),
            "--authority",
            "standalone",
        ]);
        f.admin_ok(&[
            "principal-create",
            "--tenant",
            f.scope.tenant(),
            "--principal",
            "agent-a",
            "--kind",
            "agent",
        ]);
        f.admin_ok(&[
            "access-set",
            "--tenant",
            f.scope.tenant(),
            "--repository",
            &f.scope.repository().to_string(),
            "--principal",
            "agent-a",
            "--permission",
            "read",
            "--permission",
            "workspace.write",
        ]);
        f.admin_ok(&[
            "token-issue",
            "--tenant",
            f.scope.tenant(),
            "--repository",
            &f.scope.repository().to_string(),
            "--principal",
            "agent-a",
            "--permission",
            "read",
            "--permission",
            "workspace.write",
            "--output",
            f.credential_path.to_str().unwrap(),
        ]);
        f
    }
    fn save_config(&self) {
        if self.config_path.exists() {
            fs::remove_file(&self.config_path).unwrap();
        }
        let c = &self.config;
        files::write_private(&self.config_path,json!({"database_url":c.database_url,"schema":c.schema,"socket":c.socket,"pool_size":c.pool_size,"max_connections":c.max_connections,"request_timeout_ms":c.request_timeout_ms}).to_string().as_bytes()).unwrap();
    }
    fn admin(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .args(["admin", "--config"])
            .arg(&self.config_path)
            .args(args)
            .output()
            .unwrap()
    }
    fn admin_ok(&self, args: &[&str]) -> Value {
        let output = self.admin(args);
        assert!(
            output.status.success(),
            "admin failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("thv1_"));
        assert!(!String::from_utf8_lossy(&output.stderr).contains("thv1_"));
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn cli(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .args(["client", "--socket"])
            .arg(&self.config.socket)
            .arg("--credential-file")
            .arg(&self.credential_path)
            .args(args)
            .output()
            .unwrap()
    }
    async fn start(&self) -> Process {
        let log = fs::File::create(self.dir.path().join("server.log")).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .arg("serve")
            .arg("--config")
            .arg(&self.config_path)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap();
        let mut process = Process(child);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            assert!(
                process.0.try_wait().unwrap().is_none(),
                "server exited: {}",
                fs::read_to_string(self.dir.path().join("server.log")).unwrap()
            );
            if fs::read_to_string(self.dir.path().join("server.log"))
                .unwrap_or_default()
                .contains("\"status\":\"listening\"")
            {
                break;
            }
            assert!(Instant::now() < deadline, "server failed to listen");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        process
    }
    fn credential(&self) -> Credential {
        Credential::load(&self.credential_path).unwrap()
    }
    async fn send(&self, command: Value) -> client::Reply {
        let credential = self.credential();
        client::send(
            &self.config.socket,
            &credential.token,
            credential.request(command).to_string().into_bytes(),
            Duration::from_secs(10),
        )
        .await
        .unwrap()
    }
    async fn raw(&self, request: &[u8]) -> Vec<u8> {
        let mut stream = UnixStream::connect(&self.config.socket).await.unwrap();
        stream.write_all(request).await.unwrap();
        let mut reply = vec![];
        tokio::time::timeout(
            Duration::from_secs(10),
            stream.take(2 * 1024 * 1024).read_to_end(&mut reply),
        )
        .await
        .unwrap()
        .unwrap();
        reply
    }
    fn request_bytes(&self, body: &[u8], extra: &str) -> Vec<u8> {
        let credential = self.credential();
        let mut bytes=format!("POST /v1/commands HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n{extra}\r\n",credential.token.expose(),body.len()).into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }
    async fn seed(&self) -> RevisionRef {
        let objects = self.dir.path().join("objects");
        fs::create_dir(&objects).unwrap();
        fs::set_permissions(&objects, fs::Permissions::from_mode(0o700)).unwrap();
        let depot = ManagedStore::open(
            &objects,
            TenantId::new(self.scope.tenant()).unwrap(),
            "test-local",
        )
        .unwrap();
        let mut session = Database::new(self.config.clone())
            .unwrap()
            .connect(true)
            .await
            .unwrap();
        let db = &mut session.metadata;
        let root = Object::Directory(Directory::new(vec![]).unwrap());
        let snapshot = Object::Snapshot {
            root: root.id().unwrap(),
        };
        let operations = Object::ChangeOperations(Changes::new(vec![]).unwrap());
        let commit = Commit {
            parents: vec![],
            snapshot: snapshot.id().unwrap(),
            changes: operations.id().unwrap(),
            author_name: "Fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: "fixture base".into(),
        };
        let native = Object::NativeCommit(commit.clone());
        let base = RevisionRef::Native(native.id().unwrap());
        let mut lease = None;
        for object in [root, snapshot, operations, native] {
            let prepared = PreparedObject::new(&object).unwrap();
            let current = db
                .reserve_object(&depot, &prepared, Uuid::new_v4(), 60)
                .await
                .unwrap();
            if current.requires_upload() {
                let receipt = depot.put(current.key().generation(), &prepared).unwrap();
                db.publish_object(&current, &receipt).await.unwrap();
            }
            lease = Some(current);
        }
        db.register_native_base(
            &CommandContext::trusted(
                self.scope.clone(),
                "seed".into(),
                "fixture".into(),
                Authority::Standalone,
                1,
            ),
            &commit,
            &lease.unwrap(),
        )
        .await
        .unwrap();
        base
    }
    async fn cleanup(&self) {
        self.sql
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.config.schema))
            .await
            .unwrap();
    }
}
fn create(base: RevisionRef, id: Uuid, key: &str) -> Value {
    json!({"method":"workspace_create","workspace":id.to_string(),"key":key,"authority":"standalone","epoch":1,"base":base.to_string(),"profile_digest":treeharbor_core::SourceProfile::full().id().to_string()})
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL over a Unix socket; runs in database CI"]
async fn real_cli_bootstrap_workspace_replay_crash_restart_and_revocation() {
    let f = Fixture::new().await;
    let base = f.seed().await;
    let server = f.start().await;
    assert!(
        f.cli(&[
            "profile-put",
            "--key",
            "full-profile",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--full"
        ])
        .status
        .success()
    );
    let info = f.cli(&["repository-info"]);
    assert!(info.status.success());
    let value: Value = serde_json::from_slice(&info.stdout).unwrap();
    assert_eq!(value["result"]["authority"], "standalone");
    // A second process must not replace the live server's socket or lock.
    let duplicate = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["serve", "--config"])
        .arg(&f.config_path)
        .output()
        .unwrap();
    assert!(!duplicate.status.success());
    assert!(f.cli(&["repository-info"]).status.success());
    // Write a complete mutation, deliberately never consume its response.
    let id = Uuid::new_v4();
    let command = create(base, id, "lost-response");
    let body = f.credential().request(command.clone()).to_string();
    let mut lost = UnixStream::connect(&f.config.socket).await.unwrap();
    lost.write_all(&f.request_bytes(body.as_bytes(), ""))
        .await
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let n: i64 = f
            .sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE key='lost-response'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        if n == 1 {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(server);
    drop(lost); // SIGKILL; no graceful socket cleanup.
    assert!(
        fs::symlink_metadata(&f.config.socket)
            .unwrap()
            .file_type()
            .is_socket()
    );
    let server = f.start().await;
    let replay = f.send(command).await;
    assert!(replay.status.is_success());
    let receipt = f
        .send(json!({"method":"operation_status","key":"lost-response"}))
        .await;
    assert_eq!(replay.body["result"], receipt.body["result"]["result"]);
    let n: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='workspace.prepare'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    let status = f.cli(&["workspace-status", "--workspace", &id.to_string()]);
    assert!(status.status.success());
    let deletion = f.cli(&[
        "workspace-delete",
        "--workspace",
        &id.to_string(),
        "--key",
        "drain",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--generation",
        "1",
    ]);
    assert!(deletion.status.success());
    let drained: Value = serde_json::from_slice(
        &f.cli(&["workspace-status", "--workspace", &id.to_string()])
            .stdout,
    )
    .unwrap();
    assert_eq!(drained["result"]["state"], "draining");
    let token: Value = serde_json::from_slice(&fs::read(&f.credential_path).unwrap()).unwrap();
    f.admin_ok(&[
        "token-revoke",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--token-id",
        token["token_id"].as_str().unwrap(),
    ]);
    let revoked = f.cli(&["operation-status", "--key", "lost-response"]);
    assert!(!revoked.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&revoked.stdout).unwrap()["error"]["code"],
        "unauthenticated"
    );
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL over a Unix socket; runs in database CI"]
async fn http_boundary_rejects_ambiguous_headers_bodies_and_privilege_injection() {
    let f = Fixture::new().await;
    let server = f.start().await;
    let body = f
        .credential()
        .request(json!({"method":"repository_info"}))
        .to_string();
    for (request, expected) in [
        (
            b"GET /healthz HTTP/1.1\r\nHost: treeharbor.local\r\n\r\n".to_vec(),
            200,
        ),
        (
            f.request_bytes(body.as_bytes(), "Origin: https://evil.invalid\r\n"),
            400,
        ),
        (
            f.request_bytes(body.as_bytes(), "Host: treeharbor.local\r\n"),
            400,
        ),
        (
            f.request_bytes(body.as_bytes(), "Authorization: Bearer ignored\r\n"),
            401,
        ),
        (
            f.request_bytes(body.as_bytes(), "Content-Type: application/json\r\n"),
            415,
        ),
        (f.request_bytes(&vec![b' '; 65537], ""), 413),
        (
            f.request_bytes(
                body.replace("repository_info", "workspace_ready")
                    .as_bytes(),
                "",
            ),
            400,
        ),
        (
            f.request_bytes(
                body.replace("\"version\":1", "\"version\":1,\"version\":1")
                    .as_bytes(),
                "",
            ),
            400,
        ),
    ] {
        let reply = f.raw(&request).await;
        assert!(
            reply.starts_with(format!("HTTP/1.1 {expected} ").as_bytes()),
            "unexpected status: {}",
            String::from_utf8_lossy(&reply[..reply.len().min(64)])
        );
        assert!(!reply.windows(5).any(|v| v == b"thv1_"));
    }
    let credential = f.credential();
    let wrong = json!({"version":1,"tenant":"other-tenant","repository":f.scope.repository().to_string(),"command":{"method":"repository_info"}});
    let reply = client::send(
        &f.config.socket,
        &credential.token,
        wrong.to_string().into_bytes(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(reply.status.as_u16(), 403);
    assert_eq!(reply.body["error"]["code"], "forbidden");
    let no_overwrite = f.admin(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--permission",
        "read",
        "--output",
        f.credential_path.to_str().unwrap(),
    ]);
    assert!(!no_overwrite.status.success());
    assert!(f.cli(&["repository-info"]).status.success());
    // Enforce bytes actually streamed, not just the declared Content-Length.
    let mut chunked=format!("POST /v1/commands HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nTransfer-Encoding: chunked\r\n\r\n10001\r\n",credential.token.expose()).into_bytes();
    chunked.extend(vec![b' '; 65537]);
    chunked.extend_from_slice(b"\r\n0\r\n\r\n");
    assert!(f.raw(&chunked).await.starts_with(b"HTTP/1.1 413 "));
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL over a Unix socket; runs in database CI"]
async fn startup_refuses_schema_drift_and_pool_recovers_its_terminated_connection() {
    let f = Fixture::new().await;
    f.sql
        .execute(
            "UPDATE th_schema_version SET migration_digest=decode(repeat('00',32),'hex')",
            &[],
        )
        .await
        .unwrap();
    assert!(
        Database::new(f.config.clone())
            .unwrap()
            .connect(true)
            .await
            .is_err()
    );
    let refused = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["serve", "--config"])
        .arg(&f.config_path)
        .output()
        .unwrap();
    assert!(!refused.status.success());
    assert!(!f.config.socket.exists());
    // Restore the fixture's checksum from the immutable migration chain, via a
    // fresh migrate in this disposable schema rather than weakening the check.
    f.sql
        .batch_execute(&format!(
            "DROP SCHEMA {} CASCADE; CREATE SCHEMA {}",
            f.config.schema, f.config.schema
        ))
        .await
        .unwrap();
    f.admin_ok(&["migrate"]);
    let database = Database::new(f.config.clone()).unwrap();
    let pool = database.pool().unwrap();
    let session = pool.get().await.unwrap();
    let pid = session.backend_pid();
    drop(session);
    let stopped: bool = f
        .sql
        .query_one("SELECT pg_terminate_backend($1,2000)", &[&pid])
        .await
        .unwrap()
        .get(0);
    assert!(stopped);
    let session = pool.get().await.unwrap();
    assert_ne!(session.backend_pid(), pid);
    session.metadata.ping().await.unwrap();
    drop(session);
    drop(pool);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL over a Unix socket; runs in database CI"]
async fn request_timeout_rolls_back_before_pool_reuse_and_same_key_can_retry() {
    let mut f = Fixture::new().await;
    let base = f.seed().await;
    f.config.request_timeout_ms = 300;
    f.save_config();
    f.sql.batch_execute("CREATE SEQUENCE reached_effect; CREATE FUNCTION slow_workspace() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM nextval('reached_effect'); PERFORM pg_sleep(1); RETURN NEW; END $$; CREATE TRIGGER slow_workspace BEFORE INSERT ON th_workspaces FOR EACH ROW EXECUTE FUNCTION slow_workspace()").await.unwrap();
    let server = f.start().await;
    assert!(
        f.cli(&[
            "profile-put",
            "--key",
            "full-profile",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--full"
        ])
        .status
        .success()
    );
    let command = create(base, Uuid::new_v4(), "cancelled-request");
    let reply = f.send(command.clone()).await;
    assert_eq!(reply.status.as_u16(), 504);
    assert_eq!(reply.body["error"]["code"], "outcome_unknown");
    // Sequence changes survive rollback and prove cancellation reached the
    // mutation, rather than merely timing out during connection setup.
    let reached: bool = f
        .sql
        .query_one("SELECT is_called FROM reached_effect", &[])
        .await
        .unwrap()
        .get(0);
    assert!(reached);
    // The next pool checkout must execute the queued rollback before returning.
    // Wait for the slow statement to finish instead of relying on a timing guess.
    f.sql
        .batch_execute("DROP TRIGGER slow_workspace ON th_workspaces")
        .await
        .unwrap();
    let info = f.send(json!({"method":"repository_info"})).await;
    assert!(info.status.is_success());
    let n: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_workspaces", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 0);
    let reply = f.send(command).await;
    assert!(reply.status.is_success());
    let n: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_operations WHERE key='cancelled-request'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    let n: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='workspace.prepare'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL over a Unix socket; runs in database CI"]
async fn connection_limit_header_deadline_and_graceful_shutdown_release_socket() {
    let mut f = Fixture::new().await;
    f.config.max_connections = 1;
    f.save_config();
    let mut server = f.start().await;
    let health = b"GET /healthz HTTP/1.1\r\nHost: treeharbor.local\r\n\r\n";
    assert!(f.raw(health).await.starts_with(b"HTTP/1.1 200 "));
    let mut held = UnixStream::connect(&f.config.socket).await.unwrap();
    held.write_all(b"POST /v1/commands HTTP/1.1\r\nHost: treeharbor.local\r\n")
        .await
        .unwrap();
    let mut excess = UnixStream::connect(&f.config.socket).await.unwrap();
    let _ = excess.write_all(health).await;
    let mut byte = [0u8; 1];
    match tokio::time::timeout(Duration::from_secs(2), excess.read(&mut byte))
        .await
        .unwrap()
    {
        Ok(0) => {}
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
        other => panic!("excess connection not rejected: {other:?}"),
    }
    let mut timed_out = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(7), held.read_to_end(&mut timed_out))
        .await
        .unwrap();
    assert!(
        result.is_ok() || result.is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionReset)
    );
    assert!(f.raw(health).await.starts_with(b"HTTP/1.1 200 "));
    rustix::process::kill_process(
        rustix::process::Pid::from_raw(server.0.id() as i32).unwrap(),
        rustix::process::Signal::TERM,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "shutdown failed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!f.config.socket.exists());
    // The persistent lock file is reusable, not removed while other owners may hold it.
    drop(server);
    let server = f.start().await;
    assert!(f.cli(&["repository-info"]).status.success());
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL over a Unix socket; runs in database CI"]
async fn cli_profile_roundtrip_survives_restart_and_workspace_rejects_unknown_definition() {
    let f = Fixture::new().await;
    let base = f.seed().await;
    let server = f.start().await;
    let args = [
        "profile-put",
        "--key",
        "sparse-selection",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--path",
        "src/lib.rs",
        "--path",
        "src",
    ];
    let output = f.cli(&args);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    let profile = treeharbor_core::SourceProfile::sparse(vec![
        treeharbor_core::RepoPath::new(b"src".to_vec()).unwrap(),
    ])
    .unwrap();
    assert_eq!(
        receipt["result"]["profile_digest"],
        profile.id().to_string()
    );
    assert_eq!(receipt["result"]["profile"]["paths_hex"], json!(["737263"]));
    assert_eq!(
        serde_json::from_slice::<Value>(&f.cli(&args).stdout).unwrap(),
        receipt
    );
    let get = f.cli(&["profile-get", "--profile-digest", &profile.id().to_string()]);
    assert!(get.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&get.stdout).unwrap(),
        receipt
    );
    let workspace = Uuid::new_v4();
    let mut command = create(base, workspace, "profile-workspace");
    // Full profile is not registered here. An arbitrary digest is not enough.
    let rejected = f.send(command.clone()).await;
    assert_eq!(rejected.status.as_u16(), 404);
    let count: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_workspaces", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    command["profile_digest"] = json!(profile.id().to_string());
    assert!(f.send(command).await.status.is_success());
    let status = f
        .send(json!({"method":"workspace_status","workspace":workspace.to_string()}))
        .await;
    assert_eq!(status.body["result"]["profile_resolved"], true);
    assert!(
        f.sql
            .execute(
                "DELETE FROM th_source_profiles WHERE digest=$1",
                &[&profile.id().as_bytes().as_slice()]
            )
            .await
            .is_err()
    );
    assert_eq!(
        status.body["result"]["profile_digest"],
        profile.id().to_string()
    );
    for invalid in [
        json!({"kind":"full","paths_hex":[]}),
        json!({"kind":"sparse","paths_hex":[]}),
        json!({"kind":"sparse","paths_hex":["2e2e"]}),
        json!({"kind":"sparse","paths_hex":["FF"]}),
    ] {
        let reply=f.send(json!({"method":"source_profile_put","key":"invalid","authority":"standalone","epoch":1,"profile":invalid})).await;
        assert_eq!(reply.status.as_u16(), 400);
    }
    drop(server);
    let server = f.start().await;
    let get = f.cli(&["profile-get", "--profile-digest", &profile.id().to_string()]);
    assert!(get.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&get.stdout).unwrap(),
        receipt
    );
    drop(server);
    f.cleanup().await;
}
