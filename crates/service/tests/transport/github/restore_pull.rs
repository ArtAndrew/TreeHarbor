use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) async fn exercise(f: &mut Fixture, server: &mut Process, owner: &Path, bridge: &Path) {
    let row=f.sql.query_one("SELECT pull,binding,observation FROM th_github_pulls WHERE state='observed' ORDER BY created_at LIMIT 1",&[]).await.unwrap();
    let operation: Uuid = row.get("pull");
    let binding: Uuid = row.get("binding");
    let prior: Value = row.get("observation");
    let snapshot = prior["pull"].clone();
    f.credential_path = owner.to_owned();
    result(request(
        f,
        &json!({"action":"revoke","key":"restore-pull-revoke","epoch":1,"binding":binding}),
    ));
    // Explicit quarantine/capture fixtures: transport tests exercise the actual
    // host adapter separately. This trial proves HTTP and local journal replay.
    let manifest = "91".repeat(32);
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
    f.sql.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)",
        &[&f.scope.tenant(),&f.scope.repository(),&format!("restore-quarantine:{manifest}"),&digest,&json!({"version":1,"status":"quarantined","manifest_sha256":manifest,"database":database})]).await.unwrap();
    f.credential_path = bridge.to_owned();
    let context = result(request(
        f,
        &json!({"action":"restore_context","epoch":1,"binding":binding,"manifest_sha256":manifest}),
    ));
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let identity = snapshot["identity"].clone();
    let evidence = json!({"version":1,"binding":binding,"manifest_sha256":manifest,
        "context_sha256":treeharbor_metadata::github::restore_digest(&context).unwrap(),"operation":operation,"identity":identity,
        "report":{"version":1,"app_id":context["specification"]["app_id"],"installation_id":context["specification"]["upstream"]["installation_id"],
            "started_at_unix_ms":now,"finished_at_unix_ms":now,"before":snapshot,"after":snapshot,"token_revoked":true}});
    let state = f.dir.path().join("restore-pull-capture");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = json!({"version":1,"kind":"restore_pull","tenant":f.scope.tenant(),"repository":f.scope.repository(),
        "epoch":1,"binding":binding,"manifest_sha256":manifest,"operation":operation,"identity":identity});
    files::write_private(&state.join("intent.json"), intent.to_string().as_bytes()).unwrap();
    let key = "github-publish-report-restore-pull-fixture";
    files::write_private(
        &state.join("report-0000.json"),
        json!({"version":1,"key":key,"report":evidence,"token_expires_at":null})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let record =
        json!({"action":"restore_pull_record","key":key,"epoch":1,"specification":evidence});
    let saved = result(request(f, &record));
    assert_eq!(saved["metadata_and_marker_match"], true);
    assert_eq!(saved["publication_authorized"], false);
    assert!(!state.join("ack-0000.json").exists());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let cli = |f: &Fixture| {
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .env("PATH", "/no-executables")
            .args(["github", "restore-inspect-pull", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(&f.credential_path)
            .args([
                "--binding",
                &binding.to_string(),
                "--epoch",
                "1",
                "--manifest-sha256",
                &manifest,
                "--operation",
                &operation.to_string(),
                "--number",
                &identity["number"].to_string(),
                "--node-id",
                identity["node_id"].as_str().unwrap(),
            ])
            .arg("--state")
            .arg(&state)
            .arg("--config")
            .arg(f.dir.path().join("absent-app-config.json"))
            .output()
            .unwrap()
    };
    for _ in 0..2 {
        let result = cli(f);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&result.stdout).unwrap(),
            saved
        );
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(state.join("ack-0000.json")).unwrap()).unwrap(),
        saved
    );
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE kind='github.restore.pull.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    super::restore_queue::exercise(f, server, binding, &manifest).await;
    // Local acknowledgment is never substituted for current server authorization.
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "read",
    ]);
    assert!(!cli(f).status.success());
}
