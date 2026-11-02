use super::*;
pub(super) async fn exercise(f: &mut Fixture, server: &mut Process, binding: Uuid, manifest: &str) {
    let row=f.sql.query_one("SELECT publication,latest_report FROM th_github_check_publications WHERE binding=$1 AND latest_report#>>'{check,node_id}' IS NOT NULL ORDER BY publication LIMIT 1",&[&binding]).await.unwrap();
    let operation: Uuid = row.get(0);
    let prior: Value = row.get(1);
    let check = &prior["check"];
    let identity = json!({"id":check["id"],"node_id":check["node_id"]});
    let bundle = result(request(
        f,
        &json!({"action":"restore_check_context","epoch":1,"binding":binding,"manifest_sha256":manifest,"operation":operation,"identity":identity}),
    ));
    let snapshot = json!({"identity":identity,"app_id":check["app_id"],"name":check["name"],"head_revision":check["head_revision"],"external_id":check["external_id"],"status":check["status"],"conclusion":check["conclusion"]});
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    // Explicit host-capture fixture; HTTP adapter tests exercise provider reads.
    let spec = json!({"version":1,"binding":binding,"manifest_sha256":manifest,"context_sha256":bundle["context_sha256"],"operation":operation,"identity":identity,
        "report":{"version":1,"app_id":bundle["context"]["specification"]["app_id"],"installation_id":bundle["context"]["specification"]["upstream"]["installation_id"],"repository_id":bundle["context"]["specification"]["upstream"]["repository_id"],"started_at_unix_ms":now,"finished_at_unix_ms":now,"before":snapshot,"after":snapshot,"token_revoked":true}});
    let state = f.dir.path().join("restore-check-capture");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = json!({"version":1,"kind":"restore_check","tenant":f.scope.tenant(),"repository":f.scope.repository(),"epoch":1,"binding":binding,"manifest_sha256":manifest,"operation":operation,"identity":identity});
    files::write_private(&state.join("intent.json"), intent.to_string().as_bytes()).unwrap();
    let key = "github-publish-report-restore-check-fixture";
    files::write_private(
        &state.join("report-0000.json"),
        json!({"version":1,"key":key,"report":spec,"token_expires_at":null})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let saved = result(request(
        f,
        &json!({"action":"restore_check_record","epoch":1,"key":key,"specification":spec}),
    ));
    assert_eq!(saved["metadata_matches"], true);
    assert_eq!(saved["validation_authorized"], false);
    assert!(!state.join("ack-0000.json").exists());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    for _ in 0..2 {
        let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .env("PATH", "/no-executables")
            .args(["github", "restore-inspect-check", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(&f.credential_path)
            .args([
                "--epoch",
                "1",
                "--binding",
                &binding.to_string(),
                "--manifest-sha256",
                manifest,
                "--operation",
                &operation.to_string(),
                "--check-id",
                &identity["id"].to_string(),
                "--node-id",
                identity["node_id"].as_str().unwrap(),
            ])
            .arg("--state")
            .arg(&state)
            .arg("--config")
            .arg(f.dir.path().join("absent-app.json"))
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&output.stdout).unwrap(),
            saved
        );
    }
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(state.join("ack-0000.json")).unwrap()).unwrap(),
        saved
    );
    assert_eq!(f.sql.query_one("SELECT count(*) FROM th_operations WHERE key=$1 AND kind='github.restore.check.v1'",&[&key]).await.unwrap().get::<_,i64>(0),1);
    assert_eq!(
        f.sql
            .query_one(
                "SELECT latest_report FROM th_github_check_publications WHERE publication=$1",
                &[&operation]
            )
            .await
            .unwrap()
            .get::<_, Value>(0),
        prior
    );
}
