use super::*;
use sha2::{Digest, Sha256};
fn cli(f: &Fixture, action: &str) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args([
            "github",
            "restore-reconnect",
            action,
            "--epoch",
            "1",
            "--socket",
        ])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path);
    c
}
fn output(c: &mut Command) -> Value {
    let out = c.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
async fn interrupt_activation(f: &mut Fixture, server: &mut Process, spec: &std::path::Path) {
    let original = f.config.socket.clone();
    let path = f.dir.path().join("activate-fault.sock");
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    f.config.socket = path;
    let upstream = original.clone();
    let (sent, got) = tokio::sync::oneshot::channel();
    let proxy = tokio::spawn(async move {
        let (mut input, _) = listener.accept().await.unwrap();
        let bytes = message(&mut input).await;
        assert!(
            String::from_utf8_lossy(&bytes).contains("\"action\":\"restore_reconnect_activate\"")
        );
        let mut output = UnixStream::connect(upstream).await.unwrap();
        output.write_all(&bytes).await.unwrap();
        let reply = message(&mut output).await;
        assert!(
            reply.starts_with(b"HTTP/1.1 200 "),
            "{}",
            String::from_utf8_lossy(&reply)
        );
        sent.send(()).unwrap();
        std::future::pending::<()>().await;
    });
    let diagnostic = f.dir.path().join("activate.stderr");
    let mut worker = Process(
        cli(f, "activate")
            .args(["--key", "activate", "--specification"])
            .arg(spec)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    tokio::time::timeout(Duration::from_secs(30), got)
        .await
        .expect("activation reply not committed")
        .unwrap();
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    proxy.abort();
    let _ = proxy.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    f.config.socket = original;
    *server = f.start().await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual CLI reconnect/lost activation ack/server restart; external probe/inventory/quarantine are explicit fixtures"]
async fn restore_reconnect_cli_recovers_activation_without_app_or_duplicate_binding() {
    exercise(false).await;
    exercise(true).await;
}
async fn exercise(prospective: bool) {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let mut server = f.start().await;
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    if !prospective {
        result(request(&f, &bind));
        result(request(
            &f,
            &json!({"action":"revoke","key":"revoke","epoch":1,"binding":bind["binding"]}),
        ));
    }
    let manifest = "97".repeat(32);
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
    if prospective {
        let target = json!({"version":1,"manifest_sha256":manifest,"target":bind["binding"],"binding":bind["specification"],"evidence_sha256":"96".repeat(32),"prospective_target_confirmed":true});
        let path = f.dir.path().join("target.json");
        files::write_private(&path, target.to_string().as_bytes()).unwrap();
        let register = |f: &Fixture| {
            let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
            c.env("PATH", "/no-executables")
                .args([
                    "github",
                    "restore-register-target",
                    "--epoch",
                    "1",
                    "--key",
                    "target",
                    "--socket",
                ])
                .arg(&f.config.socket)
                .arg("--credential-file")
                .arg(&f.credential_path)
                .arg("--specification")
                .arg(&path);
            c
        };
        let registered = output(&mut register(&f));
        assert_eq!(registered["historical_connection_asserted"], false);
        assert_eq!(registered["publication_authorized"], false);
        assert_eq!(
            f.sql
                .query_one(
                    "SELECT count(*) FROM th_outbox WHERE kind='github.binding.configured'",
                    &[]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
        server.0.kill().unwrap();
        server.0.wait().unwrap();
        server = f.start().await;
        assert_eq!(registered, output(&mut register(&f)));
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
    }
    let new_binding = Uuid::new_v4();
    let plan = json!({"version":1,"manifest_sha256":manifest,"source_binding":bind["binding"],"new_binding":new_binding,"binding":bind["specification"],"ttl_seconds":300,"evidence_sha256":"98".repeat(32),"old_writers_and_runtimes_fenced":true,"old_credentials_cannot_mint":true,"old_tokens_revoked_or_expired":true,"prior_host_mutations_settled":true,"host_writes_controlled":true,"host_policy_reviewed":true});
    let path = f.dir.path().join("reconnect-plan.json");
    files::write_private(&path, plan.to_string().as_bytes()).unwrap();
    let planned = output(
        cli(&f, "plan")
            .args(["--key", "plan", "--specification"])
            .arg(path),
    );
    let mut second = plan.clone();
    second["new_binding"] = json!(Uuid::new_v4());
    let cancel_path = f.dir.path().join("cancel-plan.json");
    files::write_private(&cancel_path, second.to_string().as_bytes()).unwrap();
    output(
        cli(&f, "plan")
            .args(["--key", "cancel-plan", "--specification"])
            .arg(cancel_path),
    );
    let cancelled =
        output(cli(&f, "cancel").args(["--key", "cancel", "--plan-key", "cancel-plan"]));
    assert_eq!(cancelled["outcome"], "cancelled");
    let after = planned["observe_after_unix"].as_u64().unwrap();
    let current = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    if current < after {
        tokio::time::sleep(Duration::from_secs(after - current)).await;
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let observation = json!({"version":1,"api_version":"2026-03-10","observed_at_unix":now,"app_id":7,"binding":bind["specification"]["upstream"],"repository_node_id":"R_fixture","owner_type":"Organization","private":false,"default_branch":"main","archived":false,"disabled":false,"read_connection_verified":true,"probe_token_revoked":true,"queue_eligibility":"public_organization_eligible","queue_configuration_verified":false,"publication_ready":false});
    let proof = json!({"version":1,"plan_key":"plan","plan_sha256":treeharbor_metadata::github::restore_digest(&planned).unwrap(),"started_at_unix":now,"finished_at_unix":now,"observation":observation});
    let state = f.dir.path().join("probe");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    files::write_private(&state.join("report-0000.json"),json!({"version":1,"key":"github-publish-report-reconnect-fixture","report":proof,"token_expires_at":null}).to_string().as_bytes()).unwrap();
    f.credential_path = bridge;
    let probed = output(
        cli(&f, "probe")
            .args(["--plan-key", "plan", "--state"])
            .arg(&state)
            .arg("--config")
            .arg(f.dir.path().join("missing-app")),
    );
    assert_eq!(probed["publication_authorized"], false);
    let ctx = result(request(
        &f,
        &json!({"action":"restore_context","epoch":1,"binding":bind["binding"],"manifest_sha256":manifest}),
    ));
    // An empty retained host is an explicit fixture. Source/pin coverage with
    // actual Rust projection and nonempty inventories is tested in metadata.
    let inv = json!({"version":1,"binding":bind["binding"],"manifest_sha256":manifest,"context_sha256":treeharbor_metadata::github::restore_digest(&ctx).unwrap(),"app_id":7,"installation_id":9,"repository_id":11,"repository_node":"R_fixture","default_branch":"main","started_at_unix":now,"finished_at_unix":now,"head_count":0,"pull_count":0,"page_sha256":[],"token_revoked":true,"token_expires_at":"fixture"});
    result(request(
        &f,
        &json!({"action":"restore_inventory_start","epoch":1,"key":"inventory","specification":inv}),
    ));
    result(request(
        &f,
        &json!({"action":"restore_inventory_seal","epoch":1,"inventory_key":"inventory"}),
    ));
    f.credential_path = owner;
    let review_spec =
        json!({"version":1,"manifest_sha256":manifest,"inventory_keys":["inventory"]});
    let reviewed = result(request(
        &f,
        &json!({"action":"restore_review","epoch":1,"specification":review_spec}),
    ));
    assert_eq!(reviewed["assessment"]["local_review_complete"], true);
    let activation = json!({"version":1,"plan_key":"plan","probe_key":probed["key"],"review":review_spec,"assessment_sha256":reviewed["assessment_sha256"],"external_assertions_still_valid":true});
    let act_path = f.dir.path().join("activation.json");
    files::write_private(&act_path, activation.to_string().as_bytes()).unwrap();
    interrupt_activation(&mut f, &mut server, &act_path).await;
    let accepted = output(
        cli(&f, "activate")
            .args(["--key", "activate", "--specification"])
            .arg(&act_path),
    );
    assert_eq!(accepted["binding"], json!(new_binding));
    assert_eq!(accepted["external_fencing_verified"], false);
    let status = output(cli(&f, "status").args(["--plan-key", "plan"]));
    assert_eq!(status["current_binding_state"], "active");
    assert_eq!(status["reconciliation_required"], false);
    assert_eq!(f.sql.query_one("SELECT count(*) FROM th_operations WHERE kind='github.restore.reconnect.activate.v1'",&[]).await.unwrap().get::<_,i64>(0),1);
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_github_bindings", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    result(request(
        &f,
        &json!({"action":"revoke","key":"revoke-new","epoch":1,"binding":new_binding}),
    ));
    assert_eq!(
        accepted,
        output(
            cli(&f, "activate")
                .args(["--key", "activate", "--specification"])
                .arg(&act_path)
        )
    );
    assert_eq!(
        output(cli(&f, "status").args(["--plan-key", "plan"]))["current_binding_state"],
        "revoked"
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
