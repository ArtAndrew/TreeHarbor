#![cfg(unix)]
#[path = "transport/native.rs"]
mod native;
#[path = "transport/proposals.rs"]
mod proposals;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; exercises actual node CLI"]
async fn native_import_to_node_files_and_reopen_work_with_git_absent() {
    let mut f = Fixture::new().await;
    f.importer();
    let source = f.dir.path().join("node-source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("main.rs"), b"fn main() {}\n").unwrap();
    let pack = f.dir.path().join("node-pack");
    let packed = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["source-pack", "--directory"])
        .arg(&source)
        .arg("--output")
        .arg(&pack)
        .args([
            "--path",
            "main.rs",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(packed.status.success());
    let server = f.start().await;
    let imported = f.cli(&[
        "source-import",
        "--pack",
        pack.to_str().unwrap(),
        "--state",
        f.dir.path().join("node-import.json").to_str().unwrap(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let imported: Value = serde_json::from_slice(&imported.stdout).unwrap();
    let profile = f.cli(&[
        "profile-put",
        "--key",
        "node-full",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--full",
    ]);
    assert!(profile.status.success());
    let profile: Value = serde_json::from_slice(&profile.stdout).unwrap();
    let workspace = Uuid::new_v4().to_string();
    let created = f.cli(&[
        "workspace-create",
        "--key",
        "node-workspace",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &workspace,
        "--base",
        imported["result"]["revision"].as_str().unwrap(),
        "--profile-digest",
        profile["result"]["profile_digest"].as_str().unwrap(),
    ]);
    assert!(created.status.success());
    let root = f.dir.path().join("node");
    let initialized = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["node", "initialize", "--root"])
        .arg(&root)
        .args(["--tenant", f.scope.tenant(), "--principal", "node-local"])
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let identity: Value = serde_json::from_slice(&initialized.stdout).unwrap();
    let storage = identity["storage"].as_str().unwrap();
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        "node-local",
        "--kind",
        "node",
    ]);
    f.admin_ok(&[
        "node-register",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        "node-local",
        "--storage",
        storage,
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "node-local",
        "--permission",
        "runtime.control",
    ]);
    let node_credential = f.dir.path().join("node-credential.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "node-local",
        "--permission",
        "runtime.control",
        "--output",
        node_credential.to_str().unwrap(),
    ]);
    let node_cli = |action: &str, extra: &[&str], credential: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["node", action, "--config"])
            .arg(&f.config_path)
            .arg("--credential-file")
            .arg(credential)
            .arg("--root")
            .arg(&root)
            .args(["--storage", storage])
            .args(extra)
            .output()
            .unwrap()
    };
    let refused = node_cli("prepare-once", &[], &f.credential_path);
    assert!(!refused.status.success());
    let prepared = node_cli("prepare-once", &[], &node_credential);
    assert!(
        prepared.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared.stderr)
    );
    assert!(!String::from_utf8_lossy(&prepared.stdout).contains("thv1_"));
    let prepared: Value = serde_json::from_slice(&prepared.stdout).unwrap();
    assert_eq!(prepared["state"], "files_ready");
    assert_eq!(prepared["workspace"], workspace);
    assert!(prepared["runtime"].is_null());
    let files = PathBuf::from(prepared["path"].as_str().unwrap());
    assert_eq!(fs::read(files.join("main.rs")).unwrap(), b"fn main() {}\n");
    fs::write(files.join("main.rs"), b"saved edit").unwrap();
    let reopened = node_cli("prepared", &["--workspace", &workspace], &node_credential);
    assert!(reopened.status.success());
    let reopened: Value = serde_json::from_slice(&reopened.stdout).unwrap();
    assert_eq!(reopened["path"], prepared["path"]);
    assert_eq!(reopened["recovered_baseline"], true);
    assert_eq!(fs::read(files.join("main.rs")).unwrap(), b"saved edit");
    let idle = node_cli("prepare-once", &[], &node_credential);
    assert!(idle.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&idle.stdout).unwrap()["state"],
        "idle"
    );
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn native_source_cli_lists_pages_and_reads_verified_files_without_following_links() {
    use std::os::unix::ffi::OsStrExt;
    use treeharbor_metadata::workspace_reads::SourceFence;
    let mut f = Fixture::new().await;
    let outsider = f.importer();
    let source = f.dir.path().join("input");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::write(source.join("src/main.rs"), b"fn main() {}\n").unwrap();
    fs::write(source.join("src/run.sh"), b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(source.join("src/run.sh"), fs::Permissions::from_mode(0o755)).unwrap();
    std::os::unix::fs::symlink("main.rs", source.join("src/link")).unwrap();
    fs::create_dir(source.join("raw")).unwrap();
    let raw_name: &[u8] = if cfg!(target_os = "macos") {
        b"raw/name"
    } else {
        b"raw/name-\xff"
    };
    fs::write(
        source.join(std::ffi::OsStr::from_bytes(raw_name)),
        b"binary path contents\0\xff",
    )
    .unwrap();
    let pack = f.dir.path().join("pack");
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .arg("source-pack")
        .arg("--directory")
        .arg(&source)
        .arg("--output")
        .arg(&pack)
        .args([
            "--path",
            "src",
            "--path",
            "raw",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packed: Value = serde_json::from_slice(&output.stdout).unwrap();
    let server = f.start().await;
    let journal = f.dir.path().join("import.json");
    let imported = f.cli(&[
        "source-import",
        "--pack",
        pack.to_str().unwrap(),
        "--state",
        journal.to_str().unwrap(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ]);
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stdout)
    );
    let accepted: Value = serde_json::from_slice(&imported.stdout).unwrap();
    // A delivery selection is not an ACL. An explicit baseline read outside this
    // profile is allowed, but must not mutate/expand the physical workspace.
    let profile = f.cli(&[
        "profile-put",
        "--key",
        "main-only",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--path",
        "src/main.rs",
    ]);
    assert!(profile.status.success());
    let profile: Value = serde_json::from_slice(&profile.stdout).unwrap();
    let workspace = Uuid::new_v4();
    let id = workspace.to_string();
    let common = [
        "--workspace",
        &id,
        "--generation",
        "1",
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ];
    let created = f.cli(&[
        "workspace-create",
        "--key",
        "source-workspace",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &id,
        "--base",
        accepted["result"]["revision"].as_str().unwrap(),
        "--profile-digest",
        profile["result"]["profile_digest"].as_str().unwrap(),
    ]);
    assert!(created.status.success());
    let first = f.cli(
        &[
            &["source-list"][..],
            &common,
            &["--path", "src", "--limit", "2"],
        ]
        .concat(),
    );
    assert!(
        first.status.success(),
        "{} {}",
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    let page: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(page["result"]["snapshot"], packed["snapshot"]);
    assert_eq!(page["result"]["entries"].as_array().unwrap().len(), 2);
    assert_eq!(page["result"]["entries"][0]["name_hex"], "6c696e6b");
    assert_eq!(page["result"]["entries"][0]["mode"], "120000");
    assert_eq!(page["result"]["next_after_hex"], "6d61696e2e7273");
    let second = f.cli(
        &[
            &["source-list"][..],
            &common,
            &[
                "--path",
                "src",
                "--limit",
                "2",
                "--after-hex",
                "6d61696e2e7273",
            ],
        ]
        .concat(),
    );
    assert!(second.status.success());
    let page: Value = serde_json::from_slice(&second.stdout).unwrap();
    assert_eq!(page["result"]["entries"].as_array().unwrap().len(), 1);
    assert_eq!(page["result"]["entries"][0]["name_hex"], "72756e2e7368");
    assert_eq!(page["result"]["entries"][0]["mode"], "100755");
    assert!(page["result"]["next_after_hex"].is_null());
    for (name, bytes, mode) in [
        ("main.rs", b"fn main() {}\n".as_slice(), "100644"),
        ("run.sh", b"#!/bin/sh\nexit 0\n".as_slice(), "100755"),
        ("link", b"main.rs".as_slice(), "120000"),
    ] {
        let out = f.dir.path().join(format!("download-{name}"));
        let path = format!("src/{name}");
        let args = [
            &["source-file"][..],
            &common,
            &["--path", &path, "--output", out.to_str().unwrap()],
        ]
        .concat();
        let result = f.cli(&args);
        assert!(
            result.status.success(),
            "{} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(fs::read(&out).unwrap(), bytes);
        let meta = fs::symlink_metadata(&out).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let receipt: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(receipt["result"]["mode"], mode);
        assert_eq!(receipt["result"]["base"], accepted["result"]["revision"]);
        assert!(!f.cli(&args).status.success()); // no clobber, even identical bytes
        assert_eq!(fs::read(out).unwrap(), bytes);
    }
    let fence = SourceFence {
        workspace,
        generation: 1,
        authority: Authority::Standalone,
        epoch: 1,
    };
    let operation = json!({"kind":"file","path_hex":"7372632f6d61696e2e7273"});
    let raw_operation = json!({"kind":"file","path_hex":raw_name.iter().map(|b|format!("{b:02x}")).collect::<String>()});
    let raw = treeharbor_service::download::read(
        &f.config.socket,
        &f.credential(),
        fence,
        raw_operation,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(raw.status.is_success());
    assert_eq!(raw.content.unwrap(), b"binary path contents\0\xff");
    let denied = treeharbor_service::download::read(
        &f.config.socket,
        &outsider,
        fence,
        operation.clone(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(denied.status.as_u16(), 404);
    assert!(denied.content.is_none());
    let stale = treeharbor_service::download::read(
        &f.config.socket,
        &f.credential(),
        SourceFence {
            generation: 2,
            ..fence
        },
        operation.clone(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(stale.status.as_u16(), 409);
    for (path, code) in [("src/absent", 404), ("src", 409), ("src/link/child", 404)] {
        let out = f.dir.path().join("must-not-exist");
        let result = f.cli(
            &[
                &["source-file"][..],
                &common,
                &["--path", path, "--output", out.to_str().unwrap()],
            ]
            .concat(),
        );
        assert!(!result.status.success());
        assert!(!out.exists());
        let response: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(
            response["error"]["code"],
            if code == 404 {
                "source_path_not_found"
            } else {
                "source_path_type"
            }
        );
    }
    let status = f.cli(&["workspace-status", "--workspace", &id]);
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["result"]["state"], "requested");
    assert_eq!(status["result"]["generation"], 1);
    assert_eq!(
        status["result"]["profile_digest"],
        profile["result"]["profile_digest"]
    );
    let deleted = f.cli(&[
        "workspace-delete",
        "--key",
        "drain-source",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &id,
        "--generation",
        "1",
    ]);
    assert!(deleted.status.success());
    let drained = treeharbor_service::download::read(
        &f.config.socket,
        &f.credential(),
        fence,
        operation,
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(drained.status.as_u16(), 409);
    assert!(drained.content.is_none());
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn directory_import_resumes_partial_graph_and_completed_receipt_without_source_or_git() {
    let mut f = Fixture::new().await;
    f.importer();
    let source = f.dir.path().join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::write(source.join("src/main.rs"), b"fn main() {}\n").unwrap();
    fs::write(source.join("not-selected"), b"private").unwrap();
    let pack_path = f.dir.path().join("pack");
    let state = f.dir.path().join("import.json");
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .arg("source-pack")
        .arg("--directory")
        .arg(&source)
        .arg("--output")
        .arg(&pack_path)
        .args([
            "--path",
            "src",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
            "--message",
            "capture",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let packed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        packed["commit"],
        "sha256:edd78b7b781955d45661c7d81c46ac4939ec0cb6fa9a80dc1fd28b0e53b5bf3f"
    );
    let pack = treeharbor_service::pack::Pack::open(&pack_path).unwrap();
    let args = [
        "source-import",
        "--pack",
        pack_path.to_str().unwrap(),
        "--state",
        state.to_str().unwrap(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ];
    // No server exists yet. The identity must be durable before first network IO.
    assert!(!f.cli(&args).status.success());
    let original = files::read_private(&state, 4096).unwrap();
    let journal: Value = serde_json::from_slice(&original).unwrap();
    assert_eq!(
        journal["pack_fingerprint"],
        "sha256:cb4a7d0f8f09dab7215cf4c3624c59cda21a61308b103fa275f73e9f0008f1d2"
    );
    let upload: Uuid = journal["upload"].as_str().unwrap().parse().unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(pack_path.join("manifest.json")).unwrap()).unwrap();
    let mut server = f.start().await;
    let begin = f.send(json!({"method":"source_begin","key":format!("import-{upload}-begin"),"authority":"standalone","epoch":1,"upload":upload.to_string(),"ttl_seconds":journal["ttl_seconds"],"max_objects":journal["max_objects"],"max_bytes":journal["max_bytes"]})).await;
    assert!(begin.status.is_success(), "{}", begin.body);
    // Stop after the first actual binary PUT, then remove the original source.
    let blob = manifest["objects"][0].as_str().unwrap().parse().unwrap();
    let reply = client::send_object(
        &f.config.socket,
        &f.credential(),
        upload,
        blob,
        pack.frame(blob).unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert!(reply.status.is_success());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    fs::remove_dir_all(&source).unwrap();
    server = f.start().await;
    // Altering the invocation must not overwrite its identity or send requests.
    let mismatch = f.cli(&[args.as_slice(), &["--ttl-seconds", "60"]].concat());
    assert!(!mismatch.status.success());
    assert_eq!(fs::read(&state).unwrap(), original);
    let count: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_source_upload_items", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    let result = f.cli(&args);
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let accepted: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        accepted["result"]["revision"],
        RevisionRef::Native(pack.commit()).to_string()
    );
    assert_eq!(accepted["result"]["state"], "completed");
    assert_eq!(fs::read(&state).unwrap(), original);
    // Lost final reply: exact replay survives process loss and session expiry.
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    f.sql.execute("UPDATE th_source_uploads SET expires_at=clock_timestamp()-interval '1 second' WHERE upload=$1", &[&upload]).await.unwrap();
    server = f.start().await;
    assert_eq!(f.cli(&args).stdout, result.stdout);
    let (sessions, items, bases, pins): (i64, i64, i64, i64) = {
        let row = f.sql.query_one("SELECT (SELECT count(*) FROM th_source_uploads), (SELECT count(*) FROM th_source_upload_items), (SELECT count(*) FROM th_bases), (SELECT count(*) FROM th_object_pins)", &[]).await.unwrap();
        (row.get(0), row.get(1), row.get(2), row.get(3))
    };
    assert_eq!(
        (sessions, items, bases, pins),
        (1, pack.object_count() as i64, 1, 1)
    );
    assert!(
        f.cli(&[
            "profile-put",
            "--key",
            "import-full",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--full"
        ])
        .status
        .success()
    );
    let response = f.cli(&[
        "workspace-create",
        "--key",
        "from-directory",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &Uuid::new_v4().to_string(),
        "--base",
        &RevisionRef::Native(pack.commit()).to_string(),
        "--profile-digest",
        &treeharbor_core::SourceProfile::full().id().to_string(),
    ]);
    assert!(
        response.status.success(),
        "{}",
        String::from_utf8_lossy(&response.stdout)
    );
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; runs in database CI"]
async fn binary_source_upload_survives_process_loss_and_creates_a_real_workspace_base() {
    let mut f = Fixture::new().await;
    let agent = f.importer();
    let upload = Uuid::new_v4().to_string();
    let mut server = f.start().await;
    let begin = [
        "source-begin",
        "--key",
        "source-one",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--upload",
        &upload,
    ];
    assert!(f.cli(&begin).status.success());
    let blob = Object::Blob(b"source bytes\0\xff".to_vec());
    let dir = Object::Directory(
        Directory::new(vec![treeharbor_core::Entry {
            name: treeharbor_core::Name::new(b"main.rs".to_vec()).unwrap(),
            value: treeharbor_core::EntryRef {
                mode: treeharbor_core::EntryMode::File,
                target: blob.id().unwrap(),
            },
        }])
        .unwrap(),
    );
    let snapshot = Object::Snapshot {
        root: dir.id().unwrap(),
    };
    let changes = Object::ChangeOperations(Changes::new(vec![]).unwrap());
    let commit = Object::NativeCommit(Commit {
        parents: vec![],
        snapshot: snapshot.id().unwrap(),
        changes: changes.id().unwrap(),
        author_name: "Importer".into(),
        author_email: "import@example.invalid".into(),
        timestamp_seconds: 0,
        timezone_minutes: 0,
        message: "native import".into(),
    });
    let credential = f.credential();
    let id: Uuid = upload.parse().unwrap();
    // Known global digests and missing child objects do not confer membership.
    let reply = client::send_object(
        &f.config.socket,
        &credential,
        id,
        dir.id().unwrap(),
        dir.encode().unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(reply.status.as_u16(), 409);
    assert_eq!(reply.body["error"]["code"], "upload_membership");
    let reply = client::send_object(
        &f.config.socket,
        &agent,
        id,
        blob.id().unwrap(),
        blob.encode().unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(reply.status.as_u16(), 403);
    let reply = client::send_object(
        &f.config.socket,
        &credential,
        id,
        dir.id().unwrap(),
        blob.encode().unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(reply.status.as_u16(), 400);
    let objects = [blob, dir, snapshot, changes, commit];
    for (index, object) in objects.iter().enumerate() {
        let path = f.dir.path().join(format!("object-{index}"));
        files::write_private(&path, &object.encode().unwrap()).unwrap();
        let args = [
            "source-put",
            "--upload",
            &upload,
            "--object",
            &object.id().unwrap().to_string(),
            "--file",
            path.to_str().unwrap(),
        ];
        let output = f.cli(&args);
        assert!(
            output.status.success(),
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if index == 0 {
            server.0.kill().unwrap();
            server.0.wait().unwrap();
            drop(server);
            server = f.start().await;
            assert!(f.cli(&begin).status.success());
            assert!(f.cli(&args).status.success());
        }
    }
    let root = objects.last().unwrap().id().unwrap();
    let finish = [
        "source-finish",
        "--key",
        "finish-source-one",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--upload",
        &upload,
        "--commit",
        &root.to_string(),
    ];
    let result = f.cli(&finish);
    assert!(result.status.success());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    server = f.start().await;
    assert_eq!(f.cli(&finish).stdout, result.stdout);
    let status = f.cli(&["source-status", "--upload", &upload]);
    assert!(status.status.success());
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(status["result"]["reserved_objects"], 5);
    assert_eq!(status["result"]["state"], "completed");
    assert!(
        f.cli(&[
            "profile-put",
            "--key",
            "all",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--full"
        ])
        .status
        .success()
    );
    let workspace = Uuid::new_v4();
    let response = f.cli(&[
        "workspace-create",
        "--key",
        "from-source",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &workspace.to_string(),
        "--base",
        &RevisionRef::Native(root).to_string(),
        "--profile-digest",
        &treeharbor_core::SourceProfile::full().id().to_string(),
    ]);
    assert!(
        response.status.success(),
        "{}",
        String::from_utf8_lossy(&response.stdout)
    );
    let protected: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_object_pins", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(protected, 2);
    drop(server);
    f.cleanup().await;
}

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
            storage: None,
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
    fn importer(&mut self) -> Credential {
        let storage = self.dir.path().join("source-store");
        fs::create_dir(&storage).unwrap();
        fs::set_permissions(&storage, fs::Permissions::from_mode(0o700)).unwrap();
        self.config.storage = Some(treeharbor_service::config::StorageConfig {
            root: storage,
            backend: "source-1".into(),
        });
        self.save_config();
        self.admin_ok(&[
            "principal-create",
            "--tenant",
            self.scope.tenant(),
            "--principal",
            "importer",
            "--kind",
            "human",
        ]);
        self.admin_ok(&[
            "access-set",
            "--tenant",
            self.scope.tenant(),
            "--repository",
            &self.scope.repository().to_string(),
            "--principal",
            "importer",
            "--permission",
            "repository.write",
            "--permission",
            "workspace.write",
            "--permission",
            "read",
        ]);
        let agent = self.credential();
        self.credential_path = self.dir.path().join("importer.json");
        self.admin_ok(&[
            "token-issue",
            "--tenant",
            self.scope.tenant(),
            "--repository",
            &self.scope.repository().to_string(),
            "--principal",
            "importer",
            "--permission",
            "repository.write",
            "--permission",
            "workspace.write",
            "--permission",
            "read",
            "--output",
            self.credential_path.to_str().unwrap(),
        ]);
        agent
    }
    fn save_config(&self) {
        if self.config_path.exists() {
            fs::remove_file(&self.config_path).unwrap();
        }
        let c = &self.config;
        files::write_private(&self.config_path,json!({"database_url":c.database_url,"schema":c.schema,"socket":c.socket,"pool_size":c.pool_size,"max_connections":c.max_connections,"request_timeout_ms":c.request_timeout_ms,"storage":c.storage.as_ref().map(|s|json!({"root":s.root,"backend":s.backend}))}).to_string().as_bytes()).unwrap();
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
            .env("PATH", "/treeharbor-test-no-executables")
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
            .env("PATH", "/treeharbor-test-no-executables")
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
    // This must be the first connection. EOF from a preceding health request
    // does not prove its server task has already dropped the sole permit: that
    // race can reject `held`, then admit `excess`. start() only observes a log.
    let held_at = Instant::now();
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
    assert!(held_at.elapsed() >= Duration::from_secs(4));
    // After EOF, observe capacity becoming available instead of assuming that
    // the server has already completed the task which owned this connection.
    let available_by = Instant::now() + Duration::from_secs(3);
    loop {
        let probe = async {
            let mut stream = UnixStream::connect(&f.config.socket).await?;
            stream.write_all(health).await?;
            let mut response = Vec::new();
            stream.take(1024).read_to_end(&mut response).await?;
            Ok::<_, std::io::Error>(response)
        };
        match tokio::time::timeout(Duration::from_secs(2), probe)
            .await
            .unwrap()
        {
            Ok(response) if response.starts_with(b"HTTP/1.1 200 ") => break,
            Ok(response) if response.is_empty() => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe
                ) => {}
            other => panic!("unexpected health response after release: {other:?}"),
        }
        assert!(
            Instant::now() < available_by,
            "connection permit not released"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
