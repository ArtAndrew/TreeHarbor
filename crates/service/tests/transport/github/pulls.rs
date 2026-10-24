use super::*;
#[path = "inspections.rs"]
mod inspections;

fn cli(f: &Fixture, id: Uuid, state: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/no-executables")
        .args(["github", "pull", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--pull", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(config);
    command
}
pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    publisher: &Path,
    publication: Uuid,
) {
    let id = Uuid::new_v4();
    f.credential_path = owner.to_owned();
    let specification = json!({"publication":publication.to_string(),"base":"main","title":"Agent PR","body":"Selected result","draft":true,"previous":null,"ttl_seconds":600});
    let create = json!({"action":"pull_request","key":"pull-create","epoch":1,"pull":id.to_string(),"specification":specification});
    let created = result(request(f, &create));
    assert_eq!(result(request(f, &create)), created);
    assert_eq!(created["state"], "requested");
    assert_eq!(created["merge_authorized"], false);
    assert!(!request(f,&json!({"action":"pull_begin","key":"owner-begin","epoch":1,"pull":id.to_string(),"attempt":Uuid::new_v4().to_string()})).status.success());
    f.credential_path = publisher.to_owned();
    let context = result(request(
        f,
        &json!({"action":"pull_context","epoch":1,"pull":id.to_string()}),
    ));
    let state = f.dir.path().join("pull-driver");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let pem = f.dir.path().join("pull-fixture.pem");
    let app = f.dir.path().join("pull-fixture.json");
    files::write_private(
        &pem,
        include_bytes!("../../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    files::write_private(&app,json!({"version":1,"app_id":context["specification"]["app_id"],"client_id":context["specification"]["client_id"],"private_key_file":pem,"repository":context["specification"]["upstream"]}).to_string().as_bytes()).unwrap();
    // The real CLI reaches its committed one-shot grant through a private proxy.
    // Never deliver the grant: no external host request or real App is involved.
    let socket = f.dir.path().join("pull-proxy.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600)).unwrap();
    let upstream = f.config.socket.clone();
    let (signal, received) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let mut signal = Some(signal);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut incoming, _) = listener.accept().await.unwrap();
            let bytes = message(&mut incoming).await;
            let hold = String::from_utf8_lossy(&bytes).contains("\"action\":\"pull_begin\"");
            let signal = if hold { signal.take() } else { None };
            let upstream = upstream.clone();
            clients.spawn(async move {
                let mut outgoing = UnixStream::connect(upstream).await.unwrap();
                outgoing.write_all(&bytes).await.unwrap();
                if let Some(signal) = signal {
                    let response = message(&mut outgoing).await;
                    assert!(
                        String::from_utf8_lossy(&response).contains("\"dispatch_granted\":true")
                    );
                    signal.send(()).unwrap();
                    std::future::pending::<()>().await;
                } else {
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                }
            });
        }
    });
    let original = f.config.socket.clone();
    f.config.socket = socket;
    let diagnostic = f.dir.path().join("pull-driver.stderr");
    let mut child = Process(
        cli(f, id, &state, &app)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let signal = tokio::time::timeout(Duration::from_secs(30), received).await;
    assert!(
        signal.is_ok(),
        "CLI exit {:?}: {}",
        child.0.try_wait().unwrap(),
        fs::read_to_string(&diagnostic).unwrap()
    );
    signal.unwrap().unwrap();
    f.config.socket = original;
    let duplicate = cli(f, id, &state, &app).output().unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("lock"));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    proxy.abort();
    let _ = proxy.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let context = result(request(
        f,
        &json!({"action":"pull_context","epoch":1,"pull":id.to_string()}),
    ));
    assert_eq!(context["state"], "attempting");
    let attempt: Value =
        serde_json::from_slice(&fs::read(state.join("attempt.json")).unwrap()).unwrap();
    assert_eq!(context["attempt"], attempt["attempt"]);
    let begin = json!({"action":"pull_begin","key":format!("github-pull-begin-{id}"),"epoch":1,"pull":id.to_string(),"attempt":attempt["attempt"]});
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
    let branch: Uuid = context["target"]["branch_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let snapshot = json!({"identity":{"number":17,"node_id":"PR_test_17"},"head_repository":context["specification"]["upstream"]["repository_id"],"base_repository":context["specification"]["upstream"]["repository_id"],"head_ref":format!("treeharbor/{}",branch.simple()),"base_ref":"main","head_revision":context["target"]["git_revision"],"base_revision":context["target"]["git_revision"],"title":context["target"]["title"],"body":context["target"]["body"],"draft":true,"state":"open","merged":false});
    let report = json!({"attempt":context["attempt"],"binding":context["binding"],"git_revision":context["target"]["git_revision"],"pull":snapshot,"observed_at_unix_ms":now,"token_revoked":false});
    // Explicit trusted host fixture after a lost grant. This tests journal/server
    // recovery, not a real GitHub observation or proof that a PR was created.
    files::write_private(&state.join("report-0000.json"),json!({"version":1,"key":"github-publish-report-pull-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    fs::remove_file(&app).unwrap();
    fs::remove_file(&pem).unwrap();
    // A wrong scope is rejected before loading a pending report or App file.
    let other = f.dir.path().join("wrong-pull-state");
    fs::create_dir(&other).unwrap();
    fs::set_permissions(&other, fs::Permissions::from_mode(0o700)).unwrap();
    let mut wrong: Value =
        serde_json::from_slice(&fs::read(state.join("intent.json")).unwrap()).unwrap();
    wrong["tenant"] = json!("foreign");
    files::write_private(&other.join("intent.json"), wrong.to_string().as_bytes()).unwrap();
    assert!(!cli(f, id, &other, &app).output().unwrap().status.success());
    let recovered = cli(f, id, &state, &app).output().unwrap();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let observed: Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(observed["state"], "observed");
    assert_eq!(observed["merge_authorized"], false);
    assert_eq!(observed["observation"]["token_revoked"], false);
    assert!(state.join("ack-0000.json").is_file());
    inspections::exercise(f, server, owner, publisher, id).await;
    f.credential_path = publisher.to_owned();
    assert!(
        cli(f, id, &f.dir.path().join("missing-state"), &app)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.pull.attempting'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    f.credential_path = owner.to_owned();
    let update = Uuid::new_v4();
    let mut spec = specification;
    spec["previous"] = json!(id.to_string());
    spec["title"] = json!("New title");
    let next = result(request(
        f,
        &json!({"action":"pull_request","key":"pull-update","epoch":1,"pull":update.to_string(),"specification":spec}),
    ));
    assert_eq!(next["target"]["previous"]["identity"], snapshot["identity"]);
    result(request(
        f,
        &json!({"action":"pull_cancel","key":"cancel-update","epoch":1,"pull":update.to_string()}),
    ));
    f.credential_path = publisher.to_owned();
    let cancelled = cli(f, update, &f.dir.path().join("not-created"), &app)
        .output()
        .unwrap();
    assert!(!cancelled.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&cancelled.stdout).unwrap()["state"],
        "cancelled"
    );
    f.credential_path = owner.to_owned();
}
