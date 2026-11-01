use super::*;
use sha2::{Digest, Sha256};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; real Git oracle/native upload and server mapping with no executable PATH; quarantine journal is a fixture"]
async fn restore_source_mapping_uses_retained_objects_and_survives_lost_response() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    let agent = f.importer();
    let owner = f.credential_path.clone();
    let bridge = provision_bridge(&f);
    let pack_path = f.dir.path().join("source-pack");
    git_history_pack(&f, &pack_path);
    let pack = treeharbor_service::pack::Pack::open(&pack_path).unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(pack_path.join("manifest.json")).unwrap()).unwrap();
    let git_revision = manifest["source_revision"].as_str().unwrap();
    let root = pack.commit();
    let mut server = f.start().await;
    result(f.cli(&[
        "source-import",
        "--pack",
        pack_path.to_str().unwrap(),
        "--state",
        f.dir.path().join("import.json").to_str().unwrap(),
        "--authority",
        "connected",
        "--epoch",
        "1",
    ]));
    let binding: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &binding));
    result(request(
        &f,
        &json!({"action":"revoke","key":"revoke","epoch":1,"binding":binding["binding"]}),
    ));
    // Explicit recovery journal fixture, not a claim that this test ran pg_restore.
    let backup = "88".repeat(32);
    let database: String = f
        .sql
        .query_one("SELECT current_database()::text", &[])
        .await
        .unwrap()
        .get(0);
    let digest = Sha256::digest(format!(
        "treeharbor-restore-quarantine\0v1\0{backup}\0{database}"
    ))
    .to_vec();
    f.sql.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)",
        &[&f.scope.tenant(),&f.scope.repository(),&format!("restore-quarantine:{backup}"),&digest,&json!({"version":1,"status":"quarantined","manifest_sha256":backup,"database":database})]).await.unwrap();
    f.credential_path = bridge;
    let ctx = result(request(
        &f,
        &json!({"action":"restore_context","epoch":1,"binding":binding["binding"],"manifest_sha256":backup}),
    ));
    f.credential_path = owner;
    let decision = result(request(
        &f,
        &json!({"action":"restore_decision_record","key":"preserved","epoch":1,"specification":{
        "version":1,"binding":binding["binding"],"manifest_sha256":backup,"context_sha256":treeharbor_metadata::github::restore_digest(&ctx).unwrap(),
        "evidence_sha256":"99".repeat(32),"subject":{"kind":"external_branch","name":"treeharbor/recovered"},
        "disposition":{"action":"preserve_source","source_holder":format!("base:{}",root.hex()),"native_revision":RevisionRef::Native(root).to_string()},
        "previous_key":null,"note":"Synthetic host subject with actual accepted Git history."}}),
    ));
    assert_eq!(
        decision["source_preservation"]["git_mapping_verified"],
        false
    );
    let fields = json!({"key":"map-source","epoch":1,"decision_key":"preserved","git_revision":git_revision});
    let denial = client::restore_verify_source(
        &f.config.socket,
        &agent,
        fields.clone(),
        Duration::from_secs(15),
    )
    .await
    .unwrap();
    assert_eq!(denial.status.as_u16(), 403);
    let mut forged = fields.clone();
    forged["git_mapping_verified"] = json!(true);
    assert_eq!(
        client::restore_verify_source(
            &f.config.socket,
            &f.credential(),
            forged,
            Duration::from_secs(15)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        400
    );
    let mut wrong = fields.clone();
    wrong["git_revision"] = json!(format!("git:sha1:{}", "00".repeat(20)));
    let failed = client::restore_verify_source(
        &f.config.socket,
        &f.credential(),
        wrong,
        Duration::from_secs(15),
    )
    .await
    .unwrap();
    assert_eq!(failed.status.as_u16(), 409, "{}", failed.body);
    assert_eq!(failed.body["error"]["code"], "source_mapping_mismatch");
    let count: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_operations WHERE kind='github.restore.mapping.v1'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);
    // CLI and server both run with a PATH containing no executable. The reference
    // Git repository/bundle were already removed by git_history_pack.
    let receipt = result(f.cli(&[
        "restore-verify-source",
        "--key",
        "map-source",
        "--epoch",
        "1",
        "--decision-key",
        "preserved",
        "--git-revision",
        git_revision,
    ]));
    assert_eq!(receipt["git_mapping_verified"], true);
    assert_eq!(receipt["git_revision"], git_revision);
    assert_eq!(
        receipt["native_revision"],
        RevisionRef::Native(root).to_string()
    );
    assert_eq!(receipt["source_objects"], pack.object_count());
    assert_eq!(receipt["source_bytes"], pack.encoded_bytes());
    assert_eq!(receipt["publication_authorized"], false);
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    server = f.start().await;
    let replay = client::restore_verify_source(
        &f.config.socket,
        &f.credential(),
        fields.clone(),
        Duration::from_secs(15),
    )
    .await
    .unwrap();
    assert_eq!(replay.status.as_u16(), 200);
    assert_eq!(replay.body["result"], receipt);
    let historical = result(request(
        &f,
        &json!({"action":"restore_mapping_status","key":"map-source"}),
    ));
    assert_eq!(historical, receipt);
    let mut changed = fields.clone();
    changed["decision_key"] = json!("other");
    assert_eq!(
        client::restore_verify_source(
            &f.config.socket,
            &f.credential(),
            changed,
            Duration::from_secs(15)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        409
    );
    let mut bind = binding.clone();
    bind["key"] = json!("new-bind");
    bind["binding"] = json!(Uuid::new_v4());
    assert!(!request(&f, &bind).status.success());
    // Current authority fences stop new verification. Historical receipt recovery
    // still returns facts already committed; it is never an external write grant.
    f.sql
        .execute(
            "UPDATE th_repositories SET state='frozen' WHERE tenant=$1 AND repository=$2",
            &[&f.scope.tenant(), &f.scope.repository()],
        )
        .await
        .unwrap();
    let mut fresh = fields.clone();
    fresh["key"] = json!("map-after-freeze");
    assert_eq!(
        client::restore_verify_source(
            &f.config.socket,
            &f.credential(),
            fresh,
            Duration::from_secs(15)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        409
    );
    assert_eq!(
        client::restore_verify_source(
            &f.config.socket,
            &f.credential(),
            fields,
            Duration::from_secs(15)
        )
        .await
        .unwrap()
        .body["result"],
        receipt
    );
    let storage = &f.config.storage.as_ref().unwrap().root;
    assert!(!fs::read_dir(storage).unwrap().any(|e| {
        e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".treeharbor-restore-source-")
    }));
    drop(server);
    f.cleanup().await;
}
