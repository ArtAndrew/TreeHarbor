use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;

fn cli(f: &Fixture, state: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args([
            "github",
            "restore-acquire-source",
            "--epoch",
            "1",
            "--decision-key",
            "acquire-source",
            "--socket",
        ])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .arg("--state")
        .arg(state)
        .arg("--config")
        .arg(f.dir.path().join("missing-app.json"))
        .args(["--git", "/no-git"]);
    c
}

// Lose the actual preservation response after its transaction commits, then
// remove the worker and server. No host endpoint is injected into production.
async fn interrupt_preservation(f: &mut Fixture, server: &mut Process, state: &Path) {
    let path = f.dir.path().join("acquire-fault.sock");
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
                String::from_utf8_lossy(&bytes).contains("\"action\":\"restore_decision_record\"");
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
    let diagnostic = f.dir.path().join("acquire.stderr");
    let mut worker = Process(
        cli(f, state)
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
#[ignore = "requires PostgreSQL; complete real source acquisition from a retained Git pack, lost preservation response, native verification and replay; host download/quarantine are not live"]
async fn restore_acquisition_retains_post_backup_history_and_recovers_without_local_source() {
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
    let manifest = "94".repeat(32);
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
    f.sql.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)", &[&f.scope.tenant(),&f.scope.repository(),&format!("restore-quarantine:{manifest}"),&digest,&json!({"version":1,"status":"quarantined","manifest_sha256":manifest,"database":database})]).await.unwrap();
    f.credential_path = bridge;
    let ctx = result(request(
        &f,
        &json!({"action":"restore_context","epoch":1,"binding":bind["binding"],"manifest_sha256":manifest}),
    ));
    f.credential_path = owner;
    // Source is absent from the restored database. Model an operator-retained
    // complete source spool; real download safety has independent transport tests.
    let state = f.dir.path().join("acquisition");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    git_history_pack(&f, &state.join("pack"));
    let pack = treeharbor_service::pack::Pack::open(&state.join("pack")).unwrap();
    let source: Value =
        serde_json::from_slice(&fs::read(state.join("pack/manifest.json")).unwrap()).unwrap();
    let git = source["source_revision"].clone();
    let root = pack.commit();
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_bases", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let acquisition = json!({"version":3,"binding":bind["binding"],"manifest_sha256":manifest,"context_sha256":treeharbor_metadata::github::restore_digest(&ctx).unwrap(),"evidence_sha256":"95".repeat(32),"subject":{"kind":"external_branch","name":"treeharbor/post-backup"},"disposition":{"action":"acquire_source","git_revision":git,"max_objects":1000,"max_bytes":1048576},"previous_key":null,"note":"Acquire exact post-backup source; synthetic host identity."});
    result(request(
        &f,
        &json!({"action":"restore_decision_record","key":"acquire-source","epoch":1,"specification":acquisition}),
    ));
    interrupt_preservation(&mut f, &mut server, &state).await;
    assert!(!state.join("mapping.json").exists());
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE kind='github.restore.mapping.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    let pin: String = f.sql.query_one("SELECT result#>>'{source_preservation,holder}' FROM th_operations WHERE kind='github.restore.decision.v1' AND result#>>'{specification,disposition,action}'='preserve_source'",&[]).await.unwrap().get(0);
    assert_eq!(
        f.sql
            .query_one("SELECT digest FROM th_object_pins WHERE holder=$1", &[&pin])
            .await
            .unwrap()
            .get::<_, Vec<u8>>(0),
        root.as_bytes()
    );
    fs::remove_dir_all(state.join("pack")).unwrap();
    let output = cli(&f, &state).output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(saved["git_revision"], git);
    assert_eq!(
        saved["native_revision"],
        RevisionRef::Native(root).to_string()
    );
    assert_eq!(saved["source_objects"], pack.object_count());
    assert_eq!(saved["source_bytes"], pack.encoded_bytes());
    assert_eq!(saved["git_mapping_verified"], true);
    assert_eq!(saved["publication_authorized"], false);
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    server = f.start().await;
    fs::remove_file(state.join("mapping.json")).unwrap();
    for _ in 0..2 {
        let out = cli(&f, &state).output().unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(serde_json::from_slice::<Value>(&out.stdout).unwrap(), saved);
    }
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE kind='github.restore.mapping.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
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
