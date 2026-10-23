use super::*;

fn provision_bridge(f: &Fixture) -> PathBuf {
    let bridge = f.dir.path().join("bridge-import.json");
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
    bridge
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual Git history, bridge HTTP, restart and export (no live App)"]
async fn github_source_http_preserves_exact_git_history_after_partial_upload_and_server_death() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let pack_path = f.dir.path().join("original-pack");
    git_history_pack(&f, &pack_path);
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
    fs::remove_dir_all(pack_path).unwrap();
    let server = f.start().await;
    assert_eq!(result(request(&f, &finish)), completed);
    // A completed actual CLI import does not require its App key or local spool.
    let replay = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
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
    f.credential_path = owner;
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
    let exported = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
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
    drop(server);
    f.cleanup().await;
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
