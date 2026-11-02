use super::*;
use sha2::{Digest, Sha256};
use std::path::Path;

pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    bridge: &Path,
    binding: Uuid,
    channel: Uuid,
    recovery: Uuid,
) {
    f.credential_path = owner.to_owned();
    result(request(
        f,
        &json!({"action":"event_channel_revoke","key":"restore-delivery-channel","epoch":1,"channel":channel}),
    ));
    result(request(
        f,
        &json!({"action":"revoke","key":"restore-delivery-binding","epoch":1,"binding":binding}),
    ));
    // Explicit quarantine fixture; the host adapter and backup paths have separate trials.
    let manifest = "93".repeat(32);
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
    f.credential_path = bridge.to_owned();
    let bundle = result(request(
        f,
        &json!({"action":"restore_delivery_context","epoch":1,"binding":binding,"manifest_sha256":manifest,"recovery":recovery,"provider_id":700}),
    ));
    let prior: Value = f.sql.query_one("SELECT to_jsonb(a) FROM th_github_event_recovery_attempts a WHERE recovery=$1 AND provider_id=700", &[&recovery]).await.unwrap().get(0);
    let now: i64 = f
        .sql
        .query_one(
            "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    let mut d = bundle["retained_details"].clone();
    d["observed_at_unix_ms"] = json!(now);
    let spec = json!({"version":1,"binding":binding,"manifest_sha256":manifest,"context_sha256":bundle["context_sha256"],"recovery":recovery,"provider_id":700,
        "report":{"version":1,"app_id":7,"installation_id":9,"repository_id":11,"callback_url":bundle["callback_url"],"started_at_unix_ms":now,"finished_at_unix_ms":now,"before":d,"after":d}});
    let state = f.dir.path().join("restore-delivery-capture");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = json!({"version":1,"kind":"restore_delivery","tenant":f.scope.tenant(),"repository":f.scope.repository(),"epoch":1,"binding":binding,"manifest_sha256":manifest,"recovery":recovery,"provider_id":700});
    files::write_private(&state.join("intent.json"), intent.to_string().as_bytes()).unwrap();
    let key = "github-publish-report-restore-delivery-fixture";
    files::write_private(
        &state.join("report-0000.json"),
        json!({"version":1,"key":key,"report":spec,"token_expires_at":null})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let saved = result(request(
        f,
        &json!({"action":"restore_delivery_record","epoch":1,"key":key,"specification":spec}),
    ));
    assert_eq!(saved["inbox"]["state"], "received");
    assert_eq!(saved["prior_dispatch_settled"], false);
    assert_eq!(saved["event_processing_verified"], false);
    assert!(!state.join("ack-0000.json").exists());
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
    let cli = |f: &Fixture| {
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .env("PATH", "/no-executables")
            .args(["github", "restore-inspect-delivery", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(&f.credential_path)
            .args([
                "--epoch",
                "1",
                "--binding",
                &binding.to_string(),
                "--manifest-sha256",
                &manifest,
                "--recovery",
                &recovery.to_string(),
                "--provider-id",
                "700",
            ])
            .arg("--state")
            .arg(&state)
            .arg("--config")
            .arg(f.dir.path().join("absent-app.json"))
            .output()
            .unwrap()
    };
    for _ in 0..2 {
        let output = cli(f);
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
    assert_eq!(f.sql.query_one("SELECT to_jsonb(a) FROM th_github_event_recovery_attempts a WHERE recovery=$1 AND provider_id=700", &[&recovery]).await.unwrap().get::<_,Value>(0), prior);
    assert_eq!(f.sql.query_one("SELECT count(*) FROM th_operations WHERE key=$1 AND kind='github.restore.delivery.v1'", &[&key]).await.unwrap().get::<_,i64>(0), 1);
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
