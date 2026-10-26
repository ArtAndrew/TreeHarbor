//! Actual authenticated CLI/service and PostgreSQL recovery. Policy definitions
//! and upstream identities are synthetic fixtures; no provider is contacted.
use super::*;
use std::path::Path;

fn owner(f: &Fixture) -> PathBuf {
    let path = f.dir.path().join("admission-owner.json");
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        "admission-owner",
        "--kind",
        "human",
    ]);
    for action in ["access-set", "token-issue"] {
        let mut args = vec![action, "--tenant", f.scope.tenant(), "--repository"];
        let repository = f.scope.repository().to_string();
        args.extend([
            &repository,
            "--principal",
            "admission-owner",
            "--permission",
            "policy.write",
            "--permission",
            "read",
        ]);
        if action == "token-issue" {
            args.extend(["--output", path.to_str().unwrap()]);
        }
        f.admin_ok(&args);
    }
    path
}

fn policy_cli(f: &Fixture, request: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["client", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["github", "--request"])
        .arg(request);
    command
}

async fn counts(f: &Fixture) -> Vec<i64> {
    let row = f.sql.query_one(
        "SELECT (SELECT count(*) FROM th_operations), (SELECT count(*) FROM th_outbox), (SELECT count(*) FROM th_github_admission_policies), (SELECT count(*) FROM th_github_admission_refs)",
        &[],
    ).await.unwrap();
    (0..4).map(|i| row.get(i)).collect()
}

async fn before_commit(f: &Fixture, server: &mut Process, request: &Path) {
    // The actual service begins its transaction, then blocks on this isolated
    // schema's policy relation. Killing it here must leave no command receipt.
    f.sql
        .batch_execute("BEGIN; LOCK TABLE th_github_admission_refs IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let diagnostic = f.dir.path().join("admission-before.stderr");
    let mut cli = Process(
        policy_cli(f, request)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting: i64 = f.sql.query_one(
            "SELECT count(*) FROM pg_locks WHERE relation='th_github_admission_refs'::regclass AND NOT granted",
            &[],
        ).await.unwrap().get(0);
        if waiting > 0 {
            break;
        }
        assert!(
            cli.0.try_wait().unwrap().is_none() && Instant::now() < deadline,
            "service did not reach the blocked transaction: {}",
            fs::read_to_string(&diagnostic).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    assert!(!cli.0.wait().unwrap().success());
    f.sql.batch_execute("ROLLBACK").await.unwrap();
    *server = f.start().await;
}

async fn after_commit(f: &mut Fixture, server: &mut Process, request: &Path) -> Value {
    let socket = f.dir.path().join("admission-lost-reply.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let original = f.config.socket.clone();
    let upstream = original.clone();
    let (signal, received) = tokio::sync::oneshot::channel();
    let hold = tokio::spawn(async move {
        let (mut incoming, _) = listener.accept().await.unwrap();
        let request = message(&mut incoming).await;
        assert!(String::from_utf8_lossy(&request).contains("\"action\":\"admission_policy_set\""));
        let mut outgoing = UnixStream::connect(upstream).await.unwrap();
        outgoing.write_all(&request).await.unwrap();
        let response = message(&mut outgoing).await;
        assert!(
            response.starts_with(b"HTTP/1.1 200 "),
            "{}",
            String::from_utf8_lossy(&response)
        );
        let body = response.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        let response: Value = serde_json::from_slice(&response[body..]).unwrap();
        signal.send(response["result"].clone()).unwrap();
        // Retain both sockets without delivering any response to the actual CLI.
        std::future::pending::<()>().await;
    });
    f.config.socket = socket;
    let output = f.dir.path().join("admission-lost.stdout");
    let diagnostic = f.dir.path().join("admission-lost.stderr");
    let mut cli = Process(
        policy_cli(f, request)
            .stdout(fs::File::create(&output).unwrap())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let committed = tokio::time::timeout(Duration::from_secs(10), received)
        .await
        .unwrap_or_else(|_| panic!("{}", fs::read_to_string(&diagnostic).unwrap()))
        .unwrap();
    assert!(cli.0.try_wait().unwrap().is_none());
    assert!(fs::read(output).unwrap().is_empty());
    assert_eq!(counts(f).await[3], 1);
    cli.0.kill().unwrap();
    cli.0.wait().unwrap();
    hold.abort();
    let _ = hold.await;
    f.config.socket = original;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    committed
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual CLI/service process loss before/after commit, synthetic policy and host identity, no provider access"]
async fn admission_policy_cli_replays_lost_commit_and_rotates_contexts_without_changing_legacy_policy()
 {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    f.config.request_timeout_ms = 15000;
    f.save_config();
    let importer = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let owner = owner(&f);
    let mut server = f.start().await;
    let binding: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &binding));
    let binding = binding["binding"].clone();
    let v: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/connected-admission-policy-v1.json"
    ))
    .unwrap();
    let definition = v["policy"].clone();
    let plan = json!({"action":"admission_policy_plan","epoch":1,"binding":binding,"base_ref":"main","expected_generation":0,"definition":definition});
    let set = json!({"action":"admission_policy_set","key":"admission-first","epoch":1,"binding":binding,"base_ref":"main","expected_generation":0,"definition":definition});
    let read = json!({"action":"admission_policy","epoch":1,"binding":binding,"base_ref":"main"});
    let legacy_read =
        json!({"action":"check_policy","epoch":1,"binding":binding,"base_ref":"main"});

    for credential in [importer, bridge, f.dir.path().join("agent.json")] {
        f.credential_path = credential;
        assert!(!request(&f, &plan).status.success());
        assert!(!request(&f, &set).status.success());
    }
    // A normal reader can inspect adoption state, without gaining policy rights.
    let unconfigured = result(request(&f, &read));
    assert_eq!(unconfigured["generation"], 0);
    assert_eq!(unconfigured["state"], "unconfigured");
    assert!(unconfigured["policy"].is_null());
    f.credential_path = owner;

    let legacy: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/workflow-policy-v1.json"
    ))
    .unwrap();
    result(request(
        &f,
        &json!({"action":"check_policy_set","key":"legacy-workflow","epoch":1,"binding":binding,"base_ref":"main","expected_generation":0,"definition":legacy}),
    ));
    let legacy_before = result(request(&f, &legacy_read));
    assert_eq!(legacy_before["definition"], legacy);

    let before = counts(&f).await;
    let preview = result(request(&f, &plan));
    assert_eq!(result(request(&f, &plan)), preview);
    assert_eq!(preview["approved"], false);
    assert_eq!(preview["expected_generation"], 0);
    assert_eq!(preview["generation"], 1);
    assert_eq!(preview["policy"], v["digest"]);
    assert_eq!(preview["rules"], definition["rules"]);
    // Names are independent Python hashlib vectors for the fixed binding above.
    let expected = json!([
        {"app_id":7,"name":"CI / build"},
        {"app_id":7,"name":"TreeHarbor/e1/abac01f8d5f95ef3b6c12278a6e31126cf0f820f3d13aa46eb0c22877aa8717ee"},
        {"app_id":7,"name":"TreeHarbor/e1/aecc51a1a380edfc5cee32429db126d098e2996a8a6ad01714a04b7df5f661ea1"}
    ]);
    assert_eq!(preview["required"], expected);
    assert_eq!(counts(&f).await, before);
    assert_eq!(result(request(&f, &read)), unconfigured);

    let saved_request = f.dir.path().join("admission-set.json");
    files::write_private(&saved_request, &serde_json::to_vec(&set).unwrap()).unwrap();
    before_commit(&f, &mut server, &saved_request).await;
    assert_eq!(counts(&f).await, before);
    assert_eq!(result(request(&f, &read)), unconfigured);
    let committed = after_commit(&mut f, &mut server, &saved_request).await;
    assert_eq!(committed["generation"], 1);
    assert_eq!(committed["policy"], v["digest"]);
    assert_eq!(committed["required"], expected);
    assert_eq!(result(request(&f, &set)), committed);
    let first = result(request(&f, &read));
    assert_eq!(first["state"], "active");
    assert_eq!(first["generation"], 1);
    assert_eq!(first["definition"], definition);
    assert_eq!(first["required"], expected);
    assert_eq!(first["policy"], v["digest"]);
    assert_eq!(counts(&f).await, vec![before[0] + 1, before[1] + 1, 1, 1]);

    let mut stale = set.clone();
    stale["key"] = json!("stale-generation");
    assert!(!request(&f, &stale).status.success());
    assert!(!request(&f, &plan).status.success());
    let mut conflict = set.clone();
    conflict["definition"]["rules"]["sha256"] = json!("cd".repeat(32));
    assert!(!request(&f, &conflict).status.success());
    assert_eq!(result(request(&f, &read)), first);

    let mut replacement = set.clone();
    replacement["key"] = json!("admission-replace");
    replacement["expected_generation"] = json!(1);
    replacement["definition"]["managed"][0]["validators"] = json!(["trusted-ci"]);
    let changed = result(request(&f, &replacement));
    assert_eq!(changed["generation"], 2);
    assert_ne!(changed["policy"], v["digest"]);
    assert_ne!(changed["required"], expected);
    let revoke = json!({"action":"admission_policy_set","key":"admission-revoke","epoch":1,"binding":binding,"base_ref":"main","expected_generation":2,"definition":null});
    let revoked = result(request(&f, &revoke));
    assert_eq!(revoked["generation"], 3);
    assert!(revoked["policy"].is_null());
    assert!(revoked["required"].is_null());
    let tombstone = result(request(&f, &read));
    assert_eq!(tombstone["state"], "revoked");
    assert_eq!(tombstone["generation"], 3);
    assert!(tombstone["definition"].is_null());
    assert_eq!(result(request(&f, &set)), committed);
    assert_eq!(result(request(&f, &read)), tombstone);
    assert_eq!(result(request(&f, &replacement)), changed);
    assert_eq!(result(request(&f, &read)), tombstone);

    let mut restore = set.clone();
    restore["key"] = json!("admission-restore");
    restore["expected_generation"] = json!(3);
    let restored = result(request(&f, &restore));
    assert_eq!(restored["generation"], 4);
    assert_eq!(restored["policy"], v["digest"]);
    let restored_required = json!([
        {"app_id":7,"name":"CI / build"},
        {"app_id":7,"name":"TreeHarbor/e1/a424cbf04608c194fbf42a55ae8c5c4f015372b4c1724d5346a814902f423477c"},
        {"app_id":7,"name":"TreeHarbor/e1/a8695e1d2058671938e37095a08adfbb788eb5094437d45314b1a07496dc0a510"}
    ]);
    assert_eq!(restored["required"], restored_required);
    assert_ne!(restored["required"], expected);
    assert_eq!(result(request(&f, &legacy_read)), legacy_before);
    assert_eq!(counts(&f).await, vec![before[0] + 4, before[1] + 4, 2, 1]);
    let status = result(request(
        &f,
        &json!({"action":"status","epoch":1,"binding":binding}),
    ));
    assert_eq!(status["publication_ready"], false);

    drop(server);
    let server = f.start().await;
    assert_eq!(result(request(&f, &restore)), restored);
    let final_read = result(request(&f, &read));
    assert_eq!(final_read["generation"], 4);
    assert_eq!(final_read["definition"], definition);
    assert_eq!(final_read["required"], restored_required);
    assert_eq!(result(request(&f, &legacy_read)), legacy_before);
    let mut other_base = read;
    other_base["base_ref"] = json!("legacy");
    assert_eq!(result(request(&f, &other_base))["state"], "unconfigured");
    assert_eq!(counts(&f).await, vec![before[0] + 4, before[1] + 4, 2, 1]);
    drop(server);
    f.cleanup().await;
}

fn reviewed_policy_cli(f: &Fixture, action: &str, definition: &Path, binding: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["github", action, "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args([
            "--binding",
            binding,
            "--base",
            "main",
            "--epoch",
            "1",
            "--expected-generation",
            "0",
            "--definition",
        ])
        .arg(definition);
    command
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual v2 owner policy CLI/service crash before and after commit; host data are fixtures"]
async fn reviewed_configuration_policy_survives_lost_reply_without_reapproving_or_losing_v1() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    f.config.request_timeout_ms = 15000;
    f.save_config();
    provision_bridge(&f);
    let owner = owner(&f);
    let mut server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &bind));
    let binding = bind["binding"].clone();
    f.credential_path = owner;
    let v: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/connected-admission-policy-v2.json"
    ))
    .unwrap();
    let set = json!({"action":"admission_policy_set","key":"reviewed-configuration","epoch":1,"binding":binding,"base_ref":"main","expected_generation":0,"definition":v["policy"]});
    let read = json!({"action":"admission_policy","epoch":1,"binding":binding,"base_ref":"main"});
    let saved = f.dir.path().join("configuration-set.json");
    files::write_private(&saved, &serde_json::to_vec(&set).unwrap()).unwrap();
    let before = counts(&f).await;
    let definition = f.dir.path().join("reviewed-policy.json");
    files::write_private(&definition, &serde_json::to_vec(&v["policy"]).unwrap()).unwrap();
    let planned = reviewed_policy_cli(&f, "policy-plan", &definition, binding.as_str().unwrap())
        .output()
        .unwrap();
    assert!(
        planned.status.success(),
        "{}",
        String::from_utf8_lossy(&planned.stderr)
    );
    let planned: Value = serde_json::from_slice(&planned.stdout).unwrap();
    assert_eq!(planned["approved"], false);
    assert_eq!(planned["host_configuration"]["required_matches"], true);
    assert_eq!(counts(&f).await, before);
    before_commit(&f, &mut server, &saved).await;
    assert_eq!(counts(&f).await, before);
    assert_eq!(result(request(&f, &read))["state"], "unconfigured");
    let committed = after_commit(&mut f, &mut server, &saved).await;
    assert_eq!(committed["policy"], v["digest"]);
    assert_eq!(committed["generation"], 1);
    let replay = reviewed_policy_cli(&f, "policy-adopt", &definition, binding.as_str().unwrap())
        .args(["--key", "reviewed-configuration"])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        committed
    );
    assert_eq!(result(request(&f, &set)), committed);
    assert_eq!(result(request(&f, &read))["definition"], v["policy"]);
    let revoke = json!({"action":"admission_policy_set","key":"reviewed-revoke","epoch":1,"binding":binding,"base_ref":"main","expected_generation":1,"definition":null});
    result(request(&f, &revoke));
    assert_eq!(result(request(&f, &set)), committed);
    assert_eq!(result(request(&f, &read))["state"], "revoked");
    let old: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/connected-admission-policy-v1.json"
    ))
    .unwrap();
    let legacy = json!({"action":"admission_policy_set","key":"legacy-explicit","epoch":1,"binding":binding,"base_ref":"main","expected_generation":2,"definition":old["policy"]});
    let legacy = result(request(&f, &legacy));
    assert_eq!(legacy["policy"], old["digest"]);
    assert_eq!(legacy["generation"], 3);
    assert_eq!(result(request(&f, &read))["definition"], old["policy"]);
    assert_eq!(counts(&f).await, vec![before[0] + 3, before[1] + 3, 2, 1]);
    drop(server);
    f.cleanup().await;
}
