use super::*;

pub(super) fn cli(f: &Fixture, id: Uuid, state: &Path, config: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/no-executables")
        .args(["github", "publish", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--publication", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(config)
        .args(["--git", "/no-git"]);
    command
}
pub(super) fn intent(f: &Fixture, state: &Path, c: &Value) {
    files::write_private(&state.join("intent.json"),json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),
        "publication":c["publication"],"epoch":c["epoch"],"projection":c["projection"],"binding":c["binding"],"specification":c["specification"],
        "branch_id":c["branch_id"],"branch":c["branch"],"native_revision":c["native_revision"],"expected_head":c["expected_head"],
        "expires_at_unix_ms":c["expires_at_unix_ms"]}).to_string().as_bytes()).unwrap();
}

#[cfg(target_os = "linux")]
pub(super) async fn lost_dispatch(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    publisher: &Path,
    projection: Uuid,
) {
    let id = Uuid::new_v4();
    f.credential_path = owner.to_owned();
    result(request(
        f,
        &json!({"action":"publication_request","key":"driver-request","epoch":1,"publication":id.to_string(),"specification":{"projection":projection.to_string(),"expected_head":null,"ttl_seconds":600}}),
    ));
    f.credential_path = publisher.to_owned();
    let context = result(request(
        f,
        &json!({"action":"publication_context","epoch":1,"publication":id.to_string()}),
    ));
    let state = f.dir.path().join("fresh-driver");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let pem = f.dir.path().join("publisher-fixture.pem");
    files::write_private(
        &pem,
        include_bytes!("../../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    let app = f.dir.path().join("publisher-fixture.json");
    files::write_private(&app,json!({"version":1,"app_id":context["specification"]["app_id"],"client_id":context["specification"]["client_id"],
        "private_key_file":pem,"repository":context["specification"]["upstream"]}).to_string().as_bytes()).unwrap();
    let socket = f.dir.path().join("publisher-proxy.sock");
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
            let hold = String::from_utf8_lossy(&bytes).contains("\"action\":\"publication_begin\"");
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
                    // Deliberately never deliver the committed grant to the CLI.
                    std::future::pending::<()>().await;
                } else {
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                }
            });
        }
    });
    let original = f.config.socket.clone();
    f.config.socket = socket;
    // Build explicit arguments because clap rejects repeated --git options.
    let mut command = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/no-executables")
        .args(["github", "publish", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(publisher)
        .args(["--publication", &id.to_string(), "--epoch", "1", "--state"])
        .arg(&state)
        .arg("--config")
        .arg(&app)
        .args(["--git", "/usr/bin/git"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut process = Process(command.spawn().unwrap());
    tokio::time::timeout(Duration::from_secs(90), received)
        .await
        .unwrap()
        .unwrap();
    f.config.socket = original;
    let simultaneous = cli(f, id, &state, &app).output().unwrap();
    assert!(!simultaneous.status.success());
    assert!(String::from_utf8_lossy(&simultaneous.stderr).contains("lock"));
    process.0.kill().unwrap();
    process.0.wait().unwrap();
    proxy.abort();
    let _ = proxy.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let context = result(request(
        f,
        &json!({"action":"publication_context","epoch":1,"publication":id.to_string()}),
    ));
    assert_eq!(context["state"], "attempting");
    let attempt: Value =
        serde_json::from_slice(&fs::read(state.join("attempt.json")).unwrap()).unwrap();
    assert_eq!(attempt["attempt"], context["attempt"]);
    let mapping: Value =
        serde_json::from_slice(&fs::read(state.join("mapping.json")).unwrap()).unwrap();
    assert_eq!(mapping["git_revision"], context["prepared_git"]);
    assert!(state.join("pack/manifest.json").is_file());
    fs::remove_dir_all(state.join("pack")).unwrap();
    fs::remove_file(&app).unwrap();
    fs::remove_file(pem).unwrap();
    // A durable report is enough for recovery; neither conversion nor dispatch
    // occurs again. The host observation is a fixture, deliberately labelled.
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let report = json!({"attempt":context["attempt"],"branch_id":context["branch_id"],"binding":context["binding"],"native_revision":context["native_revision"],
        "prepared_git":context["prepared_git"],"remote_revision":context["prepared_git"],"outcome":"observed","observed_at_unix_ms":now,"token_revoked":false});
    files::write_private(&state.join("report-0000.json"),json!({"version":1,"key":"github-publish-report-driver-fixture","report":report,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    let replay = cli(f, id, &state, &app).output().unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap()["state"],
        "observed"
    );
    let count:i64=f.sql.query_one("SELECT count(*) FROM th_operations WHERE kind='github.publication.begin.v1' AND key=$1",&[&format!("github-publish-begin-{id}")]).await.unwrap().get(0);
    assert_eq!(count, 1);
    f.credential_path = owner.to_owned();
}

#[cfg(target_os = "linux")]
async fn message(stream: &mut UnixStream) -> Vec<u8> {
    let mut bytes = Vec::new();
    loop {
        let mut chunk = [0; 8192];
        let n = stream.read(&mut chunk).await.unwrap();
        assert_ne!(n, 0);
        bytes.extend_from_slice(&chunk[..n]);
        assert!(bytes.len() < 2 * 1024 * 1024);
        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            assert!(!headers.contains("transfer-encoding:"));
            let length: usize = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .unwrap_or("0")
                .trim()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return bytes;
            }
        }
    }
}
