use super::*;
fn vectors() -> Value {
    serde_json::from_str(include_str!(
        "../../../../github-webhook/tests/fixtures/vectors-v1.json"
    ))
    .unwrap()
}
fn event(name: &str) -> Value {
    vectors()["vectors"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["name"] == name)
        .unwrap()
        .clone()
}
fn bytes(v: &Value, id: Uuid, extra: &str) -> Vec<u8> {
    let body = v["body"].as_str().unwrap();
    format!("POST /github/events HTTP/1.1\r\nHost: callback.local\r\nContent-Type: application/json\r\nX-GitHub-Event: {}\r\nX-GitHub-Delivery: {id}\r\nX-Hub-Signature-256: {}\r\nContent-Length: {}\r\nConnection: close\r\n{extra}\r\n{body}",v["event_header"].as_str().unwrap(),v["signature"].as_str().unwrap(),body.len()).into_bytes()
}
async fn raw(socket: &std::path::Path, bytes: &[u8]) -> Vec<u8> {
    let mut stream = UnixStream::connect(socket).await.unwrap();
    stream.write_all(bytes).await.unwrap();
    let mut out = Vec::new();
    tokio::time::timeout(Duration::from_secs(15), stream.read_to_end(&mut out))
        .await
        .unwrap()
        .unwrap();
    out
}
async fn receiver(f: &Fixture, config: &std::path::Path) -> Process {
    let log = f.dir.path().join("webhook.log");
    let file = fs::File::create(&log).unwrap();
    let mut process = Process(
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .env("PATH", "/no-executables")
            .args(["webhook-serve", "--config"])
            .arg(config)
            .stdout(file.try_clone().unwrap())
            .stderr(file)
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = fs::read_to_string(&log).unwrap();
        assert!(process.0.try_wait().unwrap().is_none(), "{text}");
        if text.contains("\"status\":\"listening\"") {
            return process;
        }
        assert!(Instant::now() < deadline, "{text}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
async fn count(f: &Fixture) -> i64 {
    f.sql
        .query_one("SELECT count(*) FROM th_github_events", &[])
        .await
        .unwrap()
        .get(0)
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL: actual signed webhook process, kill before/after commit, key rotation and replay"]
async fn github_webhook_receiver_survives_process_loss_and_keeps_one_durable_signed_fact() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &bind));
    let probe = Uuid::new_v4();
    f.credential_path = bridge;
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
    let channel = Uuid::new_v4();
    let create = json!({"action":"event_channel_create","key":"channel","epoch":1,"channel":channel,"specification":{"binding":bind["binding"],"max_events":8,"max_bytes":8192,"max_aliases":2}});
    assert!(!request(&f, &create).status.success());
    f.credential_path = owner;
    result(request(&f, &create));
    let socket = f.dir.path().join("webhook.sock");
    let config = f.dir.path().join("webhook.json");
    let secret = f.dir.path().join("webhook-secret");
    files::write_private(&secret, vectors()["secret"].as_str().unwrap().as_bytes()).unwrap();
    let key_id = Uuid::new_v4();
    let config_data = json!({"database":{"database_url":f.config.database_url,"schema":f.config.schema,"socket":socket,"pool_size":1,"max_connections":4,"request_timeout_ms":10000},"app_id":7,"keys":[{"id":key_id,"secret_file":secret}]});
    files::write_private(&config, config_data.to_string().as_bytes()).unwrap();
    let recv = receiver(&f, &config).await;
    assert!(
        raw(
            &socket,
            b"GET /health HTTP/1.1\r\nHost: callback.local\r\nConnection: close\r\n\r\n"
        )
        .await
        .starts_with(b"HTTP/1.1 200 ")
    );
    let v = event("checks");
    let id = Uuid::new_v4();
    let body = bytes(&v, id, "");
    let mut invalid = v.clone();
    invalid["body"] = json!(format!("{} ", v["body"].as_str().unwrap()));
    for (request, status) in [
        (bytes(&invalid, id, ""), 401),
        (
            bytes(
                &v,
                id,
                "X-GitHub-Delivery: 11111111-1111-1111-1111-111111111111\r\n",
            ),
            400,
        ),
        (bytes(&event("header-substitution"), id, ""), 422),
        (bytes(&event("ping"), id, ""), 200),
        (bytes(&v, id, "Content-Encoding: gzip\r\n"), 415),
    ] {
        let reply = raw(&socket, &request).await;
        assert!(
            reply.starts_with(format!("HTTP/1.1 {status} ").as_bytes()),
            "{}",
            String::from_utf8_lossy(&reply)
        );
    }
    assert_eq!(count(&f).await, 0);
    // Hold the repository lock and observe the actual receiver backend blocked
    // before killing it. No test-only pause or synthetic success is used.
    f.sql
        .batch_execute("BEGIN; LOCK TABLE th_repositories IN ACCESS EXCLUSIVE MODE")
        .await
        .unwrap();
    let mut pending = UnixStream::connect(&socket).await.unwrap();
    pending.write_all(&body).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let blocked:i64=f.sql.query_one("SELECT count(*) FROM pg_stat_activity WHERE pg_backend_pid()=ANY(pg_blocking_pids(pid))",&[]).await.unwrap().get(0);
        if blocked > 0 {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(recv);
    drop(pending);
    f.sql.batch_execute("COMMIT").await.unwrap();
    assert_eq!(count(&f).await, 0);
    let recv = receiver(&f, &config).await;
    // Deliver again, observe committed state from another connection, then kill
    // without reading its acknowledgement. Recovery must deduplicate that fact.
    let mut lost = UnixStream::connect(&socket).await.unwrap();
    lost.write_all(&body).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while count(&f).await != 1 {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(recv);
    drop(lost);
    let recv = receiver(&f, &config).await;
    assert!(raw(&socket, &body).await.starts_with(b"HTTP/1.1 200 "));
    assert!(
        raw(&socket, &bytes(&v, Uuid::new_v4(), ""))
            .await
            .starts_with(b"HTTP/1.1 200 ")
    );
    assert!(
        raw(&socket, &bytes(&v, Uuid::new_v4(), ""))
            .await
            .starts_with(b"HTTP/1.1 503 ")
    );
    assert!(
        raw(&socket, &bytes(&event("destroyed-merged"), id, ""))
            .await
            .starts_with(b"HTTP/1.1 409 ")
    );
    assert_eq!(count(&f).await, 1);
    let record = f
        .sql
        .query_one("SELECT body,key_id FROM th_github_events", &[])
        .await
        .unwrap();
    assert_eq!(
        record.get::<_, Vec<u8>>(0),
        v["body"].as_str().unwrap().as_bytes()
    );
    assert_eq!(record.get::<_, Uuid>(1), key_id);
    let page = result(request(
        &f,
        &json!({"action":"event_page","epoch":1,"channel":channel,"after":0}),
    ));
    assert_eq!(page["merge_verified"], false);
    assert_eq!(page["events"].as_array().unwrap().len(), 1);
    // Rotate to a new configured key. The old authentic signature is no longer
    // accepted; retaining both keys is an explicit bounded overlap configuration.
    drop(recv);
    fs::write(&secret, b"a-new-public-test-secret-never-a-real-credential").unwrap();
    let recv = receiver(&f, &config).await;
    assert!(raw(&socket, &body).await.starts_with(b"HTTP/1.1 401 "));
    drop(recv);
    fs::write(&secret, vectors()["secret"].as_str().unwrap().as_bytes()).unwrap();
    let recv = receiver(&f, &config).await;
    result(request(
        &f,
        &json!({"action":"event_channel_revoke","key":"revoke","epoch":1,"channel":channel}),
    ));
    assert!(raw(&socket, &body).await.starts_with(b"HTTP/1.1 409 "));
    let n: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_outbox WHERE kind='github.events.received'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    let log = fs::read_to_string(f.dir.path().join("webhook.log")).unwrap();
    assert!(!log.contains(vectors()["secret"].as_str().unwrap()));
    assert!(!log.contains("Проверка"));
    drop(recv);
    drop(server);
    f.cleanup().await;
}
