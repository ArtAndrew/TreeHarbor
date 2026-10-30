use super::*;
use std::{collections::BTreeMap, path::Path, process::Stdio};

fn create_backup(f: &Fixture, output: &Path, dump: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    cmd.args(["backup", "create", "--writers-stopped", "--config"])
        .arg(&f.config_path)
        .arg("--output")
        .arg(output)
        .arg("--pg-dump")
        .arg(dump)
        .arg("--state")
        .arg(&f.credential_path)
        .args(["--timeout-seconds", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if f.dir.path().join("task.json").exists() {
        cmd.arg("--state").arg(f.dir.path().join("task.json"));
    }
    cmd
}
fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"].clone()
}
fn save_proposal(f: &mut Fixture, base: RevisionRef) -> Value {
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--permission",
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
    ]);
    f.credential_path = f.dir.path().join("writer.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--permission",
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
        "--output",
        f.credential_path.to_str().unwrap(),
    ]);
    let profile = ok(f.cli(&[
        "profile-put",
        "--key",
        "backup-profile",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--full",
    ]));
    let task = ok(f.cli(&[
        "api-run-new",
        "--state",
        f.dir.path().join("task.json").to_str().unwrap(),
        "--base",
        &base.to_string(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--profile-digest",
        profile["profile_digest"].as_str().unwrap(),
        "--agent",
        "backup-test",
        "--task",
        "preserve-proposal",
    ]));
    let upload = Uuid::new_v4().to_string();
    ok(f.cli(&[
        "change-begin",
        "--key",
        "backup-upload",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--upload",
        &upload,
        "--run",
        task["run"].as_str().unwrap(),
        "--generation",
        "1",
    ]));
    let file = f.dir.path().join("proposal-bytes");
    files::write_private(&file, b"accepted source survives restore\n").unwrap();
    let blob = ok(f.cli(&[
        "change-put",
        "--upload",
        &upload,
        "--file",
        file.to_str().unwrap(),
    ]));
    let operations = json!([{"path_hex":"66696c652e747874","expected":null,"replacement":{"mode":33188,"object":blob["object"]}}]);
    let manifest = f.dir.path().join("operations.json");
    files::write_private(&manifest, operations.to_string().as_bytes()).unwrap();
    let accepted = ok(f.cli(&[
        "change-submit",
        "--key",
        "backup-submit",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--upload",
        &upload,
        "--changeset",
        &Uuid::new_v4().to_string(),
        "--operations",
        manifest.to_str().unwrap(),
    ]));
    assert_eq!(accepted["state"], "accepted");
    accepted
}
fn verify(backup: &Path, digest: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["backup", "verify", "--backup"])
        .arg(backup)
        .args(["--manifest-sha256", digest])
        .output()
        .unwrap()
}
async fn snapshot(
    client: &tokio_postgres::Client,
    schema: &str,
) -> (BTreeMap<String, Value>, Value) {
    let tables = client
        .query(
            "SELECT tablename::text FROM pg_tables WHERE schemaname::text=$1 ORDER BY tablename",
            &[&schema],
        )
        .await
        .unwrap();
    let mut data = BTreeMap::new();
    for table in tables {
        let name: String = table.get(0);
        assert!(
            name.bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'_' || c.is_ascii_digit())
        );
        let value:Value=client.query_one(&format!("SELECT coalesce(jsonb_agg(to_jsonb(r) ORDER BY to_jsonb(r)::text),'[]') FROM {schema}.{name} r"),&[]).await.unwrap().get(0);
        data.insert(name, value);
    }
    let sequences:Value=client.query_one("SELECT coalesce(jsonb_agg(jsonb_build_array(sequencename,last_value) ORDER BY sequencename),'[]') FROM pg_sequences WHERE schemaname::text=$1",&[&schema]).await.unwrap().get(0);
    (data, sequences)
}
fn copy_directory(from: &Path, to: &Path) {
    fs::create_dir(to).unwrap();
    fs::set_permissions(to, fs::Permissions::from_mode(0o700)).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_directory(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL and actual matching pg_dump/pg_restore executables"]
async fn cold_backup_restores_metadata_and_native_source_and_refuses_unsafe_inputs() {
    let dump = PathBuf::from(
        std::env::var_os("TH_TEST_PG_DUMP").expect("set TH_TEST_PG_DUMP to actual pg_dump"),
    );
    let restore = PathBuf::from(
        std::env::var_os("TH_TEST_PG_RESTORE")
            .expect("set TH_TEST_PG_RESTORE to actual pg_restore"),
    );
    let mut f = Fixture::new().await;
    // Bind the empty backend BEFORE seeding objects. Production deliberately
    // refuses adoption of a pre-existing unbound catalog under a new identity.
    let identity = Uuid::new_v4();
    Database::new(f.config.clone())
        .unwrap()
        .connect(true)
        .await
        .unwrap()
        .metadata
        .bind_storage_backend("test-local", identity)
        .await
        .unwrap();
    let base = f.seed().await;
    let objects = f.dir.path().join("objects");
    let marker = identity.to_string();
    files::write_private(&objects.join("backend-id"), marker.as_bytes()).unwrap();
    f.config.storage = Some(treeharbor_service::config::StorageConfig {
        root: objects.clone(),
        backend: "test-local".into(),
    });
    f.save_config();
    let mut service = f.start().await;
    let accepted = save_proposal(&mut f, base);
    let output = f.dir.path().join("backup");
    let active = create_backup(&f, &output, &dump).output().unwrap();
    assert!(!active.status.success());
    assert!(!output.exists());
    assert!(f.cli(&["repository-info"]).status.success());
    service.0.kill().unwrap();
    service.0.wait().unwrap();

    // Existing metadata writes cannot race a dump, even with the acknowledgement.
    f.sql
        .batch_execute("BEGIN; UPDATE th_repositories SET epoch=epoch")
        .await
        .unwrap();
    let busy = create_backup(&f, &output, &dump).output().unwrap();
    assert!(!busy.status.success());
    assert!(!output.exists());
    f.sql.batch_execute("ROLLBACK").await.unwrap();
    let before = snapshot(&f.sql, &f.config.schema).await;
    let exhausted = f.dir.path().join("exhausted");
    assert!(
        !create_backup(&f, &exhausted, &dump)
            .args(["--max-bytes", "1"])
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(!exhausted.join("manifest.json").exists());
    assert!(!verify(&exhausted, &"0".repeat(64)).status.success());
    let created = create_backup(&f, &output, &dump).output().unwrap();
    if !created.status.success() {
        panic!(
            "backup failed: {}; dump: {}",
            String::from_utf8_lossy(&created.stderr),
            fs::read_to_string(output.join("pg-dump.stderr")).unwrap_or_default()
        );
    }
    let receipt: Value = serde_json::from_slice(&created.stdout).unwrap();
    assert_eq!(receipt["complete"], true);
    assert_eq!(receipt["restore_authorized"], false);
    let digest = receipt["manifest_sha256"].as_str().unwrap();
    assert!(verify(&output, digest).status.success());
    let exact_budget = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["backup", "verify", "--backup"])
        .arg(&output)
        .args([
            "--manifest-sha256",
            digest,
            "--max-bytes",
            &receipt["bytes"].as_u64().unwrap().to_string(),
            "--max-entries",
            &receipt["entries"].as_u64().unwrap().to_string(),
        ])
        .output()
        .unwrap();
    assert!(
        exact_budget.status.success(),
        "{}",
        String::from_utf8_lossy(&exact_budget.stderr)
    );
    assert_eq!(snapshot(&f.sql, &f.config.schema).await, before);
    let original_manifest = fs::read(output.join("manifest.json")).unwrap();
    assert!(
        !create_backup(&f, &output, &dump)
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        fs::read(output.join("manifest.json")).unwrap(),
        original_manifest
    );
    assert_eq!(
        fs::read(output.join("private-state/00")).unwrap(),
        fs::read(&f.credential_path).unwrap()
    );
    assert!(!String::from_utf8_lossy(&created.stdout).contains("thv1_"));

    // A missing or corrupt live object must prevent completion even though a
    // PostgreSQL dump alone would succeed.
    let inventory: Value = serde_json::from_slice(&original_manifest).unwrap();
    let object_path = inventory["inventory"]
        .as_object()
        .unwrap()
        .keys()
        .find(|name| {
            name.contains("/objects/") && inventory["inventory"][*name]["sha256"].is_string()
        })
        .unwrap();
    let original_object = objects.join(object_path.strip_prefix("objects/").unwrap());
    let object_bytes = fs::read(&original_object).unwrap();
    for missing in [false, true] {
        if missing {
            fs::remove_file(&original_object).unwrap();
        } else {
            fs::write(&original_object, b"corrupt frame").unwrap();
        }
        let rejected = f.dir.path().join(if missing {
            "missing-object"
        } else {
            "corrupt-object"
        });
        assert!(
            !create_backup(&f, &rejected, &dump)
                .output()
                .unwrap()
                .status
                .success()
        );
        assert!(!rejected.join("manifest.json").exists());
        if missing {
            files::write_private(&original_object, &object_bytes).unwrap();
        } else {
            fs::write(&original_object, &object_bytes).unwrap();
        }
    }

    // Corruption, a different manifest identity and extra/missing files are not
    // a valid backup. Restore the original fixture before the real restore.
    let private = output.join("private-state/00");
    let saved = fs::read(&private).unwrap();
    fs::write(&private, b"broken").unwrap();
    assert!(!verify(&output, digest).status.success());
    fs::write(&private, &saved).unwrap();
    assert!(!verify(&output, &"0".repeat(64)).status.success());
    files::write_private(&output.join("extra"), b"unexpected").unwrap();
    assert!(!verify(&output, digest).status.success());
    fs::remove_file(output.join("extra")).unwrap();
    fs::remove_file(&private).unwrap();
    assert!(!verify(&output, digest).status.success());
    files::write_private(&private, &saved).unwrap();
    assert!(verify(&output, digest).status.success());

    let target = format!("th_restore_{}", Uuid::new_v4().simple());
    f.sql
        .batch_execute(&format!("CREATE DATABASE {target}"))
        .await
        .unwrap();
    let pg = f.config.database_config().unwrap();
    let mut command = Command::new(restore);
    command.env("PGDATABASE", &target).env(
        "PGHOST",
        match &pg.get_hosts()[0] {
            tokio_postgres::config::Host::Unix(path) => path,
            _ => unreachable!(),
        },
    );
    if let Some(user) = pg.get_user() {
        command.env("PGUSER", user);
    }
    if let Some(password) = pg.get_password() {
        use std::os::unix::ffi::OsStrExt;
        command.env("PGPASSWORD", std::ffi::OsStr::from_bytes(password));
    }
    if let Some(port) = pg.get_ports().first() {
        command.env("PGPORT", port.to_string());
    }
    let restored = command
        .args([
            "--dbname",
            &target,
            "--no-owner",
            "--no-privileges",
            "--single-transaction",
            "--exit-on-error",
        ])
        .stdin(fs::File::open(output.join("metadata.dump")).unwrap())
        .output()
        .unwrap();
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    let mut restored_pg = pg;
    restored_pg.dbname(&target);
    let (restored_sql, connection) = restored_pg.connect(tokio_postgres::NoTls).await.unwrap();
    let driver = tokio::spawn(async move {
        let _ = connection.await;
    });
    assert_eq!(snapshot(&restored_sql, &f.config.schema).await, before);
    let recovered = f.dir.path().join("recovered-objects");
    copy_directory(&output.join("objects"), &recovered);
    fs::rename(&objects, f.dir.path().join("old-objects-unavailable")).unwrap();
    let original_url = f.config.database_url.clone();
    f.config.database_url = format!("{} dbname={target}", f.config.database_url);
    f.config.storage.as_mut().unwrap().root = recovered.clone();
    f.save_config();
    let mut recovered_service = f.start().await;
    let recovered_proposal = ok(f.cli(&[
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "change",
        "--changeset",
        accepted["changeset"].as_str().unwrap(),
        "--version",
        "1",
    ]));
    assert_eq!(
        recovered_proposal["after_snapshot"],
        accepted["result_snapshot"]
    );
    let exported = f.dir.path().join("recovered-pack");
    let result = f.cli(&[
        "source-export",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--revision",
        &base.to_string(),
        "--output",
        exported.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!objects.exists());
    recovered_service.0.kill().unwrap();
    recovered_service.0.wait().unwrap();
    driver.abort();
    drop(restored_sql);
    f.config.database_url = original_url;
    f.sql
        .batch_execute(&format!("DROP DATABASE {target} WITH (FORCE)"))
        .await
        .unwrap();
    f.cleanup().await;
}
