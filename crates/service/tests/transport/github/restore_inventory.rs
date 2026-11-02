use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;
fn cli(f: &Fixture, state: &Path, manifest: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args([
            "github",
            "restore-capture-inventory",
            "--epoch",
            "1",
            "--binding",
            "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
            "--socket",
        ])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .arg("--manifest-sha256")
        .arg(manifest)
        .arg("--state")
        .arg(state)
        .arg("--config")
        .arg(f.dir.path().join("missing-app.json"));
    c
}
// Lose the actual page response after its transaction commits, then
// remove the worker and server. No host endpoint is injected into production.
async fn interrupt_page(f: &mut Fixture, server: &mut Process, state: &Path) {
    let path = f.dir.path().join("inventory-fault.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = f.config.socket.clone();
    let upstream = original.clone();
    f.config.socket = path;
    let (sent, got) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let mut sent = Some(sent);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut input, _) = listener.accept().await.unwrap();
            let bytes = message(&mut input).await;
            let selected =
                String::from_utf8_lossy(&bytes).contains("\"action\":\"restore_inventory_page\"");
            let signal = if selected { sent.take() } else { None };
            let upstream = upstream.clone();
            clients.spawn(async move {
                let mut output = UnixStream::connect(upstream).await.unwrap();
                output.write_all(&bytes).await.unwrap();
                if let Some(signal) = signal {
                    let reply = message(&mut output).await;
                    assert!(
                        reply.starts_with(b"HTTP/1.1 200 "),
                        "{}",
                        String::from_utf8_lossy(&reply)
                    );
                    signal.send(()).unwrap();
                    std::future::pending::<()>().await;
                } else {
                    let _ = tokio::io::copy_bidirectional(&mut input, &mut output).await;
                }
            });
        }
    });
    let diagnostic = f.dir.path().join("inventory.stderr");
    let mut worker = Process(
        cli(f, state, &"96".repeat(32))
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let reached = tokio::time::timeout(Duration::from_secs(30), got).await;
    assert!(
        reached.is_ok(),
        "worker {:?}: {}",
        worker.0.try_wait().unwrap(),
        fs::read_to_string(&diagnostic).unwrap()
    );
    reached.unwrap().unwrap();
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    proxy.abort();
    let _ = proxy.await;
    f.config.socket = original;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual paged CLI capture persistence, lost page response and restart; provider and quarantine are fixtures"]
async fn restore_inventory_cli_recovers_a_partial_upload_without_app_access() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let mut server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &bind));
    result(request(
        &f,
        &json!({"action":"revoke","key":"revoke","epoch":1,"binding":bind["binding"]}),
    ));
    let manifest = "96".repeat(32);
    let database: String = f
        .sql
        .query_one("SELECT current_database()::text", &[])
        .await
        .unwrap()
        .get(0);
    let digest = Sha256::digest(format!(
        "treeharbor-restore-quarantine\0v1\0{manifest}\0{database}"
    ))
    .to_vec();
    f.sql.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)",&[&f.scope.tenant(),&f.scope.repository(),&format!("restore-quarantine:{manifest}"),&digest,&json!({"version":1,"status":"quarantined","manifest_sha256":manifest,"database":database})]).await.unwrap();
    f.credential_path = bridge;
    let ctx = result(request(
        &f,
        &json!({"action":"restore_context","epoch":1,"binding":bind["binding"],"manifest_sha256":manifest}),
    ));
    let v: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/restore-inventory-v1.json"
    ))
    .unwrap();
    let mut spec = v["request"]["specification"].clone();
    spec["binding"] = bind["binding"].clone();
    spec["manifest_sha256"] = json!(manifest);
    spec["context_sha256"] = json!(treeharbor_metadata::github::restore_digest(&ctx).unwrap());
    spec["app_id"] = ctx["specification"]["app_id"].clone();
    spec["installation_id"] = ctx["specification"]["upstream"]["installation_id"].clone();
    spec["repository_id"] = ctx["specification"]["upstream"]["repository_id"].clone();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    spec["started_at_unix"] = json!(now);
    spec["finished_at_unix"] = json!(now);
    let state = f.dir.path().join("inventory");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let path = state.join("inventory.json");
    fs::write(
        &path,
        serde_json::to_vec(
            &json!({"version":1,"key":"inventory-fixture","specification":spec,"pages":v["pages"]}),
        )
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    interrupt_page(&mut f, &mut server, &state).await;
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE kind='github.restore.inventory.page.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(!state.join("sealed.json").exists());
    let out = cli(&f, &state, &manifest).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sealed: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(sealed["inventory_complete"], true);
    assert_eq!(sealed["publication_authorized"], false);
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    server = f.start().await;
    fs::remove_file(state.join("sealed.json")).unwrap();
    let replay = cli(&f, &state, &manifest).output().unwrap();
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&replay.stdout).unwrap(),
        sealed
    );
    f.credential_path = owner;
    let before_review: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_operations", &[])
        .await
        .unwrap()
        .get(0);
    let reviewed = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/no-executables")
        .args(["github", "restore-review", "--epoch", "1", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .arg("--manifest-sha256")
        .arg(&manifest)
        .args(["--inventory", "inventory-fixture"])
        .output()
        .unwrap();
    assert!(
        reviewed.status.success(),
        "{}",
        String::from_utf8_lossy(&reviewed.stderr)
    );
    let review: Value = serde_json::from_slice(&reviewed.stdout).unwrap();
    assert_eq!(
        review["assessment"]["bindings"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        review["assessment"]["bindings"][0]["sources"]["missing_git"]["count"],
        2
    );
    assert_eq!(review["assessment"]["local_review_complete"], false);
    assert_eq!(review["assessment"]["publication_authorized"], false);
    assert_eq!(
        before_review,
        f.sql
            .query_one("SELECT count(*) FROM th_operations", &[])
            .await
            .unwrap()
            .get::<_, i64>(0)
    );
    for page in 0..2 {
        let coverage = result(request(
            &f,
            &json!({"action":"restore_inventory_coverage","epoch":1,"inventory_key":"inventory-fixture","page":page}),
        ));
        assert!(coverage["missing_on_page"].as_u64().unwrap() > 0);
        assert_eq!(coverage["publication_authorized"], false);
    }
    for (kind, count) in [("start", 1), ("page", 2), ("seal", 1)] {
        assert_eq!(
            f.sql
                .query_one(
                    "SELECT count(*) FROM th_operations WHERE kind=$1",
                    &[&format!("github.restore.inventory.{kind}.v1")]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
            count
        );
    }
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_github_bindings WHERE state='active'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_github_publications", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    drop(server);
    f.cleanup().await;
}
