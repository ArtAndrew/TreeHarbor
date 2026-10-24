use super::*;
fn attempt_cli(f: &Fixture, id: Uuid, state: &Path, app: &Path, sequence: u64) -> Command {
    let mut c = cli(f, id, state, app);
    c.args([
        "--check-report",
        &sequence.to_string(),
        "--check-suite",
        "CS_merge_group",
        "--check-run",
        "CR_merge_group",
    ]);
    c
}
// Provider data is labelled fixture data; actual processes and database recovery
// must retain the precise source/check/attempt after the commit reply is lost.
pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    id: Uuid,
    state: &Path,
    app: &Path,
    owner: &Path,
) {
    f.credential_path = f.dir.path().join("queue-bridge.json");
    let context = result(request(
        f,
        &json!({"action":"queue_context","epoch":1,"queue":id.to_string()}),
    ));
    let sequence = context["report_sequence"].as_u64().unwrap();
    let selection = json!({"check_sequence":sequence,"side":"after","suite_id":"CS_merge_group","check_id":"CR_merge_group"});
    let selected = result(request(
        f,
        &json!({"action":"queue_attempt_context","epoch":1,"queue":id.to_string(),"selection":selection}),
    ));
    let mut report: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/group-attempt-v6.json"
    ))
    .unwrap();
    let at: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    report["started_at_unix_ms"] = json!(at);
    report["finished_at_unix_ms"] = json!(at);
    report["group_attempt"]["target"] = selected["group_attempt_target"].clone();
    let revision = selected["group_attempt_target"]["subject"]["revision"].clone();
    for side in ["before", "after"] {
        let view = &mut report["group_attempt"][side];
        view["selected"]["head_revision"] = revision.clone();
        view["current"]["head_revision"] = revision.clone();
        view["check"]["head_revision"] = revision.clone();
        for j in view["jobs"].as_array_mut().unwrap() {
            j["head_revision"] = revision.clone();
        }
    }
    let index = sequence as usize;
    files::write_private(&state.join(format!("report-{index:04}.json")),json!({"version":1,"key":"github-publish-report-attempt-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let diagnostic = f.dir.path().join("queue-attempt.stderr");
    let (original, hold, received) = proxy(
        f,
        "queue-attempt-report.sock",
        "\"action\":\"queue_report\"",
        "\"group_attempt\"",
    )
    .await;
    let mut child = Process(
        attempt_cli(f, id, state, app, sequence)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let got = tokio::time::timeout(Duration::from_secs(30), received).await;
    assert!(got.is_ok(), "{}", fs::read_to_string(&diagnostic).unwrap());
    got.unwrap().unwrap();
    f.config.socket = original;
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    hold.abort();
    let _ = hold.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    assert!(!app.exists());
    let output = attempt_cli(f, id, state, app, sequence).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(recovered["report_sequence"], sequence + 1);
    assert_eq!(recovered["observation"], context["observation"]);
    assert_eq!(recovered["merge_verified"], false);
    let ack: Value =
        serde_json::from_slice(&fs::read(state.join(format!("ack-{index:04}.json"))).unwrap())
            .unwrap();
    assert_eq!(ack["recorded_observation"], report);
    let wrong = attempt_cli(f, id, state, app, sequence + 1)
        .output()
        .unwrap();
    assert!(!wrong.status.success());
    let unchanged = result(request(
        f,
        &json!({"action":"queue_context","epoch":1,"queue":id.to_string()}),
    ));
    assert_eq!(unchanged["report_sequence"], sequence + 1);
    f.credential_path = owner.to_owned();
    let page = result(request(
        f,
        &json!({"action":"queue_attempts","epoch":1,"queue":id.to_string(),"after":0}),
    ));
    assert_eq!(page["captures"].as_array().unwrap().len(), 1);
    assert_eq!(page["captures"][0]["report"], report);
    assert_eq!(page["captures"][0]["attempt_linked"], true);
    assert_eq!(page["captures"][0]["current_attempt_matches"], true);
    assert_eq!(page["captures"][0]["definition_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert!(page["next_after"].is_null());
}
