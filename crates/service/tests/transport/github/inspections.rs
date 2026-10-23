use super::*;
fn cli(f: &Fixture, id: Uuid, state: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args(["github", "inspect", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--inspection", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(f.dir.path().join("no-app.json"));
    c
}
pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    publisher: &Path,
    pull: Uuid,
) {
    let id = Uuid::new_v4();
    f.credential_path = owner.to_owned();
    let create = json!({"action":"inspection_request","key":"inspect-create","epoch":1,"inspection":id.to_string(),"specification":{"pull":pull.to_string(),"required":[{"name":"CI / build","app_id":7}],"ttl_seconds":600}});
    let requested = result(request(f, &create));
    assert_eq!(result(request(f, &create)), requested);
    f.credential_path = publisher.to_owned();
    assert!(
        !request(
            f,
            &json!({"action":"inspection_context","epoch":1,"inspection":id.to_string()})
        )
        .status
        .success()
    );
    let reader = f.dir.path().join("inspection-reader.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.read",
        "--output",
        reader.to_str().unwrap(),
    ]);
    f.credential_path = reader;
    let context = result(request(
        f,
        &json!({"action":"inspection_context","epoch":1,"inspection":id.to_string()}),
    ));
    let state = f.dir.path().join("inspection-state");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    // With no App configuration, the actual command still binds its private
    // immutable intent; no provider call is possible in this fixture.
    assert!(!cli(f, id, &state).output().unwrap().status.success());
    let intent: Value =
        serde_json::from_slice(&fs::read(state.join("intent.json")).unwrap()).unwrap();
    assert_eq!(intent["target"], context["target"]);
    assert_eq!(intent["inspection"], id.to_string());
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/inspection-v1.json"
    )))
    .unwrap();
    let mut report = fixture["report"].clone();
    for name in ["before", "after"] {
        for (field, target) in [
            ("repository_id", "repository_id"),
            ("number", "pull_number"),
            ("node_id", "pull_node"),
            ("head_ref", "head_ref"),
            ("base_ref", "base_ref"),
            ("head_revision", "head_revision"),
        ] {
            report[name][field] = context["target"][target].clone();
        }
    }
    report["checks"][0]["runs"][0]["head_revision"] = context["target"]["head_revision"].clone();
    report["token_revoked"] = json!(false);
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    report["started_at_unix_ms"] = json!(now);
    report["finished_at_unix_ms"] = json!(now);
    // A trusted captured provider report, explicitly not a live App result.
    files::write_private(&state.join("report-0000.json"),json!({"version":1,"key":"github-publish-report-inspection-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION hold_inspection() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.inspection.report.v1' THEN PERFORM pg_advisory_xact_lock({lock}::bigint); END IF; RETURN NEW; END $$; CREATE TRIGGER hold_inspection AFTER INSERT ON th_operations FOR EACH ROW EXECUTE FUNCTION hold_inspection();")).await.unwrap();
    let mut child = Process(
        cli(f, id, &state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock&0xffff_ffff)]).await.unwrap().get::<_,bool>(0){break;}
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    f.sql
        .query_one("SELECT pg_advisory_unlock($1)", &[&lock])
        .await
        .unwrap();
    f.sql
        .batch_execute(
            "DROP TRIGGER hold_inspection ON th_operations; DROP FUNCTION hold_inspection()",
        )
        .await
        .unwrap();
    *server = f.start().await;
    assert_eq!(
        f.sql
            .query_one(
                "SELECT state FROM th_github_inspections WHERE inspection=$1",
                &[&id]
            )
            .await
            .unwrap()
            .get::<_, String>(0),
        "requested"
    );
    assert!(!state.join("ack-0000.json").exists());
    // The next attempt commits, but a private proxy drops its receipt. Retrying
    // must recover that immutable report rather than read GitHub again.
    let socket = f.dir.path().join("inspection-proxy.sock");
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
            let hold = String::from_utf8_lossy(&bytes).contains("\"action\":\"inspection_report\"");
            let signal = if hold { signal.take() } else { None };
            let upstream = upstream.clone();
            clients.spawn(async move {
                let mut outgoing = UnixStream::connect(upstream).await.unwrap();
                outgoing.write_all(&bytes).await.unwrap();
                if let Some(signal) = signal {
                    let response = message(&mut outgoing).await;
                    assert!(String::from_utf8_lossy(&response).contains("\"state\":\"observed\""));
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
    let mut child = Process(
        cli(f, id, &state)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(15), received)
        .await
        .unwrap()
        .unwrap();
    f.config.socket = original;
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    proxy.abort();
    let _ = proxy.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let output = cli(f, id, &state).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let observed: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(observed["report"], report);
    assert_eq!(observed["selected_checks_passed_at_capture"], true);
    assert_eq!(observed["merge_authorized"], false);
    assert_eq!(observed["host_policy_verified"], false);
    assert!(
        cli(f, id, &f.dir.path().join("missing-inspection-state"))
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.inspection.observed'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    f.credential_path = owner.to_owned();
    let status = result(request(
        f,
        &json!({"action":"inspection_status","epoch":1,"inspection":id.to_string()}),
    ));
    assert_eq!(status["report"], report);
}
