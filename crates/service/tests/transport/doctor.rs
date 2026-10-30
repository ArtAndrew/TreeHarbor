use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; executes the actual doctor CLI"]
async fn doctor_distinguishes_missing_namespace_unmigrated_and_current_without_writes() {
    let mut f = Fixture::new().await;
    let original_schema = f.config.schema.clone();
    let schema = format!("th_doctor_{}", Uuid::new_v4().simple());
    let input = f.dir.path().join("database.url");
    files::write_private(&input, f.config.database_url.as_bytes()).unwrap();
    let directory = f.dir.path().join("service");
    let initialized = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["init", "--directory"])
        .arg(&directory)
        .arg("--database-url-file")
        .arg(&input)
        .args(["--schema", &schema, "--backend", "doctor-test"])
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    assert!(initialized.stderr.is_empty());
    assert!(!String::from_utf8_lossy(&initialized.stdout).contains(&f.config.database_url));
    let receipt: Value = serde_json::from_slice(&initialized.stdout).unwrap();
    assert_eq!(receipt["database_initialized"], false);
    assert_eq!(receipt["credentials_issued"], false);
    f.config_path = PathBuf::from(receipt["config"].as_str().unwrap());
    f.config = Config::load(&f.config_path).unwrap();
    let storage = f.config.storage.as_ref().unwrap().root.clone();
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
    // The same generated configuration starts the real server after the explicit
    // migration. Init itself did not bind a socket, initialize storage or migrate.
    let server = f.start().await;
    assert!(
        f.raw(b"GET /healthz HTTP/1.1\r\nHost: treeharbor.local\r\n\r\n")
            .await
            .starts_with(b"HTTP/1.1 200 ")
    );
    drop(server);
    f.sql
        .batch_execute(&format!("DROP SCHEMA {} CASCADE", f.config.schema))
        .await
        .unwrap();
    f.config.schema = original_schema;
    f.cleanup().await;
}
