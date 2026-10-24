use super::*;
// Provider content is an explicit fixture. The real CLI, journal, HTTP server,
// PostgreSQL commit and killed-process replay are exercised here.
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
    let t = &context["target"]["pull"];
    let mut r: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/queue-groups-v4.json"
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
    r["started_at_unix_ms"] = json!(at);
    r["finished_at_unix_ms"] = json!(at);
    for side in ["before", "after"] {
        let g = &mut r["group_capture"][side];
        let p = &mut g["target"]["pull"];
        p["node_id"] = t["pull_node"].clone();
        p["number"] = t["pull_number"].clone();
        p["head_ref"] = t["head_ref"].clone();
        p["head_revision"] = t["head_revision"].clone();
        let m = &mut g["members"][0];
        m["pull_node"] = t["pull_node"].clone();
        m["pull_number"] = t["pull_number"].clone();
        m["source_revision"] = t["head_revision"].clone();
        m["candidate"]["parents"][1]["revision"] = t["head_revision"].clone();
    }
    r["snapshot"] = r["group_capture"]["after"]["target"].clone();
    let index = sequence as usize;
    files::write_private(&state.join(format!("report-{index:04}.json")),json!({"version":1,"key":"github-publish-report-group-fixture","report":r,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let diagnostic = f.dir.path().join("queue-group.stderr");
    let (original, hold, received) = proxy(
        f,
        "queue-group-report.sock",
        "\"action\":\"queue_report\"",
        "\"group_capture\"",
    )
    .await;
    let mut child = Process(
        cli(f, id, state, app)
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
    let output = cli(f, id, state, app).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let recovered: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(recovered["report_sequence"], sequence + 1);
    assert_eq!(recovered["merge_verified"], false);
    assert!(state.join(format!("ack-{index:04}.json")).is_file());
    f.credential_path = owner.to_owned();
    let page = result(request(
        f,
        &json!({"action":"queue_groups","epoch":1,"queue":id.to_string(),"after":0}),
    ));
    assert_eq!(page["captures"][0]["report"], r);
    assert!(page["next_after"].is_null());
    assert_eq!(page["captures"][0]["consistent"], true);
    assert_eq!(
        page["captures"][0]["after_signed_events"]["events"],
        json!([])
    );
    assert_eq!(page["merge_verified"], false);
}
