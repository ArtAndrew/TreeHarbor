use super::*;

fn cli(f: &Fixture, id: Uuid, state: &Path, app: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args(["github", "dequeue", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--dequeue", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(app);
    c
}
fn status(f: &Fixture, id: Uuid) -> Value {
    result(request(
        f,
        &json!({"action":"dequeue_context","dequeue":id.to_string(),"epoch":1}),
    ))
}
fn directory(f: &Fixture) -> PathBuf {
    f.dir.path().join("dequeue-driver")
}
pub(super) async fn request_while_unresolved(
    f: &mut Fixture,
    owner: &Path,
    queue: Uuid,
    app: &Path,
) -> Uuid {
    let bridge = f.credential_path.clone();
    f.credential_path = owner.to_owned();
    let id = Uuid::new_v4();
    let v = result(request(
        f,
        &json!({"action":"dequeue_request","key":"dequeue-create","epoch":1,"dequeue":id.to_string(),"specification":{"queue":queue.to_string(),"ttl_seconds":600,"scope":"pull_request"}}),
    ));
    assert_eq!(v["enqueue_settled"], false);
    f.credential_path = bridge;
    let state = directory(f);
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    // No result UUID is available yet: no grant or provider access is attempted.
    let out = cli(f, id, &state, app).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["state"], "enqueue_unresolved");
    assert!(v["attempt"].is_null());
    assert!(!state.join("attempt.json").exists());
    id
}
async fn kill_at(
    f: &mut Fixture,
    server: &mut Process,
    id: Uuid,
    app: &Path,
    action: &'static str,
    expected: &'static str,
    name: &str,
) {
    let (original, hold, received) = proxy(f, name, action, expected).await;
    let diagnostic = f.dir.path().join("dequeue-cli.stderr");
    let mut child = Process(
        cli(f, id, &directory(f), app)
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
}
pub(super) async fn exercise(f: &mut Fixture, server: &mut Process, id: Uuid, app: &Path) {
    let state = directory(f);
    let c = status(f, id);
    assert_eq!(c["enqueue_settled"], true);
    // Recreate only the fixture App config to reach the committed begin boundary.
    let pem = f.dir.path().join("dequeue-fixture.pem");
    files::write_private(
        &pem,
        include_bytes!("../../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    files::write_private(app,json!({"version":1,"app_id":c["specification"]["app_id"],"client_id":c["specification"]["client_id"],"private_key_file":pem,"repository":c["specification"]["upstream"]}).to_string().as_bytes()).unwrap();
    kill_at(
        f,
        server,
        id,
        app,
        "\"action\":\"dequeue_begin\"",
        "\"dispatch_granted\":true",
        "dequeue-begin.sock",
    )
    .await;
    let c = status(f, id);
    let attempt: Value =
        serde_json::from_slice(&fs::read(state.join("attempt.json")).unwrap()).unwrap();
    assert_eq!(c["attempt"], attempt["attempt"]);
    let begin = json!({"action":"dequeue_begin","key":format!("github-dequeue-begin-{id}"),"epoch":1,"dequeue":id.to_string(),"attempt":attempt["attempt"]});
    assert_eq!(result(request(f, &begin))["dispatch_granted"], false);
    let at: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    // Explicit synthetic provider receipt; this is not installed-App acceptance.
    let target = &c["target"];
    let pull = json!({"repository_id":target["pull"]["repository_id"],"number":target["pull"]["pull_number"],"node_id":target["pull"]["pull_node"],"head_ref":target["pull"]["head_ref"],"base_ref":target["pull"]["base_ref"],"head_revision":target["pull"]["head_revision"],"base_revision":target["expected_base_revision"],"state":"open","draft":false,"merged":false});
    let report = json!({"version":1,"started_at_unix_ms":at,"finished_at_unix_ms":at,"before":null,"receipt":null,"after":{"pull":pull,"queue_enabled":true,"configuration":null,"entry":null,"final_commit":null},"mutation_not_sent":false,"token_revoked":false});
    files::write_private(&state.join("report-0000.json"),json!({"version":1,"key":"github-publish-report-dequeue-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    fs::remove_file(app).unwrap();
    fs::remove_file(&pem).unwrap();
    kill_at(
        f,
        server,
        id,
        app,
        "\"action\":\"dequeue_report\"",
        "\"state\":\"host_merged_unverified\"",
        "dequeue-report.sock",
    )
    .await;
    let out = cli(f, id, &state, app).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["observation"], report);
    assert_eq!(v["state"], "host_merged_unverified");
    assert_eq!(v["merge_verified"], false);
    assert_eq!(v["report_sequence"], 1);
    assert!(state.join("ack-0000.json").is_file());
    assert_eq!(result(request(f, &begin))["dispatch_granted"], false);
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.dequeue.observed'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let mut late = report.clone();
    late["started_at_unix_ms"] = json!(at - 1000);
    late["finished_at_unix_ms"] = json!(at - 1000);
    late["before"] = late["after"].take();
    late["before"]["configuration"] = json!({"id":"MQ_one","merge_method":"SQUASH","merging_strategy":"ALLGREEN","maximum_entries_to_build":5,"maximum_entries_to_merge":5,"minimum_entries_to_merge":1,"minimum_entries_wait_minutes":0,"check_timeout_minutes":60});
    late["before"]["entry"] = json!({"id":"MQE_one","state":"QUEUED","base":null,"head":null});
    late["receipt"] = json!({"request_id":id.to_string(),"repository_id":target["pull"]["repository_id"],"pull_node":target["pull"]["pull_node"],"pull_number":target["pull"]["pull_number"],"queue_id":"MQ_one","entry_id":"MQE_one","head_revision":target["pull"]["head_revision"]});
    files::write_private(&state.join("report-0001.json"),json!({"version":1,"key":"github-publish-report-dequeue-late","report":late,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let out = cli(f, id, &state, app).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["observation"], report);
    assert_eq!(v["retained_receipt"], late["receipt"]);
    assert_eq!(v["report_sequence"], 2);
    assert!(state.join("ack-0001.json").is_file());
}
