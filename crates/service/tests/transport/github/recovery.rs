use super::*;
use std::path::Path;

fn cli(f: &Fixture, id: Uuid, state: &Path, app: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args(["github", "recover-events", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--recovery", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(app);
    c
}
fn context(f: &Fixture, id: Uuid) -> Value {
    result(request(
        f,
        &json!({"action":"recovery_context","epoch":1,"recovery":id,"after":0}),
    ))
}
fn save(state: &Path, n: usize, saved: Value) {
    // Explicit provider fixture in the actual worker journal; not live-App evidence.
    files::write_private(&state.join(format!("report-{n:04}.json")),
        json!({"version":1,"key":format!("github-publish-report-recovery-fixture-{n}"),"report":saved,"token_expires_at":null}).to_string().as_bytes()).unwrap();
}
async fn time(f: &Fixture) -> i64 {
    f.sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}
// Hold a real committed response at the socket boundary, then kill both actual
// processes. The provider endpoint is never replaced in the production client.
async fn kill_after_commit(
    f: &mut Fixture,
    server: &mut Process,
    id: Uuid,
    state: &Path,
    app: &Path,
    action: &'static str,
) {
    let path = f.dir.path().join(format!("{action}.sock"));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = f.config.socket.clone();
    let upstream = original.clone();
    f.config.socket = path;
    let (sent, got) = tokio::sync::oneshot::channel();
    let hold = tokio::spawn(async move {
        let mut sent = Some(sent);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut input, _) = listener.accept().await.unwrap();
            let bytes = message(&mut input).await;
            let selected =
                String::from_utf8_lossy(&bytes).contains(&format!("\"action\":\"{action}\""));
            let signal = if selected { sent.take() } else { None };
            let upstream = upstream.clone();
            clients.spawn(async move {
                let mut output = UnixStream::connect(upstream).await.unwrap();
                output.write_all(&bytes).await.unwrap();
                if let Some(signal) = signal {
                    let reply = message(&mut output).await;
                    assert!(
                        reply.starts_with(b"HTTP/1.1 200 "),
                        "{}",
                        String::from_utf8_lossy(&reply)
                    );
                    signal.send(()).unwrap();
                    std::future::pending::<()>().await;
                } else {
                    let _ = tokio::io::copy_bidirectional(&mut input, &mut output).await;
                }
            });
        }
    });
    let diagnostic = f.dir.path().join(format!("{action}.stderr"));
    let mut worker = Process(
        cli(f, id, state, app)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let reached = tokio::time::timeout(Duration::from_secs(20), got).await;
    assert!(
        reached.is_ok(),
        "{action}, worker {:?}: {}",
        worker.0.try_wait().unwrap(),
        fs::read_to_string(&diagnostic).unwrap()
    );
    reached.unwrap().unwrap();
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    hold.abort();
    let _ = hold.await;
    f.config.socket = original;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL: real CLI/server crashes, durable journal and authenticated receiver; provider observations are fixtures"]
async fn github_recovery_cli_resumes_lost_commits_without_app_key_or_duplicate_dispatch() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let readonly = provision_bridge(&f);
    let mut server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &bind));
    f.credential_path = readonly.clone();
    let probe = Uuid::new_v4();
    result(request(
        &f,
        &json!({"action":"probe_begin","epoch":1,"key":"probe","binding":bind["binding"],"probe":probe}),
    ));
    let observation: Value = serde_json::from_str(include_str!(
        "../../../../integrations-github/tests/fixtures/probe-v1.json"
    ))
    .unwrap();
    result(request(
        &f,
        &json!({"action":"probe_finish","epoch":1,"key":"observe","binding":bind["binding"],"probe":probe,"observation":observation}),
    ));
    f.credential_path = owner.clone();
    let channel = Uuid::new_v4();
    result(request(
        &f,
        &json!({"action":"event_channel_create","key":"channel","epoch":1,"channel":channel,"specification":{"binding":bind["binding"],"max_events":8,"max_bytes":8192,"max_aliases":4}}),
    ));
    let id = Uuid::new_v4();
    let at = time(&f).await;
    result(request(
        &f,
        &json!({"action":"recovery_request","key":"recover","epoch":1,"recovery":id,"specification":{"channel":channel,"callback_url":"https://hooks.example.com/github/events","since_unix_ms":at-60000,"ttl_seconds":300,"max_pages":2,"max_deliveries":2}}),
    ));
    f.credential_path = readonly;
    assert!(
        !request(
            &f,
            &json!({"action":"recovery_context","epoch":1,"recovery":id,"after":0})
        )
        .status
        .success()
    );
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
        "host.recover",
    ]);
    let bridge = f.dir.path().join("recovery-bridge.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.recover",
        "--output",
        bridge.to_str().unwrap(),
    ]);
    f.credential_path = bridge.clone();
    let state = f.dir.path().join("recovery-state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let app = f.dir.path().join("absent-app.json");
    // Binding the journal before the missing config fails makes restart identity
    // use the actual CLI serialization, not a hand-built copy of its intent.
    assert!(!cli(&f, id, &state, &app).output().unwrap().status.success());
    assert!(state.join("intent.json").exists());
    let guid = Uuid::new_v4();
    let a = json!({"id":700,"guid":guid,"installation_id":9,"repository_id":11,"delivered_at_unix_ms":at-1000,"redelivery":false,"action":"checks_requested","status_code":503,"throttled_at_unix_ms":null});
    let mut b = a.clone();
    b["id"] = json!(701);
    b["redelivery"] = json!(true);
    let page = json!({"version":1,"observed_at_unix_ms":time(&f).await,"cursor":null,"next_cursor":null,"scanned":2,"attempts":[a,b]});
    save(&state, 0, json!({"kind":"page","report":page}));
    kill_after_commit(&mut f, &mut server, id, &state, &app, "recovery_page").await;
    assert_eq!(context(&f, id)["page_count"], 1);
    assert!(!state.join("ack-0000.json").exists());
    assert!(!cli(&f, id, &state, &app).output().unwrap().status.success());
    assert!(state.join("ack-0000.json").exists());
    let v = events::event("checks");
    let payload: Value = serde_json::from_str(v["body"].as_str().unwrap()).unwrap();
    let digest = treeharbor_github_contract::deliveries::payload_digest(&payload).unwrap();
    let d = json!({"version":1,"attempt":a,"observed_at_unix_ms":time(&f).await,"payload_sha256":digest});
    save(&state, 1, json!({"kind":"details","details":d}));
    kill_after_commit(&mut f, &mut server, id, &state, &app, "recovery_details").await;
    assert_eq!(context(&f, id)["detail_count"], 1);
    assert!(!state.join("ack-0001.json").exists());
    let mut other = d.clone();
    other["attempt"] = b;
    result(request(
        &f,
        &json!({"action":"recovery_details","key":"second-detail","epoch":1,"recovery":id,"details":other}),
    ));
    let c = context(&f, id);
    let pem = f.dir.path().join("fixture-app.pem");
    files::write_private(
        &pem,
        include_bytes!("../../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    files::write_private(&app,json!({"version":1,"app_id":c["specification"]["app_id"],"client_id":c["specification"]["client_id"],"private_key_file":pem,"repository":c["specification"]["upstream"]}).to_string().as_bytes()).unwrap();
    // Stop before the grant reaches the worker: no provider network request is made.
    kill_after_commit(&mut f, &mut server, id, &state, &app, "redelivery_begin").await;
    assert!(state.join("ack-0001.json").exists());
    let dispatch: Value =
        serde_json::from_slice(&fs::read(state.join("dispatch-700.json")).unwrap()).unwrap();
    assert!(!state.join("dispatch-701.json").exists());
    let begin = json!({"action":"redelivery_begin","key":format!("github-redelivery-{id}-700-{}",dispatch["dispatch"].as_str().unwrap()),"epoch":1,"recovery":id,"provider_id":700,"dispatch":dispatch["dispatch"]});
    assert_eq!(result(request(&f, &begin))["dispatch_granted"], false);
    fs::remove_file(&app).unwrap();
    fs::remove_file(&pem).unwrap();
    let receipt = json!({"version":1,"observed_at_unix_ms":time(&f).await,"outcome":"unknown"});
    save(
        &state,
        2,
        json!({"kind":"report","provider_id":700,"dispatch":dispatch["dispatch"],"report":receipt}),
    );
    kill_after_commit(&mut f, &mut server, id, &state, &app, "redelivery_report").await;
    let out = cli(&f, id, &state, &app).output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let waiting: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(waiting["observed_deliveries"], 2);
    assert_eq!(waiting["observed_received"], 0);
    assert_eq!(waiting["dispatch_count"], 1);
    assert_eq!(waiting["deliveries"][0]["report"], receipt);
    assert_eq!(
        waiting["deliveries"][1]["related_dispatch"],
        dispatch["dispatch"]
    );
    assert_eq!(waiting["coverage_complete"], false);
    assert_eq!(waiting["merge_verified"], false);
    assert!(state.join("ack-0002.json").exists());
    // The actual signed receiver, rather than a worker boolean, supplies receipt.
    let socket = f.dir.path().join("recovery-webhook.sock");
    let config = f.dir.path().join("recovery-webhook.json");
    let secret = f.dir.path().join("recovery-secret");
    files::write_private(
        &secret,
        events::vectors()["secret"].as_str().unwrap().as_bytes(),
    )
    .unwrap();
    files::write_private(&config,json!({"database":{"database_url":f.config.database_url,"schema":f.config.schema,"socket":socket,"pool_size":1,"max_connections":4,"request_timeout_ms":10000},"app_id":7,"keys":[{"id":Uuid::new_v4(),"secret_file":secret}]}).to_string().as_bytes()).unwrap();
    let receiver = events::receiver(&f, &config).await;
    assert!(
        events::raw(&socket, &events::bytes(&v, guid, ""))
            .await
            .starts_with(b"HTTP/1.1 200 ")
    );
    let out = cli(&f, id, &state, &app).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let done: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(done["observed_received"], 2);
    assert_eq!(done["all_observed_received"], true);
    assert_eq!(done["dispatch_count"], 1);
    assert_eq!(done["coverage_complete"], false);
    assert_eq!(done["merge_verified"], false);
    assert_eq!(done["deliveries"][0]["inbox"]["payload_sha256"], digest);
    assert_eq!(
        done["deliveries"][0]["inbox"]["event"],
        done["deliveries"][1]["inbox"]["event"]
    );
    for kind in [
        "github.recovery.page",
        "github.recovery.dispatched",
        "github.recovery.reported",
    ] {
        assert_eq!(
            f.sql
                .query_one("SELECT count(*) FROM th_outbox WHERE kind=$1", &[&kind])
                .await
                .unwrap()
                .get::<_, i64>(0),
            1
        );
    }
    // Owner cancellation remains inspectable and does not discard late evidence.
    f.credential_path = owner;
    result(request(
        &f,
        &json!({"action":"recovery_cancel","key":"cancel","epoch":1,"recovery":id}),
    ));
    f.credential_path = bridge;
    assert!(cli(&f, id, &state, &app).output().unwrap().status.success());
    drop(receiver);
    drop(server);
    f.cleanup().await;
}
