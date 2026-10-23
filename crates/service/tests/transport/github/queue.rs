use super::*;
fn cli(f: &Fixture, id: Uuid, state: &Path, app: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args(["github", "queue", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--queue", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(app);
    c
}
async fn proxy(
    f: &mut Fixture,
    name: &str,
    action: &'static str,
    expected: &'static str,
) -> (
    PathBuf,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let path = f.dir.path().join(name);
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = f.config.socket.clone();
    let upstream = original.clone();
    f.config.socket = path;
    let (signal, received) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let mut signal = Some(signal);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut incoming, _) = listener.accept().await.unwrap();
            let bytes = message(&mut incoming).await;
            let hold = String::from_utf8_lossy(&bytes).contains(action);
            let signal = if hold { signal.take() } else { None };
            let upstream = upstream.clone();
            clients.spawn(async move {
                let mut outgoing = UnixStream::connect(upstream).await.unwrap();
                outgoing.write_all(&bytes).await.unwrap();
                if let Some(signal) = signal {
                    let reply = message(&mut outgoing).await;
                    assert!(String::from_utf8_lossy(&reply).contains(expected));
                    signal.send(()).unwrap();
                    std::future::pending::<()>().await;
                } else {
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                }
            });
        }
    });
    (original, handle, received)
}
pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    publisher: &Path,
    inspection: Uuid,
) {
    f.credential_path = owner.to_owned();
    let id = Uuid::new_v4();
    let create = json!({"action":"queue_request","key":"queue-create","epoch":1,"queue":id.to_string(),"specification":{"inspection":inspection.to_string(),"ttl_seconds":600}});
    assert!(!request(f, &create).status.success());
    f.admin_ok(&[
        "principal-create",
        "--tenant",
        f.scope.tenant(),
        "--principal",
        "queue-owner",
        "--kind",
        "human",
    ]);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "queue-owner",
        "--permission",
        "host.queue",
        "--permission",
        "read",
    ]);
    let queue_owner = f.dir.path().join("queue-owner.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "queue-owner",
        "--permission",
        "host.queue",
        "--permission",
        "read",
        "--output",
        queue_owner.to_str().unwrap(),
    ]);
    f.credential_path = queue_owner.clone();
    let requested = result(request(f, &create));
    assert_eq!(requested["state"], "requested");
    assert_eq!(result(request(f, &create)), requested);
    f.credential_path = publisher.to_owned();
    assert!(!request(f,&json!({"action":"queue_begin","key":"publisher-cannot-merge","epoch":1,"queue":id.to_string(),"attempt":Uuid::new_v4().to_string()})).status.success());
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
        "--permission",
        "host.publish",
        "--permission",
        "host.merge",
    ]);
    let bridge = f.dir.path().join("queue-bridge.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.merge",
        "--output",
        bridge.to_str().unwrap(),
    ]);
    f.credential_path = bridge;
    let context = result(request(
        f,
        &json!({"action":"queue_context","epoch":1,"queue":id.to_string()}),
    ));
    let state = f.dir.path().join("queue-driver");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let pem = f.dir.path().join("queue-fixture.pem");
    let app = f.dir.path().join("queue-fixture.json");
    files::write_private(
        &pem,
        include_bytes!("../../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    files::write_private(&app,json!({"version":1,"app_id":context["specification"]["app_id"],"client_id":context["specification"]["client_id"],"private_key_file":pem,"repository":context["specification"]["upstream"]}).to_string().as_bytes()).unwrap();
    // Actual CLI's committed dispatch reply is withheld, so no GitHub call can run.
    let (original, hold, received) = proxy(
        f,
        "queue-dispatch.sock",
        "\"action\":\"queue_begin\"",
        "\"dispatch_granted\":true",
    )
    .await;
    let diagnostic = f.dir.path().join("queue-cli.stderr");
    let mut child = Process(
        cli(f, id, &state, &app)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let outcome = tokio::time::timeout(Duration::from_secs(30), received).await;
    assert!(
        outcome.is_ok(),
        "CLI {:?}: {}",
        child.0.try_wait().unwrap(),
        fs::read_to_string(&diagnostic).unwrap()
    );
    outcome.unwrap().unwrap();
    f.config.socket = original;
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    hold.abort();
    let _ = hold.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let c = result(request(
        f,
        &json!({"action":"queue_context","epoch":1,"queue":id.to_string()}),
    ));
    let attempt: Value =
        serde_json::from_slice(&fs::read(state.join("attempt.json")).unwrap()).unwrap();
    assert_eq!(c["attempt"], attempt["attempt"]);
    assert_eq!(c["state"], "uncertain");
    let begin = json!({"action":"queue_begin","key":format!("github-queue-begin-{id}"),"epoch":1,"queue":id.to_string(),"attempt":attempt["attempt"]});
    assert_eq!(result(request(f, &begin))["dispatch_granted"], false);
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    // Explicit provider fixture, NOT installed-App or real merge-group evidence.
    let mut pull = json!({"repository_id":c["target"]["pull"]["repository_id"],"number":c["target"]["pull"]["pull_number"],"node_id":c["target"]["pull"]["pull_node"],"head_ref":c["target"]["pull"]["head_ref"],"base_ref":c["target"]["pull"]["base_ref"],"head_revision":c["target"]["pull"]["head_revision"],"base_revision":c["target"]["expected_base_revision"],"state":"closed","draft":false,"merged":true});
    let report = json!({"version":1,"started_at_unix_ms":now,"finished_at_unix_ms":now,"snapshot":{"pull":pull.take(),"queue_enabled":true,"configuration":null,"entry":null,"final_commit":{"revision":format!("git:sha1:{}","33".repeat(20)),"tree":format!("git:sha1:{}","44".repeat(20))}},"receipt":null,"async_result":null,"token_revoked":false});
    files::write_private(&state.join("report-0000.json"),json!({"version":1,"key":"github-publish-report-queue-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    fs::remove_file(&app).unwrap();
    fs::remove_file(&pem).unwrap();
    let (original, hold, received) = proxy(
        f,
        "queue-report.sock",
        "\"action\":\"queue_report\"",
        "\"state\":\"host_merged_unverified\"",
    )
    .await;
    let mut child = Process(
        cli(f, id, &state, &app)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let outcome = tokio::time::timeout(Duration::from_secs(30), received).await;
    assert!(
        outcome.is_ok(),
        "CLI {:?}: {}",
        child.0.try_wait().unwrap(),
        fs::read_to_string(&diagnostic).unwrap()
    );
    outcome.unwrap().unwrap();
    f.config.socket = original;
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    hold.abort();
    let _ = hold.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let out = cli(f, id, &state, &app).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recovered: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(recovered["observation"], report);
    assert_eq!(recovered["state"], "host_merged_unverified");
    assert_eq!(recovered["merge_verified"], false);
    assert_eq!(recovered["report_sequence"], 1);
    assert!(state.join("ack-0000.json").is_file());
    assert_eq!(result(request(f, &begin))["dispatch_granted"], false);
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.queue.observed'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    // Another observer already reported a newer host snapshot. The delayed
    // private journal still contains the sole async UUID and must not be discarded.
    let provider = Uuid::new_v4();
    let late = json!({"version":1,"started_at_unix_ms":now-1000,"finished_at_unix_ms":now-1000,
        "snapshot":null,"async_result":null,"token_revoked":true,
        "receipt":{"http_status":202,"status":"pending","request_uuid":provider.to_string(),
        "expected_head_revision":c["target"]["pull"]["head_revision"],"merge_action":"merge_queue","bypass_rules":false,"final_revision":null}});
    files::write_private(
        &state.join("report-0001.json"),
        json!({"version":1,"key":"github-publish-report-late-queue-uuid","report":late,"token_expires_at":null})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let out = cli(f, id, &state, &app).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recovered: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(recovered["provider_request"], provider.to_string());
    assert_eq!(recovered["observation"], report);
    assert_eq!(recovered["report_sequence"], 2);
    assert!(state.join("ack-0001.json").is_file());
    let at: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let async_report = json!({"version":1,"started_at_unix_ms":at,"finished_at_unix_ms":at,
        "snapshot":null,"receipt":null,"token_revoked":true,
        "async_result":{"http_status":200,"request_uuid":provider.to_string(),"outcome":{"status":"enqueued"}}});
    files::write_private(&state.join("report-0002.json"),json!({"version":1,"key":"github-publish-report-queue-terminal-result","report":async_report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let out = cli(f, id, &state, &app).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recovered: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        recovered["provider_result"]["outcome"]["status"],
        "enqueued"
    );
    assert_eq!(recovered["state"], "host_merged_unverified");
    assert_eq!(recovered["merge_verified"], false);
    assert_eq!(recovered["report_sequence"], 3);
    assert!(state.join("ack-0002.json").is_file());
    f.credential_path = queue_owner;
    let cancelled = result(request(
        f,
        &json!({"action":"queue_cancel","key":"cancel-queue","epoch":1,"queue":id.to_string()}),
    ));
    assert_eq!(cancelled["cancel_requested"], true);
    assert_eq!(cancelled["state"], "host_merged_unverified");
    assert_eq!(cancelled["merge_verified"], false);
    f.credential_path = owner.to_owned();
}
