use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; executes the actual doctor CLI"]
async fn doctor_distinguishes_missing_namespace_unmigrated_and_current_without_writes() {
    let mut f = Fixture::new().await;
    let original_schema = f.config.schema.clone();
    let storage = f.dir.path().join("objects");
    fs::create_dir(&storage).unwrap();
    fs::set_permissions(&storage, fs::Permissions::from_mode(0o700)).unwrap();
    f.config.storage = Some(treeharbor_service::config::StorageConfig {
        root: storage.clone(),
        backend: "doctor-test".into(),
    });
    f.config.schema = format!("th_doctor_{}", Uuid::new_v4().simple());
    f.save_config();
    let run = || {
        let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .args(["doctor", "--config"])
            .arg(&f.config_path)
            .output()
            .unwrap();
        assert!(output.stderr.is_empty());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(output.status.success(), report["passed"] == true);
        assert!(
            !String::from_utf8(output.stdout)
                .unwrap()
                .contains(&f.config.schema)
        );
        report
    };
    let missing = run();
    assert!(
        missing["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "schema_unavailable")
    );
    let exists: bool = f
        .sql
        .query_one(
            "SELECT EXISTS(SELECT FROM pg_namespace WHERE nspname::text=$1)",
            &[&f.config.schema],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!exists);
    f.sql
        .batch_execute(&format!("CREATE SCHEMA {}", f.config.schema))
        .await
        .unwrap();
    let unmigrated = run();
    assert!(
        unmigrated["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "schema_unverified")
    );
    let relations: i64 = f.sql.query_one("SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname::text=$1", &[&f.config.schema]).await.unwrap().get(0);
    assert_eq!(relations, 0, "doctor must not install tables");
    f.admin_ok(&["migrate"]);
    let healthy = run();
    assert_eq!(healthy["passed"], true);
    let bindings: i64 = f
        .sql
        .query_one(
            &format!(
                "SELECT count(*) FROM {}.th_storage_backends",
                f.config.schema
            ),
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(bindings, 0, "doctor must not adopt a storage backend");
    assert_eq!(fs::read_dir(&storage).unwrap().count(), 0);
    assert!(!f.config.socket.exists());
    assert!(!f.config.socket.with_extension("sock.lock").exists());
    f.sql
        .batch_execute(&format!("DROP SCHEMA {} CASCADE", f.config.schema))
        .await
        .unwrap();
    f.config.schema = original_schema;
    f.cleanup().await;
}
