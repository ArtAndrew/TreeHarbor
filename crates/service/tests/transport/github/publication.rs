use super::*;
fn publication_cli(f: &Fixture, id: Uuid, state: &Path, app: &Path, sequence: u64) -> Command {
    let mut c = cli(f, id, state, app);
    c.args(["--publication-group", &sequence.to_string()]);
    c
}
// Provider data is labelled fixture data; actual processes and database recovery
// must retain the precise source/candidate/ancestry after the commit reply is lost.
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
    f.credential_path = owner.to_owned();
    let groups = result(request(
        f,
        &json!({"action":"queue_groups","epoch":1,"queue":id.to_string(),"after":0}),
    ));
    let membership = groups["captures"][0]["sequence"].as_u64().unwrap();
    f.credential_path = f.dir.path().join("queue-bridge.json");
    let selected = result(request(
        f,
        &json!({"action":"queue_publication_context","epoch":1,"queue":id.to_string(),"membership_sequence":membership}),
    ));
    let mut report: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/publication-v8.json"
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
    report["publication"]["target"] = selected["publication_target"].clone();
    let member = &selected["publication_target"]["member"];
    let mut pull =
        groups["captures"][0]["report"]["group_capture"]["before"]["target"]["pull"].clone();
    pull["merged"] = json!(true);
    pull["state"] = json!("closed");
    for side in ["before", "after"] {
        report["publication"][side] = json!({"pull":pull,"main":member["candidate"]["commit"],"final_commit":member["candidate"]});
    }
    report["publication"]["comparison"]["base"] = member["candidate"]["commit"].clone();
    report["publication"]["comparison"]["merge_base"] = member["candidate"]["commit"].clone();
    report["publication"]["comparison"]["head_revision"] =
        member["candidate"]["commit"]["revision"].clone();
    let index = sequence as usize;
    files::write_private(&state.join(format!("report-{index:04}.json")),json!({"version":1,"key":"github-publish-report-publication-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let diagnostic = f.dir.path().join("queue-publication.stderr");
    let (original, hold, received) = proxy(
        f,
        "queue-publication-report.sock",
        "\"action\":\"queue_report\"",
        "\"publication\"",
    )
    .await;
    let mut child = Process(
        publication_cli(f, id, state, app, membership)
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
    let output = publication_cli(f, id, state, app, membership)
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
    let wrong = publication_cli(f, id, state, app, sequence + 1)
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
        &json!({"action":"queue_publications","epoch":1,"queue":id.to_string(),"after":0}),
    ));
    assert_eq!(page["captures"].as_array().unwrap().len(), 1);
    assert_eq!(page["captures"][0]["report"], report);
    assert_eq!(page["captures"][0]["exact_candidate_published"], true);
    assert_eq!(page["captures"][0]["checks_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert!(page["next_after"].is_null());
}
