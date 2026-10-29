//! Bounded operator diagnostics. Never open a storage backend, bind a socket,
//! migrate, or format an underlying error: those can mutate state or leak secrets.
use crate::{Error, config::Config, database::Database, files};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::Path,
};

fn check(name: &str, passed: bool, code: &str, advice: &str) -> Value {
    json!({"name":name,"passed":passed,"code":code,"advice":advice})
}

fn report(checks: Vec<Value>) -> Value {
    let passed = checks.iter().all(|check| check["passed"] == true);
    json!({"version":1,"scope":"local-source-service-prerequisites","passed":passed,
        "checks":checks,
        "not_checked":["filesystem write, fsync and capacity","storage backend identity binding and object integrity",
            "running service health and socket lock","runtime and process isolation","Git and GitHub permissions",
            "backup and restore"]})
}

pub(crate) async fn inspect(path: &Path) -> Value {
    let config = match Config::load(path) {
        Ok(config) => config,
        Err(error) => {
            let (code, advice) = match error {
                Error::UnsafeFile => (
                    "configuration_private",
                    "Use an owner-private directory and a regular, single-link mode-0600 configuration file owned by this user.",
                ),
                Error::Io(_) => (
                    "configuration_unreadable",
                    "Check that the private configuration file and its parent directory exist and are accessible.",
                ),
                _ => (
                    "configuration_invalid",
                    "Check configuration JSON, local Unix PostgreSQL connection, absolute paths and bounded limits.",
                ),
            };
            return report(vec![check("configuration", false, code, advice)]);
        }
    };
    let mut checks = vec![check(
        "configuration",
        true,
        "configuration_valid",
        "Configuration passed the service's private-file and format checks.",
    )];
    let socket_directory = config
        .socket
        .parent()
        .is_some_and(|parent| files::private_directory(parent).is_ok());
    checks.push(check(
        "socket_directory",
        socket_directory,
        if socket_directory {
            "directory_private"
        } else {
            "directory_unavailable"
        },
        "The socket parent must exist, be owned by this user, be private and not be a symlink.",
    ));
    let socket_ok = match fs::symlink_metadata(&config.socket) {
        Ok(meta) => {
            meta.file_type().is_socket()
                && meta.uid() == rustix::process::geteuid().as_raw()
                && meta.mode() & 0o077 == 0
        }
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    };
    checks.push(check("socket_path", socket_ok,
        if socket_ok { "socket_path_acceptable" } else { "socket_path_unsafe" },
        "Use an absent socket path or an owner-private Unix socket; inspect existing files manually. Doctor never removes them or proves a listener is healthy."));
    match &config.storage {
        Some(storage) => {
            let private = files::private_directory(&storage.root).is_ok();
            checks.push(check("storage_directory", private,
                if private { "directory_private" } else { "directory_unavailable" },
                "The configured source storage directory must exist, be private, owned by this user and not be a symlink. Actual writes and backend identity are checked separately."));
        }
        None => checks.push(check("storage_directory", false, "storage_not_configured",
            "Configure durable source storage for workspace and PR workflows. Metadata-only service configurations do not satisfy this diagnostic's scope.")),
    }
    // The normal connection path checks namespace resolution and fsync=on.
    // It changes session settings only. Keep schema validation separate so an
    // operator can distinguish a missing namespace from unapplied migrations.
    let database = match Database::new(config.clone()) {
        Ok(database) => database,
        Err(_) => {
            return report(vec![check(
                "configuration",
                false,
                "configuration_invalid",
                "Check the service configuration.",
            )]);
        }
    };
    match tokio::time::timeout(config.timeout(), database.connect(false)).await {
        Ok(Ok(session)) => {
            checks.push(check("database", true, "database_connected", "Local PostgreSQL connection, namespace resolution and fsync=on checks passed."));
            let valid = matches!(tokio::time::timeout(config.timeout(), session.metadata.check_schema()).await, Ok(Ok(())));
            checks.push(check("schema", valid,
                if valid { "schema_current" } else { "schema_unverified" },
                if valid { "Schema version and migration digest match this binary." } else {
                    "Check database permissions and the installed version. After backup, apply supported migrations explicitly with admin migrate; doctor never applies them."
                }));
        }
        Ok(Err(Error::SchemaUnavailable)) => checks.push(check("database", false, "schema_unavailable",
            "Ensure the configured PostgreSQL schema exists and this role has USAGE. Schema creation and migration must be explicit.")),
        _ => checks.push(check("database", false, "database_unverified",
            "Check PostgreSQL availability, Unix socket access, authentication, configured schema and fsync=on. Connection details are intentionally omitted.")),
    }
    report(checks)
}
