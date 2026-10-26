use super::*;
fn host_policy_cli(f: &Fixture, id: Uuid, state: &Path, app: &Path, sequence: u64) -> Command {
    let mut c = cli(f, id, state, app);
    c.args(["--host-policy-group", &sequence.to_string()]);
    c
}
// Provider data is labelled fixture data; actual processes and database recovery
// must retain the precise source/candidate/policy after the commit reply is lost.
pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    id: Uuid,
    state: &Path,
    app: &Path,
    owner: &Path,
) {
    for version in [1u8, 2] {
        exercise_version(f, server, id, state, app, owner, version).await;
    }
}
async fn exercise_version(
    f: &mut Fixture,
    server: &mut Process,
    id: Uuid,
    state: &Path,
    app: &Path,
    owner: &Path,
    version: u8,
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
        &json!({"action":"queue_host_policy_context","epoch":1,"queue":id.to_string(),"membership_sequence":membership}),
    ));
    let fixture: Value = serde_json::from_str(if version == 1 {
        include_str!("../../../../github-contract/tests/fixtures/host-policy-v1.json")
    } else {
        include_str!("../../../../github-contract/tests/fixtures/host-policy-v2.json")
    })
    .unwrap();
    let mut policy = fixture["report"].clone();
    policy["target"] = selected["host_policy_target"].clone();
    policy["repository_name"] = json!("example-org/demo");
    for side in ["before", "after"] {
        let configuration = policy[side]["group"]["target"]["configuration"].clone();
        policy[side]["group"] = groups["captures"][0]["report"]["group_capture"]["before"].clone();
        policy[side]["review"]["pull"] = policy[side]["group"]["target"]["pull"].clone();
        if version == 2 {
            policy[side]["group"]["target"]["configuration"] = configuration;
            policy[side]["visibility"]["target"]["repository_name"] = json!("example-org/demo");
        }
        policy[side]["rules"][0]["ruleset_source"] = json!("example-org/demo");
        policy[side]["rulesets"][0]["source"]["name"] = json!("example-org/demo");
        policy[side]["rulesets"][0]["document"]["value"]["source"] = json!("example-org/demo");
    }
    let at: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let report = json!({"version":version+8,"started_at_unix_ms":at,"finished_at_unix_ms":at,
        "snapshot":null,"receipt":null,"async_result":null,"token_revoked":true,
        "host_policy":policy});
    let index = sequence as usize;
    files::write_private(&state.join(format!("report-{index:04}.json")),json!({"version":1,"key":format!("github-publish-report-host_policy-v{version}-fixture"),"report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let diagnostic = f.dir.path().join("queue-host-policy.stderr");
    let (original, hold, received) = proxy(
        f,
        &format!("queue-host-policy-v{version}-report.sock"),
        "\"action\":\"queue_report\"",
        "\"host_policy\"",
    )
    .await;
    let mut child = Process(
        host_policy_cli(f, id, state, app, membership)
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
    let output = host_policy_cli(f, id, state, app, membership)
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
    let wrong = host_policy_cli(f, id, state, app, sequence + 1)
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
        &json!({"action":"queue_host_policies","epoch":1,"queue":id.to_string(),"after":sequence}),
    ));
    assert_eq!(page["captures"].as_array().unwrap().len(), 1);
    assert_eq!(page["captures"][0]["report"], report);
    assert!(page["captures"][0]["capture_digest"].is_string());
    assert_eq!(page["captures"][0]["evaluation"]["version"], version);
    if version == 1 {
        assert!(
            page["captures"][0]["evaluation"]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("legacy/required_signatures/flag_unproven"))
        );
    }
    assert_eq!(page["host_policy_verified"], false);
    assert_eq!(page["admission_verified"], false);
    assert_eq!(page["merge_verified"], false);
    assert!(page["next_after"].is_null());
}
