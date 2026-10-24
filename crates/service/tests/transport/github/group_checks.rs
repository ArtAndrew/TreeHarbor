use super::*;

// Provider checks are explicit fixtures; journal replay uses actual CLI/server
// processes and a PostgreSQL commit whose reply is lost before both are killed.
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
    let selected = result(request(
        f,
        &json!({"action":"queue_check_context","epoch":1,"queue":id.to_string(),"membership_sequence":sequence,"side":"after"}),
    ));
    let mut report: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/group-checks-v5.json"
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
    report["group_checks"]["target"] = selected["group_check_target"].clone();
    report["group_checks"]["checks"]["subject"] = selected["group_check_target"]["subject"].clone();
    for side in ["before", "after"] {
        for suite in report["group_checks"]["checks"][side]["suites"]
            .as_array_mut()
            .unwrap()
        {
            suite["head_revision"] = selected["group_check_target"]["subject"]["revision"].clone();
        }
    }
    let index = sequence as usize;
    files::write_private(&state.join(format!("report-{index:04}.json")), json!({"version":1,"key":"github-publish-report-check-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let diagnostic = f.dir.path().join("queue-check.stderr");
    let (original, hold, received) = proxy(
        f,
        "queue-check-report.sock",
        "\"action\":\"queue_report\"",
        "\"group_checks\"",
    )
    .await;
    let mut child = Process(
        cli(f, id, state, app)
            .args(["--group-report", &sequence.to_string()])
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
    // The App config was removed earlier; recovery cannot depend on GitHub.
    assert!(!app.exists());
    let output = cli(f, id, state, app)
        .args([
            "--group-report",
            &sequence.to_string(),
            "--group-side",
            "after",
        ])
        .output()
        .unwrap();
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
    // A check report cannot be selected as a membership report, even after restart.
    let invalid = cli(f, id, state, app)
        .args(["--group-report", &(sequence + 1).to_string()])
        .output()
        .unwrap();
    assert!(!invalid.status.success());
    let unchanged = result(request(
        f,
        &json!({"action":"queue_context","epoch":1,"queue":id.to_string()}),
    ));
    assert_eq!(unchanged["report_sequence"], sequence + 1);
    f.credential_path = owner.to_owned();
    let page = result(request(
        f,
        &json!({"action":"queue_checks","epoch":1,"queue":id.to_string(),"after":0}),
    ));
    assert_eq!(page["captures"].as_array().unwrap().len(), 1);
    assert_eq!(page["captures"][0]["report"], report);
    assert_eq!(page["captures"][0]["checks_consistent"], true);
    assert_eq!(page["captures"][0]["attempt_verified"], false);
    assert_eq!(page["captures"][0]["definition_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert!(page["next_after"].is_null());
}
