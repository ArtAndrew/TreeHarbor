use super::*;
use std::path::Path;
#[path = "github/admission_policy.rs"]
mod admission_policy;
#[path = "github/archive_activation.rs"]
mod archive_activation;
#[path = "github/archive_observation.rs"]
mod archive_observation;
#[path = "github/archive_payload.rs"]
mod archive_payload;
#[path = "github/archive_stage.rs"]
mod archive_stage;
#[path = "github/events.rs"]
mod events;
#[cfg(target_os = "linux")]
#[path = "github/git_workspace_runtime.rs"]
mod git_workspace_runtime;
#[path = "github/projections.rs"]
mod projections;
#[path = "github/recovery.rs"]
mod recovery;
#[path = "github/restore_acquire.rs"]
mod restore_acquire;
#[path = "github/restore_check.rs"]
mod restore_check;
#[path = "github/restore_delivery.rs"]
mod restore_delivery;
#[path = "github/restore_inventory.rs"]
mod restore_inventory;
#[path = "github/restore_pull.rs"]
mod restore_pull;
#[path = "github/restore_queue.rs"]
mod restore_queue;
#[path = "github/restore_reconnect.rs"]
mod restore_reconnect;
#[path = "github/restore_source.rs"]
mod restore_source;

fn provision_bridge(f: &Fixture) -> PathBuf {
    provision_read_bridge(f, "bridge")
}

fn provision_read_bridge(f: &Fixture, principal: &str) -> PathBuf {
    let bridge = f.dir.path().join(format!("{principal}-import.json"));
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        principal,
        "--kind",
        "bridge",
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        principal,
        "--permission",
        "host.read",
    ]);
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        principal,
        "--permission",
        "host.read",
        "--output",
        bridge.to_str().unwrap(),
    ]);
    bridge
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectedPreparationBenchmark {
    bundle: PathBuf,
    git_revision: String,
    arguments: Vec<String>,
    #[serde(default)]
    reuse_git_pack: bool,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual Git history, bridge HTTP, restart and export (no live App)"]
async fn github_source_http_preserves_exact_git_history_after_partial_upload_and_server_death() {
    let mut f = match std::env::var_os("TH_TEST_GIT_WORKSPACE_BINARY") {
        Some(path) => Fixture::new_with_binary(Authority::Connected, PathBuf::from(path)).await,
        None => Fixture::new_with_authority(Authority::Connected).await,
    };
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let pack_path = f.dir.path().join("original-pack");
    let benchmark = std::env::var_os("TH_TEST_CONNECTED_PREPARATION").map(|path| {
        serde_json::from_slice::<ConnectedPreparationBenchmark>(&fs::read(path).unwrap()).unwrap()
    });
    let capture_seconds = if let Some(benchmark) = &benchmark {
        use treeharbor_git_bridge::{Limits, WorkspacePackScope};
        use treeharbor_service::pack::{GitCapture, capture_git, capture_git_with_workspace_pack};
        let RevisionRef::Git(commit) = benchmark.git_revision.parse().unwrap() else {
            panic!("expected an exact Git revision");
        };
        let capture = GitCapture {
            git: Path::new("/usr/bin/git"),
            bundle: &benchmark.bundle,
            commit,
            output: &pack_path,
            limits: Limits::default(),
        };
        let start = std::time::Instant::now();
        if benchmark.reuse_git_pack {
            capture_git_with_workspace_pack(
                capture,
                &WorkspacePackScope {
                    tenant: f.scope.tenant().into(),
                    repository: f.scope.repository(),
                    epoch: 1,
                },
            )
            .unwrap();
            assert!(pack_path.join("workspace-git-v1/objects.pack").is_file());
        } else {
            capture_git(capture).unwrap();
        }
        Some(start.elapsed().as_secs_f64())
    } else {
        git_history_pack(&f, &pack_path);
        None
    };
    let pack = treeharbor_service::pack::Pack::open(&pack_path).unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(pack_path.join("manifest.json")).unwrap()).unwrap();
    let revision = manifest["source_revision"].as_str().unwrap().to_owned();
    let frames: Vec<_> = manifest["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| {
            let id = id.as_str().unwrap().parse().unwrap();
            (id, pack.frame(id).unwrap())
        })
        .collect();
    let root = pack.commit();
    let server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &bind));
    let id = Uuid::new_v4();
    let requested = json!({"action":"import_request","key":"request","epoch":1,"import":id.to_string(),"binding":bind["binding"],"specification":{"git_revision":revision,"max_objects":pack.object_count(),"max_bytes":pack.encoded_bytes(),"ttl_seconds":3600}});
    assert_eq!(result(request(&f, &requested))["state"], "pending");
    let owner_credential = f.credential();
    f.credential_path = bridge;
    assert!(!request(&f, &requested).status.success());
    let begin = json!({"action":"import_begin","key":"begin","epoch":1,"import":id.to_string(),"native_commit":root.to_string()});
    result(request(&f, &begin));
    let credential = f.credential();
    let (object, frame) = &frames[0];
    assert_eq!(
        client::send_object(
            &f.config.socket,
            &credential,
            id,
            *object,
            frame.clone(),
            Duration::from_secs(5)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        403
    );
    assert_eq!(
        client::send_github_source_object(
            &f.config.socket,
            &owner_credential,
            id,
            *object,
            frame.clone(),
            Duration::from_secs(5)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        403
    );
    for (object, frame) in frames.iter().take(2) {
        assert!(
            client::send_github_source_object(
                &f.config.socket,
                &credential,
                id,
                *object,
                frame.clone(),
                Duration::from_secs(5)
            )
            .await
            .unwrap()
            .status
            .is_success()
        );
    }
    drop(server);
    let server = f.start().await;
    assert_eq!(result(request(&f, &begin))["outcome"], "receiving");
    for (object, frame) in &frames {
        assert!(
            client::send_github_source_object(
                &f.config.socket,
                &credential,
                id,
                *object,
                frame.clone(),
                Duration::from_secs(5)
            )
            .await
            .unwrap()
            .status
            .is_success()
        );
    }
    let finish = json!({"action":"import_finish","key":"finish","epoch":1,"import":id.to_string()});
    let completed = result(request(&f, &finish));
    assert_eq!(completed["git_revision"], revision);
    drop(server);
    drop(pack);
    if !benchmark.as_ref().is_some_and(|b| b.reuse_git_pack) {
        fs::remove_dir_all(&pack_path).unwrap();
    }
    let server = f.start().await;
    assert_eq!(result(request(&f, &finish)), completed);
    // A completed actual CLI import does not require its App key or local spool.
    let replay = Command::new(&f.binary)
        .env("PATH", "/no-executables")
        .args(["github", "import", "--config"])
        .arg(f.dir.path().join("absent.json"))
        .arg("--socket")
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--import", &id.to_string(), "--epoch", "1", "--state"])
        .arg(f.dir.path().join("absent-state"))
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        completed
    );
    assert!(!f.dir.path().join("absent-state").exists());
    f.credential_path = owner;
    if let Some(benchmark) = benchmark {
        let postgres_version: String = f
            .sql
            .query_one("SHOW server_version", &[])
            .await
            .unwrap()
            .get(0);
        let context = f.dir.path().join("benchmark-context.json");
        let payload = json!({
            "config": f.config_path,
            "socket": f.config.socket,
            "credential": f.credential_path,
            "store": f.config.storage.as_ref().unwrap().root,
            "tenant": f.scope.tenant(),
            "repository_id": f.scope.repository().to_string(),
            "native_revision": RevisionRef::Native(root).to_string(),
            "git_revision": revision,
            "import_capture_seconds": capture_seconds,
            "git_import_pack": benchmark.reuse_git_pack.then_some(&pack_path),
            "postgres_version": postgres_version,
        });
        files::write_private(&context, &serde_json::to_vec(&payload).unwrap()).unwrap();
        let driver = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../scripts/benchmark-workspace-preparation.py");
        let status = Command::new("python3")
            .arg("-B")
            .arg(driver)
            .args(benchmark.arguments)
            .arg("--binary")
            .arg(&f.binary)
            .arg("--connected-fixture")
            .arg(context)
            .status();
        drop(server);
        f.cleanup().await;
        assert!(status.unwrap().success());
        return;
    }
    let restored = f.dir.path().join("restored");
    let export = f.cli(&[
        "source-export",
        "--revision",
        &RevisionRef::Native(root).to_string(),
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--output",
        restored.to_str().unwrap(),
    ]);
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let restored_pack = treeharbor_service::pack::Pack::open(&restored).unwrap();
    for (id, bytes) in frames {
        assert_eq!(restored_pack.frame(id).unwrap(), bytes);
    }
    let git_output = f.dir.path().join("restored-git");
    let exported = Command::new(&f.binary)
        .args(["git-export", "--pack"])
        .arg(&restored)
        .arg("--output")
        .arg(&git_output)
        .args(["--git", "/usr/bin/git"])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let git_manifest: Value =
        serde_json::from_slice(&fs::read(git_output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        git_manifest["refs"]["refs/heads/main"]["git"],
        revision.strip_prefix("git:sha1:").unwrap()
    );
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_native_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    prepare_and_recover_git_workspace(&f, root, &revision).await;
    drop(server);
    f.cleanup().await;
}

async fn prepare_and_recover_git_workspace(
    f: &Fixture,
    root: treeharbor_core::ObjectId,
    revision: &str,
) {
    let profile = result(f.cli(&[
        "profile-put",
        "--key",
        "git-full",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--full",
    ]));
    let create = |workspace: &str| {
        result(f.cli(&[
            "workspace-create",
            "--key",
            &format!("create-{workspace}"),
            "--authority",
            "connected",
            "--epoch",
            "1",
            "--workspace",
            workspace,
            "--base",
            &RevisionRef::Native(root).to_string(),
            "--profile-digest",
            profile["profile_digest"].as_str().unwrap(),
            "--git-compatible",
        ]));
    };
    let workspace = Uuid::new_v4().to_string();
    create(&workspace);
    let node_root = f.dir.path().join("git-node");
    let node_init = Command::new(&f.binary)
        .args(["node", "initialize", "--root"])
        .arg(&node_root)
        .args(["--tenant", f.scope.tenant(), "--principal", "git-node"])
        .output()
        .unwrap();
    assert!(
        node_init.status.success(),
        "{}",
        String::from_utf8_lossy(&node_init.stderr)
    );
    let identity: Value = serde_json::from_slice(&node_init.stdout).unwrap();
    let storage = identity["storage"].as_str().unwrap();
    let repo = f.scope.repository().to_string();
    let tenant = f.scope.tenant();
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        tenant,
        "--principal",
        "git-node",
        "--kind",
        "node",
    ]);
    f.admin_ok(&[
        "node-register",
        "--tenant",
        tenant,
        "--principal",
        "git-node",
        "--storage",
        storage,
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        tenant,
        "--repository",
        &repo,
        "--principal",
        "git-node",
        "--permission",
        "runtime.control",
    ]);
    let credential = f.dir.path().join("git-node-token.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        tenant,
        "--repository",
        &repo,
        "--principal",
        "git-node",
        "--permission",
        "runtime.control",
        "--output",
        credential.to_str().unwrap(),
    ]);
    #[cfg(target_os = "linux")]
    git_workspace_runtime::exercise_if_configured(
        f,
        root,
        revision,
        &node_root,
        &credential,
        storage,
        profile["profile_digest"].as_str().unwrap(),
    );
    let node_for = |workspace: &str, action: &str, extra: &[&str]| {
        Command::new(&f.binary)
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["node", action, "--config"])
            .arg(&f.config_path)
            .arg("--credential-file")
            .arg(&credential)
            .arg("--root")
            .arg(&node_root)
            .args(["--storage", storage, "--workspace", workspace])
            .args(extra)
            .output()
            .unwrap()
    };
    let node = |action, extra: &[&str]| node_for(&workspace, action, extra);
    let output = |response: Output| -> Value {
        assert!(
            response.status.success(),
            "{}",
            String::from_utf8_lossy(&response.stderr)
        );
        serde_json::from_slice(&response.stdout).unwrap()
    };
    assert_eq!(output(node("prepare-once", &[]))["state"], "idle");
    assert!(
        !node("prepare-once", &["--git", "/usr/bin/false"])
            .status
            .success()
    );
    let slot: Uuid = f
        .sql
        .query_one(
            "SELECT slot FROM th_workspace_preparations WHERE workspace::text=$1",
            &[&workspace],
        )
        .await
        .unwrap()
        .get(0);
    let placement = node_root.join("slots").join(slot.to_string()).join("tree");
    assert!(!placement.exists());
    let expire = || async {
        f.sql.execute("UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE kind='workspace.prepare' AND payload->>'workspace'=$1", &[&workspace]).await.unwrap();
    };
    expire().await;
    f.sql.batch_execute(
        "CREATE FUNCTION reject_git_ack() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='prepared' THEN RAISE EXCEPTION 'injected acknowledgement failure'; END IF; RETURN NEW; END; $$; CREATE TRIGGER reject_git_ack BEFORE UPDATE ON th_workspace_preparations FOR EACH ROW EXECUTE FUNCTION reject_git_ack();"
    ).await.unwrap();
    let git = f.dir.path().join("counted-git");
    fs::write(
        &git,
        b"#!/bin/sh\nprintf 'invoked\\n' >> \"$0.calls\"\nexec /usr/bin/git \"$@\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&git, fs::Permissions::from_mode(0o700)).unwrap();
    let git_args = ["--git", git.to_str().unwrap()];
    assert!(!node("prepare-once", &git_args).status.success());
    let calls = git.with_extension("calls");
    let cold_calls = fs::read(&calls).unwrap();
    assert!(!cold_calls.is_empty());
    let files = placement.join("files");
    assert!(files.join(".git/index").is_file());
    assert!(placement.join("publication.json").is_file());
    assert_eq!(fs::read(files.join("main.rs")).unwrap(), b"fn main() {}\n");
    let head = git_reference::git(&files, &["rev-parse", "HEAD"], b"");
    assert_eq!(
        String::from_utf8_lossy(&head).trim(),
        revision.strip_prefix("git:sha1:").unwrap()
    );
    assert!(git_reference::git(&files, &["status", "--porcelain"], b"").is_empty());
    f.sql
        .batch_execute("DROP TRIGGER reject_git_ack ON th_workspace_preparations")
        .await
        .unwrap();
    expire().await;
    let recovered = output(node("prepare-once", &["--git", "/usr/bin/false"]));
    assert_eq!(recovered["state"], "files_ready");
    assert_eq!(recovered["recovered_baseline"], true);
    assert_eq!(
        PathBuf::from(recovered["path"].as_str().unwrap()),
        files.canonicalize().unwrap()
    );

    // Once acknowledged, neither Git metadata nor working edits are regenerated.
    fs::write(files.join("main.rs"), b"agent edit").unwrap();
    fs::write(files.join(".git/config"), b"invalid agent metadata").unwrap();
    assert_eq!(output(node("prepared", &[]))["state"], "files_ready");
    assert_eq!(fs::read(files.join("main.rs")).unwrap(), b"agent edit");
    assert_eq!(
        fs::read(files.join(".git/config")).unwrap(),
        b"invalid agent metadata"
    );

    // A new process, workspace and slot reuse only the checked seed. The first
    // workspace is still present with edited files and invalid Git config.
    let warm_workspace = Uuid::new_v4().to_string();
    create(&warm_workspace);
    let warm = output(node_for(&warm_workspace, "prepare-once", &git_args));
    assert_eq!(warm["state"], "files_ready");
    assert_eq!(warm["preparation_statistics"]["source_objects"], 0);
    assert_eq!(warm["preparation_statistics"]["read_batches"], 0);
    assert!(
        warm["preparation_statistics"]["local_bytes_copied"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(fs::read(&calls).unwrap(), cold_calls);
    let warm_files = PathBuf::from(warm["path"].as_str().unwrap());
    assert_eq!(
        git_reference::git(&warm_files, &["rev-parse", "HEAD"], b""),
        head
    );
    assert!(git_reference::git(&warm_files, &["status", "--porcelain"], b"").is_empty());
    git_reference::git(&warm_files, &["fsck", "--strict", "--no-dangling"], b"");
    assert_eq!(fs::read(files.join("main.rs")).unwrap(), b"agent edit");

    let damaged_seed = node_root.parent().unwrap().join(format!(
        ".treeharbor-source-seed-v1-{storage}/ready/contents-v1"
    ));
    let mut contents = fs::read(&damaged_seed).unwrap();
    *contents.last_mut().unwrap() ^= 1;
    fs::write(damaged_seed, contents).unwrap();
    let rebuilt_workspace = Uuid::new_v4().to_string();
    create(&rebuilt_workspace);
    let rebuilt = output(node_for(&rebuilt_workspace, "prepare-once", &git_args));
    assert_eq!(rebuilt["state"], "files_ready");
    assert!(
        rebuilt["preparation_statistics"]["source_objects"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(rebuilt["preparation_statistics"]["local_bytes_copied"], 0);
    let rebuilt_files = PathBuf::from(rebuilt["path"].as_str().unwrap());
    assert_eq!(
        fs::read(rebuilt_files.join("main.rs")).unwrap(),
        b"fn main() {}\n"
    );
    assert!(git_reference::git(&rebuilt_files, &["status", "--porcelain"], b"").is_empty());
    assert_eq!(fs::read(calls).unwrap(), cold_calls);

    for (id, path) in [
        (&workspace, files),
        (&warm_workspace, warm_files),
        (&rebuilt_workspace, rebuilt_files),
    ] {
        result(f.cli(&[
            "workspace-delete",
            "--key",
            &format!("drain-{id}"),
            "--authority",
            "connected",
            "--epoch",
            "1",
            "--workspace",
            id,
            "--generation",
            "1",
        ]));
        output(node_for(id, "cleanup-once", &[]));
        assert!(!path.exists());
        assert_eq!(
            result(f.cli(&["workspace-status", "--workspace", id]))["state"],
            "deleted"
        );
    }
}

fn request(f: &Fixture, value: &Value) -> Output {
    let path = f
        .dir
        .path()
        .join(format!("request-{}.json", Uuid::new_v4()));
    files::write_private(&path, &serde_json::to_vec(value).unwrap()).unwrap();
    f.cli(&["github", "--request", path.to_str().unwrap()])
}
fn result(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"].clone()
}
fn verify(f: &Fixture, binding: &str, probe: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["github", "verify", "--config"])
        .arg(f.dir.path().join("absent-app-key-config.json"))
        .arg("--socket")
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--binding", binding, "--probe", probe, "--epoch", "1"])
        .output()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual Connected CLI and service restart, trusted observation fixture, no live App"]
async fn github_cli_replays_observation_after_restart_without_app_key_and_fences_revocation() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = f.dir.path().join("bridge.json");
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        "bridge",
        "--kind",
        "bridge",
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.read",
    ]);
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.read",
        "--output",
        bridge.to_str().unwrap(),
    ]);
    let server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    let binding = bind["binding"].as_str().unwrap();
    assert_eq!(result(request(&f, &bind))["outcome"], "configured");
    let status = result(request(
        &f,
        &json!({"action":"status","epoch":1,"binding":binding}),
    ));
    assert_eq!(status["state"], "active");
    assert_eq!(status["publication_ready"], false);
    f.credential_path = f.dir.path().join("agent.json");
    let probe = Uuid::new_v4().to_string();
    let begin = json!({"action":"probe_begin","epoch":1,"key":format!("github-probe-start-{probe}"),"binding":binding,"probe":probe});
    assert!(!request(&f, &begin).status.success());
    f.credential_path = bridge;
    assert!(!request(&f, &bind).status.success());
    assert_eq!(result(request(&f, &begin))["outcome"], "requested");
    // Kill the actual server before observation publication; retain the attempt.
    drop(server);
    let server = f.start().await;
    assert_eq!(result(request(&f, &begin))["outcome"], "requested");
    // A locally substituted App must fail before making any GitHub request.
    // The public fixture key is not an installed App credential.
    let app_config = f.dir.path().join("absent-app-key-config.json");
    let app_key = f.dir.path().join("fixture-key.pem");
    files::write_private(
        &app_key,
        include_bytes!("../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    files::write_private(
        &app_config,
        serde_json::to_vec(&json!({
            "version":1,"app_id":8,"client_id":"Iv1_public_test","private_key_file":app_key,
            "repository":bind["specification"]["upstream"]
        }))
        .unwrap()
        .as_slice(),
    )
    .unwrap();
    let mismatch = verify(&f, binding, &probe);
    assert!(!mismatch.status.success());
    assert!(String::from_utf8_lossy(&mismatch.stderr).contains("differs from the pinned identity"));
    assert_eq!(
        result(request(
            &f,
            &json!({"action":"probe_context","epoch":1,"binding":binding,"probe":probe})
        ))["state"],
        "pending"
    );
    fs::remove_file(app_config).unwrap();
    fs::remove_file(app_key).unwrap();
    let observation: Value = serde_json::from_str(include_str!(
        "../../../integrations-github/tests/fixtures/probe-v1.json"
    ))
    .unwrap();
    let finish = json!({"action":"probe_finish","epoch":1,"key":format!("github-probe-finish-{probe}"),"binding":binding,"probe":probe,"observation":observation});
    let accepted = result(request(&f, &finish));
    assert_eq!(accepted["observation"], observation);
    // Lose the server after commit and retrieve only the durable result. No
    // installed App or network observation is claimed by this trusted fixture.
    drop(server);
    let server = f.start().await;
    assert_eq!(result(request(&f, &finish)), accepted);
    let replay = verify(&f, binding, &probe);
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    let replay: Value = serde_json::from_slice(&replay.stdout).unwrap();
    assert_eq!(replay["state"], "observed");
    assert_eq!(replay["observation"], observation);
    assert_eq!(replay["publication_ready"], false);
    assert!(
        !request(
            &f,
            &json!({"action":"revoke","key":"bridge-cannot-revoke","epoch":1,"binding":binding})
        )
        .status
        .success()
    );
    let bridge = f.credential_path.clone();
    f.credential_path = owner;
    assert_eq!(
        result(request(
            &f,
            &json!({"action":"revoke","key":"owner-revoke","epoch":1,"binding":binding})
        ))["outcome"],
        "revoked"
    );
    assert_eq!(result(request(&f, &bind))["outcome"], "configured");
    assert_eq!(
        result(request(
            &f,
            &json!({"action":"status","epoch":1,"binding":binding})
        ))["state"],
        "revoked"
    );
    f.credential_path = bridge;
    assert!(!verify(&f, binding, &probe).status.success());
    assert_eq!(result(request(&f, &finish)), accepted);
    let count: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='github.connection.observed'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 1);
    let log = fs::read_to_string(f.dir.path().join("server.log")).unwrap();
    assert!(!log.contains("thv1_"));
    drop(server);
    f.cleanup().await;
}

async fn message(stream: &mut UnixStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 8192];
        let n = stream.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            assert!(!headers.contains("transfer-encoding:"));
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .unwrap_or("0")
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return bytes;
            }
        }
    }
}
