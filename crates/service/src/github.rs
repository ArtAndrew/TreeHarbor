use crate::{Error, Result, files};
use clap::Subcommand;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use treeharbor_github::{AppCredentials, Connection, RepositoryBinding};

#[derive(Subcommand)]
pub enum Action {
    /// Verify a selected repository with a temporary read-only App token.
    /// Creates no binding, branch or PR and makes no queue-readiness claim.
    Probe {
        /// Private JSON file; the RSA key remains in a separate private file.
        #[arg(long)]
        config: PathBuf,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    app_id: u64,
    client_id: String,
    private_key_file: PathBuf,
    repository: RepositoryBinding,
}
fn load(path: &Path) -> Result<(AppCredentials, RepositoryBinding)> {
    let config: Config = serde_json::from_slice(&files::read_private(path, 16 * 1024)?)
        .map_err(|_| treeharbor_github::Error::Configuration)?;
    if config.version != 1 || !config.private_key_file.is_absolute() {
        return Err(treeharbor_github::Error::Configuration.into());
    }
    config.repository.validate()?;
    let credentials = AppCredentials::from_pem(
        config.app_id,
        config.client_id,
        files::read_private(&config.private_key_file, 16 * 1024)?,
    )?;
    Ok((credentials, config.repository))
}
pub async fn run(action: Action) -> Result<bool> {
    match action {
        Action::Probe { config } => {
            let (credentials, binding) = load(&config)?;
            let observation = Connection::new(credentials)?.probe(&binding).await?;
            println!(
                "{}",
                serde_json::to_string_pretty(&observation).map_err(|_| Error::Http)?
            );
            Ok(true)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn app_config_requires_private_files_and_never_accepts_inline_secrets_or_custom_hosts() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let pem = dir.path().join("app.pem");
        let path = dir.path().join("app.json");
        files::write_private(
            &pem,
            include_bytes!("../../integrations-github/tests/fixtures/test-app.pem"),
        )
        .unwrap();
        let value = serde_json::json!({
            "version":1,"app_id":7,"client_id":"Iv1_test", "private_key_file":pem,
            "repository":{"installation_id":9,"repository_id":11,"owner_id":13,"owner":"org","name":"demo"}
        });
        files::write_private(&path, &serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(load(&path).is_ok());
        for field in ["token", "private_key", "api_url"] {
            let mut bad = value.clone();
            bad[field] = "must-not-be-accepted".into();
            fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();
            assert!(load(&path).is_err());
        }
        fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        fs::set_permissions(&pem, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(load(&path), Err(Error::UnsafeFile)));
        fs::set_permissions(&pem, fs::Permissions::from_mode(0o600)).unwrap();
        let target = dir.path().join("target.pem");
        fs::rename(&pem, &target).unwrap();
        symlink(&target, &pem).unwrap();
        assert!(load(&path).is_err());
    }
}
