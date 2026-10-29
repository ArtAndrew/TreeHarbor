#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    process::Command,
};
use treeharbor_service::files;

#[test]
fn doctor_reports_independent_setup_failures_without_secrets_or_writes() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let config = dir.path().join("config.json");
    let socket = dir.path().join("service.sock");
    let storage = dir.path().join("objects");
    fs::create_dir(&storage).unwrap();
    fs::set_permissions(&storage, fs::Permissions::from_mode(0o755)).unwrap();
    // No PostgreSQL server is needed. Use a unique absent socket directory and
    // synthetic secrets to exercise actual CLI redaction on connection failure.
    let database_url = format!(
        "host={} user=doctor_secret_user password=doctor_secret_password dbname=doctor_secret_database",
        dir.path().join("absent-postgres").display()
    );
    files::write_private(
        &config,
        json!({"database_url":database_url,"socket":socket,"schema":"doctor_secret_schema",
        "storage":{"root":storage,"backend":"doctor-test"}})
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    let run = || {
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .arg("doctor")
            .arg("--config")
            .arg(&config)
            .output()
            .unwrap()
    };
    let output = run();
    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(!text.contains("doctor_secret"));
    assert!(!text.contains(dir.path().to_str().unwrap()));
    let report: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(report["passed"], false);
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "storage_directory" && c["passed"] == false)
    );
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "database_unverified")
    );
    assert_eq!(fs::read_dir(&storage).unwrap().count(), 0);
    assert!(!socket.exists());
    assert!(!socket.with_extension("sock.lock").exists());

    // An existing non-socket file must survive diagnosis unchanged.
    fs::write(&socket, b"retain-existing-file").unwrap();
    let output = run();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success());
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["code"] == "socket_path_unsafe")
    );
    assert_eq!(fs::read(&socket).unwrap(), b"retain-existing-file");

    fs::set_permissions(&config, fs::Permissions::from_mode(0o644)).unwrap();
    let output = run();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success());
    assert_eq!(report["checks"][0]["code"], "configuration_private");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o600)).unwrap();

    // A configuration alias is refused before parsing or connecting.
    let alias = dir.path().join("alias.json");
    symlink(&config, &alias).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["doctor", "--config"])
        .arg(&alias)
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(!output.status.success());
    assert_eq!(report["checks"].as_array().unwrap().len(), 1);
    assert_eq!(report["checks"][0]["code"], "configuration_unreadable");
}
