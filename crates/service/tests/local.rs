#![cfg(unix)]
use clap::Parser;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use treeharbor_service::{Error, cli::Cli, config::Config, files};

#[test]
fn private_files_reject_aliases_exposure_and_overwrite() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("credential");
    files::write_private(&path, b"first").unwrap();
    assert_eq!(files::read_private(&path, 5).unwrap(), b"first");
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(matches!(files::read_private(&path, 4), Err(Error::Size)));
    assert!(files::write_private(&path, b"replacement").is_err());
    assert_eq!(fs::read(&path).unwrap(), b"first");
    let link = dir.path().join("alias");
    symlink(&path, &link).unwrap();
    assert!(files::read_private(&link, 100).is_err());
    fs::remove_file(&link).unwrap();
    fs::hard_link(&path, &link).unwrap();
    assert!(matches!(
        files::read_private(&path, 100),
        Err(Error::UnsafeFile)
    ));
    fs::remove_file(&link).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        files::read_private(&path, 100),
        Err(Error::UnsafeFile)
    ));
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        files::read_private(&path, 100),
        Err(Error::UnsafeFile)
    ));
}

#[test]
fn configuration_forbids_tcp_fallback_ambiguous_schema_and_unbounded_limits() {
    let mut config = Config {
        storage: None,
        database_url: "host=/tmp dbname=test".into(),
        schema: "public".into(),
        socket: "/tmp/service.sock".into(),
        pool_size: 8,
        max_connections: 64,
        request_timeout_ms: 15000,
    };
    assert!(config.database_config().is_ok());
    for value in [
        "dbname=test",
        "host=127.0.0.1",
        "host=/tmp,127.0.0.1",
        "host=/tmp hostaddr=127.0.0.1",
        "host=/tmp options='-c search_path=evil'",
        "host=relative",
    ] {
        config.database_url = value.into();
        assert!(matches!(config.database_config(), Err(Error::Config)));
    }
    config.database_url = "host=/tmp".into();
    for value in [
        "",
        "public,evil",
        "pg_temp",
        "UPPER",
        "public;drop table x",
        "9schema",
    ] {
        config.schema = value.into();
        // Temporary schemas must not shadow the durable metadata schema.
        assert!(config.database_config().is_err(), "accepted schema {value}");
    }
    config.schema = "public".into();
    config.pool_size = 0;
    assert!(config.database_config().is_err());
    config.pool_size = 8;
    config.max_connections = 1025;
    assert!(config.database_config().is_err());
    config.max_connections = 64;
    config.request_timeout_ms = 0;
    assert!(config.database_config().is_err());
}

#[test]
fn cli_requires_explicit_grants_and_mutation_fences() {
    let prefix = [
        "treeharbor",
        "admin",
        "--config",
        "config.json",
        "access-set",
        "--tenant",
        "test",
        "--repository",
        "00000000-0000-0000-0000-000000000000",
        "--principal",
        "agent",
    ];
    assert!(Cli::try_parse_from(prefix).is_err());
    assert!(Cli::try_parse_from(prefix.into_iter().chain(["--revoke-all"])).is_ok());
    assert!(Cli::try_parse_from(prefix.into_iter().chain(["--permission", "read"])).is_ok());
    assert!(
        Cli::try_parse_from(
            prefix
                .into_iter()
                .chain(["--permission", "read", "--revoke-all"])
        )
        .is_err()
    );
    assert!(
        Cli::try_parse_from([
            "treeharbor",
            "client",
            "--socket",
            "/tmp/s",
            "--credential-file",
            "c",
            "workspace-create"
        ])
        .is_err()
    );
}

#[tokio::test]
async fn host_configuration_cli_needs_no_pr_and_rejects_exposed_config_before_network() {
    let prefix = [
        "treeharbor",
        "github",
        "policy-inspect",
        "--config",
        "config.json",
        "--base",
        "main",
    ];
    assert!(Cli::try_parse_from(prefix).is_ok());
    assert!(Cli::try_parse_from(prefix.into_iter().chain(["--include-queue-settings"])).is_ok());
    assert!(Cli::try_parse_from(&prefix[..5]).is_err());
    assert!(Cli::try_parse_from(prefix.into_iter().chain(["--approve"])).is_err());
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let path = dir.path().join("app.json");
    fs::write(&path, b"synthetic private app configuration").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    let cli = Cli::try_parse_from([
        "treeharbor",
        "github",
        "policy-inspect",
        "--config",
        path.to_str().unwrap(),
        "--base",
        "main",
    ])
    .unwrap();
    assert!(matches!(
        treeharbor_service::cli::run(cli).await,
        Err(Error::UnsafeFile)
    ));
}
