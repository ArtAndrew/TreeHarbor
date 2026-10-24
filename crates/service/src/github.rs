use crate::{
    Error, Result,
    client::{self, Credential},
    files,
};
use clap::Subcommand;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;
use treeharbor_github::{AppCredentials, Connection, RepositoryBinding};
use uuid::Uuid;
mod dequeue;
mod import;
mod inspections;
pub(crate) mod projection;
mod publish;
mod pulls;
mod queue;
mod recovery;

#[derive(Subcommand)]
pub enum Action {
    /// Resume an owner-authorized bounded delivery scan and signed-inbox recovery.
    RecoverEvents {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        recovery: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory; keep it across interruption and retry.
        #[arg(long)]
        state: PathBuf,
    },
    /// Remove an explicitly authorized PR from the host queue, or recover its attempt.
    Dequeue {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        dequeue: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory, separate from the original queue journal.
        #[arg(long)]
        state: PathBuf,
        /// Original queue journal for recovering a still-unresolved enqueue.
        #[arg(long)]
        queue_state: Option<PathBuf>,
    },
    /// Deliver one owner-authorized queue request, or observe its existing attempt.
    Queue {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        queue: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory; preserve it for recovery.
        #[arg(long)]
        state: PathBuf,
    },
    /// Capture exact selected PR checks and active rulesets; does not authorize merge.
    Inspect {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        inspection: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory; retain it for receipt recovery.
        #[arg(long)]
        state: PathBuf,
    },
    /// Create/update an owner-selected PR, or reconcile its previous attempt.
    Pull {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        pull: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory; retain it across interruption and retries.
        #[arg(long)]
        state: PathBuf,
    },
    /// Publish an owner-selected projection, or reconcile its sole previous attempt.
    Publish {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        publication: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory; retain it across interruption and retries.
        #[arg(long)]
        state: PathBuf,
        #[arg(long, default_value = "/usr/bin/git")]
        git: PathBuf,
    },
    /// Prepare an owner's exact accepted ChangeSet for optional GitHub publication.
    Prepare {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        projection: Uuid,
        #[arg(long)]
        key: String,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
    },
    /// Download a ready projection as the assigned HostPublish bridge; no upstream write.
    Export {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        projection: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long)]
        output: PathBuf,
    },
    /// Download and import an owner's exact Git revision through its assigned bridge.
    Import {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        import: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        /// Existing private directory; retain it for retry after interruption.
        #[arg(long)]
        state: PathBuf,
        #[arg(long, default_value = "/usr/bin/git")]
        git: PathBuf,
    },
    /// Verify a durable binding as its assigned bridge and retain the observation.
    /// Reuse the same probe ID after interruption; no source or PR is published.
    Verify {
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long)]
        binding: Uuid,
        #[arg(long)]
        probe: Uuid,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
    },
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
fn load(path: &Path) -> Result<(AppCredentials, Config)> {
    let config: Config = serde_json::from_slice(&files::read_private(path, 16 * 1024)?)
        .map_err(|_| treeharbor_github::Error::Configuration)?;
    if config.version != 1 || !config.private_key_file.is_absolute() {
        return Err(treeharbor_github::Error::Configuration.into());
    }
    config.repository.validate()?;
    let credentials = AppCredentials::from_pem(
        config.app_id,
        config.client_id.clone(),
        files::read_private(&config.private_key_file, 16 * 1024)?,
    )?;
    Ok((credentials, config))
}
async fn command(socket: &Path, credential: &Credential, request: Value) -> Result<Value> {
    let body = credential.request(json!({"method":"github","request":request}));
    let reply = client::send(
        socket,
        &credential.token,
        serde_json::to_vec(&body).map_err(|_| Error::Http)?,
        Duration::from_secs(15),
    )
    .await?;
    if !reply.status.is_success() {
        return Err(Error::GithubBinding {
            status: reply.status.as_u16(),
        });
    }
    if reply.body.get("version") != Some(&json!(1)) {
        return Err(Error::Http);
    }
    reply.body.get("result").cloned().ok_or(Error::Http)
}
fn matches_config(config: &Config, spec: &treeharbor_metadata::github::BindingSpec) -> Result<()> {
    let upstream: treeharbor_metadata::github::Upstream =
        serde_json::from_value(json!(config.repository)).map_err(|_| Error::Http)?;
    if config.app_id != spec.app_id
        || config.client_id != spec.client_id
        || upstream != spec.upstream
    {
        return Err(treeharbor_github::Error::Identity.into());
    }
    Ok(())
}
pub async fn run(action: Action) -> Result<bool> {
    match action {
        Action::RecoverEvents {
            config,
            socket,
            credential_file,
            recovery,
            epoch,
            state,
        } => {
            recovery::run(recovery::Request {
                config,
                socket,
                credential_file,
                id: recovery,
                epoch,
                state,
            })
            .await
        }
        Action::Dequeue {
            config,
            socket,
            credential_file,
            dequeue,
            epoch,
            state,
            queue_state,
        } => {
            dequeue::run(dequeue::Request {
                config,
                socket,
                credential_file,
                id: dequeue,
                epoch,
                state,
                queue_state,
            })
            .await
        }
        Action::Queue {
            config,
            socket,
            credential_file,
            queue,
            epoch,
            state,
        } => {
            queue::run(queue::Request {
                config,
                socket,
                credential_file,
                id: queue,
                epoch,
                state,
            })
            .await
        }
        Action::Inspect {
            config,
            socket,
            credential_file,
            inspection,
            epoch,
            state,
        } => {
            inspections::run(inspections::Request {
                config,
                socket,
                credential_file,
                id: inspection,
                epoch,
                state,
            })
            .await
        }
        Action::Pull {
            config,
            socket,
            credential_file,
            pull,
            epoch,
            state,
        } => {
            pulls::run(pulls::Request {
                config,
                socket,
                credential_file,
                id: pull,
                epoch,
                state,
            })
            .await
        }
        Action::Publish {
            config,
            socket,
            credential_file,
            publication,
            epoch,
            state,
            git,
        } => {
            publish::run(publish::Request {
                config,
                socket,
                credential_file,
                id: publication,
                epoch,
                state,
                git,
            })
            .await
        }
        Action::Prepare {
            socket,
            credential_file,
            projection,
            key,
            epoch,
        } => {
            let credential = Credential::load(&credential_file)?;
            let reply = client::github_projection_prepare(
                &socket,
                &credential,
                json!({"projection":projection.to_string(),"key":key,"epoch":epoch}),
                Duration::from_secs(120),
            )
            .await?;
            println!("{}", reply.body);
            Ok(reply.status.is_success())
        }
        Action::Export {
            socket,
            credential_file,
            projection,
            epoch,
            output,
        } => {
            let credential = Credential::load(&credential_file)?;
            let context=command(&socket,&credential,json!({"action":"projection_context","epoch":epoch,"projection":projection.to_string()})).await?;
            if context["projection"] != projection.to_string()
                || context["epoch"] != epoch
                || context["state"] != "ready"
                || context["is_current_version"] != true
            {
                return Err(Error::Http);
            }
            let treeharbor_core::RevisionRef::Native(revision) = context["revision"]
                .as_str()
                .ok_or(Error::Http)?
                .parse()
                .map_err(|_| Error::Http)?
            else {
                return Err(Error::Http);
            };
            let reply = crate::archive::download_projection(
                &socket,
                &credential,
                projection,
                epoch,
                revision,
                &output,
                Duration::from_secs(120),
            )
            .await?;
            println!("{}", reply.body);
            Ok(reply.status.is_success())
        }
        Action::Import {
            config,
            socket,
            credential_file,
            import,
            epoch,
            state,
            git,
        } => {
            import::run(import::Request {
                config,
                socket,
                credential_file,
                id: import,
                epoch,
                state,
                git,
            })
            .await
        }
        Action::Verify {
            config,
            socket,
            credential_file,
            binding,
            probe,
            epoch,
        } => {
            let credential = Credential::load(&credential_file)?;
            command(&socket,&credential,json!({"action":"probe_begin","key":format!("github-probe-start-{probe}"),"epoch":epoch,"binding":binding.to_string(),"probe":probe.to_string()})).await?;
            let context=command(&socket,&credential,json!({"action":"probe_context","epoch":epoch,"binding":binding.to_string(),"probe":probe.to_string()})).await?;
            if context.get("binding") != Some(&json!(binding.to_string()))
                || context.get("probe") != Some(&json!(probe.to_string()))
                || context.get("epoch") != Some(&json!(epoch))
            {
                return Err(Error::Http);
            }
            let result = if context.get("state") == Some(&json!("observed")) {
                // A retained receipt needs no App key/network. It remains historical
                // evidence, not a fresh connection check or publication permission.
                context
            } else if context.get("state") == Some(&json!("pending"))
                && context.get("unexpired") == Some(&json!(true))
            {
                let spec: treeharbor_metadata::github::BindingSpec =
                    serde_json::from_value(context["specification"].clone())
                        .map_err(|_| Error::Http)?;
                spec.validate().map_err(|_| Error::Http)?;
                let (credentials, config) = load(&config)?;
                matches_config(&config, &spec)?;
                let observation = Connection::new(credentials)?
                    .probe(&config.repository)
                    .await?;
                command(&socket,&credential,json!({"action":"probe_finish","key":format!("github-probe-finish-{probe}"),"epoch":epoch,"binding":binding.to_string(),"probe":probe.to_string(),"observation":observation})).await?
            } else {
                return Err(Error::Http);
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&result).map_err(|_| Error::Http)?
            );
            Ok(true)
        }
        Action::Probe { config } => {
            let (credentials, config) = load(&config)?;
            let observation = Connection::new(credentials)?
                .probe(&config.repository)
                .await?;
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
