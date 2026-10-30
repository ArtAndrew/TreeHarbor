//! Local operator setup only. No database, credentials, repositories or processes.
use crate::{Error, Result, config::Config, files};
use clap::Args;
use serde_json::{Value, json};
use std::{
    fs::{DirBuilder, File},
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Component, PathBuf},
};

#[derive(Args)]
pub(crate) struct Options {
    /// New absolute directory under an existing owner-private parent. Never reused.
    #[arg(long)]
    directory: PathBuf,
    /// Existing private UTF-8 file containing the local PostgreSQL connection string.
    #[arg(long)]
    database_url_file: PathBuf,
    #[arg(long, default_value = "public")]
    schema: String,
    #[arg(long, default_value = "local")]
    backend: String,
}

pub(crate) fn create(options: Options) -> Result<Value> {
    // Never include connection strings, input file contents or raw IO errors.
    create_inner(options).map_err(|_| Error::InitState)
}

fn create_inner(options: Options) -> Result<Value> {
    let destination = &options.directory;
    if !destination.is_absolute()
        || !matches!(
            destination.components().next_back(),
            Some(Component::Normal(_))
        )
    {
        return Err(Error::InitState);
    }
    let parent = files::private_directory(destination.parent().ok_or(Error::InitState)?)?;
    let root = parent.join(destination.file_name().ok_or(Error::InitState)?);
    root.to_str().ok_or(Error::InitState)?;
    let socket = root.join("service.sock");
    // Leave room for the terminator on both supported Unix sockaddr_un layouts.
    if socket.as_os_str().as_bytes().len() > 100 {
        return Err(Error::InitState);
    }
    let database_url = String::from_utf8(files::read_private(&options.database_url_file, 8192)?)
        .map_err(|_| Error::InitState)?;
    let value = json!({
        "database_url": database_url.trim_end_matches(['\r', '\n']),
        "schema": options.schema,
        "socket": socket,
        "storage": {"root": root.join("objects"), "backend": options.backend},
    });
    let config: Config = serde_json::from_value(value.clone()).map_err(|_| Error::InitState)?;
    config.database_config()?;
    let bytes = serde_json::to_vec_pretty(&value).map_err(|_| Error::InitState)?;
    if bytes.len() > 16 * 1024 {
        return Err(Error::InitState);
    }

    // mkdir is the exclusive claim. Even a complete prior directory is refused.
    // The operator controls this parent and its ancestors; same-uid adversaries
    // are outside the existing private configuration trust boundary.
    DirBuilder::new().mode(0o700).create(&root)?;
    File::open(&parent)?.sync_all()?;
    for name in ["objects", "private"] {
        let path = root.join(name);
        DirBuilder::new().mode(0o700).create(&path)?;
        File::open(&path)?.sync_all()?;
    }
    File::open(&root)?.sync_all()?;
    let config_path = root.join("service.json");
    files::write_private(&config_path, &bytes)?;
    Ok(json!({
        "version": 1, "scope": "local-service-files", "status": "created",
        "config": config_path, "socket": socket,
        "storage": root.join("objects"), "private_state": root.join("private"),
        "database_initialized": false, "repositories_created": false,
        "credentials_issued": false, "service_started": false,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    fn options(root: &std::path::Path, name: &str) -> Options {
        Options {
            directory: root.join(name),
            database_url_file: root.join("database.url"),
            schema: "public".into(),
            backend: "local".into(),
        }
    }

    #[test]
    fn private_setup_refuses_reuse_and_invalid_inputs_without_disclosing_connection() {
        let temp = tempfile::Builder::new()
            .prefix("thi-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let parent = temp.path().canonicalize().unwrap();
        let input = parent.join("database.url");
        let connection = b"host=/not-running-postgres dbname=example password='private-password'\n";
        files::write_private(&input, connection).unwrap();
        let report = create(options(&parent, "instance")).unwrap();
        assert!(!report.to_string().contains("private-password"));
        assert!(!report.to_string().contains("not-running-postgres"));
        let root = parent.join("instance");
        let config_path = root.join("service.json");
        let config = Config::load(&config_path).unwrap();
        assert_eq!(
            config.database_url,
            String::from_utf8(connection.to_vec()).unwrap().trim()
        );
        assert_eq!(config.storage.unwrap().root, root.join("objects"));
        assert!(!config.socket.exists());
        for p in [&root, &root.join("objects"), &root.join("private")] {
            assert_eq!(fs::metadata(p).unwrap().permissions().mode() & 0o777, 0o700);
        }
        assert_eq!(
            fs::metadata(&config_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let original = fs::read(&config_path).unwrap();
        fs::write(root.join("private/retained"), b"keep").unwrap();
        assert!(create(options(&parent, "instance")).is_err());
        assert_eq!(fs::read(&config_path).unwrap(), original);
        assert_eq!(fs::read(root.join("private/retained")).unwrap(), b"keep");
        fs::create_dir(parent.join("partial")).unwrap();
        assert!(create(options(&parent, "partial")).is_err());
        assert_eq!(fs::read_dir(parent.join("partial")).unwrap().count(), 0);
        symlink(&root, parent.join("alias")).unwrap();
        assert!(create(options(&parent, "alias")).is_err());
        fs::write(
            &input,
            b"host=untrusted.example password='private-password'",
        )
        .unwrap();
        let error = create(options(&parent, "invalid")).unwrap_err();
        assert!(!error.to_string().contains("private-password"));
        assert!(!parent.join("invalid").exists());
        fs::write(&input, connection).unwrap();
        fs::set_permissions(&input, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(create(options(&parent, "public-input")).is_err());
        assert!(!parent.join("public-input").exists());
        fs::set_permissions(&input, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&input, parent.join("input-link")).unwrap();
        let mut args = options(&parent, "linked-input");
        args.database_url_file = parent.join("input-link");
        assert!(create(args).is_err());
        assert!(!parent.join("linked-input").exists());
        assert!(create(options(&parent, &"x".repeat(100))).is_err());
        assert!(!parent.join("x".repeat(100)).exists());
    }

    #[test]
    fn concurrent_initializers_leave_one_complete_configuration() {
        let temp = tempfile::Builder::new()
            .prefix("thi-")
            .tempdir_in("/tmp")
            .unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let root = temp.path().canonicalize().unwrap();
        files::write_private(
            &root.join("database.url"),
            b"host=/not-running-postgres dbname=example",
        )
        .unwrap();
        let barrier = std::sync::Barrier::new(2);
        let results = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                barrier.wait();
                create(options(&root, "instance"))
            });
            let second = scope.spawn(|| {
                barrier.wait();
                create(options(&root, "instance"))
            });
            [first.join().unwrap(), second.join().unwrap()]
        });
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        Config::load(&root.join("instance/service.json")).unwrap();
        assert_eq!(fs::read_dir(root.join("instance")).unwrap().count(), 3);
    }
}
