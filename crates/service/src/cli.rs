//! Trusted local administration is deliberately separate from agent HTTP commands.
use crate::{
    Error, Result,
    client::{self, Credential},
    config::Config,
    database::Database,
    files,
    server::Server,
};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};
use std::{ffi::OsString, os::unix::ffi::OsStrExt, path::PathBuf, time::Duration};
use treeharbor_metadata::{
    Authority, Scope,
    access::{Permission, PrincipalKind},
};
use uuid::Uuid;

#[derive(Parser)]
#[command(
    name = "treeharbor",
    version,
    about = "TreeHarbor development transport (local Unix socket)"
)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Capture explicitly selected files from a stopped native source directory.
    SourcePack {
        #[arg(long)]
        directory: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long = "path", required = true)]
        paths: Vec<OsString>,
        #[arg(long)]
        author_name: String,
        #[arg(long)]
        author_email: String,
        #[arg(long, default_value = "Native source import")]
        message: String,
        #[arg(long, default_value_t = 0, allow_hyphen_values = true)]
        timestamp_seconds: i64,
        #[arg(long, default_value_t = 100_000)]
        max_objects: u64,
        #[arg(long, default_value_t = 4_294_967_296)]
        max_bytes: u64,
    },
    /// Serve authenticated commands. Requires an explicitly migrated database.
    Serve {
        #[arg(long)]
        config: PathBuf,
    },
    /// Trusted database administration; never expose this command to an agent.
    Admin {
        #[arg(long)]
        config: PathBuf,
        #[command(subcommand)]
        action: Admin,
    },
    /// Use a repository-scoped credential without database access.
    Client {
        #[arg(long)]
        socket: PathBuf,
        #[arg(long)]
        credential_file: PathBuf,
        #[arg(long, default_value_t=15_000, value_parser=clap::value_parser!(u64).range(100..=60_000))]
        timeout_ms: u64,
        #[command(subcommand)]
        action: Agent,
    },
}
#[derive(Args)]
struct Repository {
    #[arg(long)]
    tenant: String,
    #[arg(long)]
    repository: Uuid,
}
impl Repository {
    fn scope(&self) -> Result<Scope> {
        Scope::new(&self.tenant, self.repository).map_err(|_| Error::Config)
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Mode {
    Connected,
    Standalone,
}
impl From<Mode> for Authority {
    fn from(value: Mode) -> Self {
        match value {
            Mode::Connected => Self::Connected,
            Mode::Standalone => Self::Standalone,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Kind {
    Human,
    Agent,
    Node,
    Validator,
    Bridge,
}
impl From<Kind> for PrincipalKind {
    fn from(value: Kind) -> Self {
        match value {
            Kind::Human => Self::Human,
            Kind::Agent => Self::Agent,
            Kind::Node => Self::Node,
            Kind::Validator => Self::Validator,
            Kind::Bridge => Self::Bridge,
        }
    }
}
#[derive(Clone, Copy, ValueEnum)]
enum Grant {
    Read,
    #[value(name = "workspace.write")]
    WorkspaceWrite,
    #[value(name = "change.write")]
    ChangeWrite,
    #[value(name = "queue.request")]
    QueueRequest,
    #[value(name = "repository.write")]
    RepositoryWrite,
    #[value(name = "runtime.control")]
    RuntimeControl,
    #[value(name = "validation.report")]
    ValidationReport,
    #[value(name = "host.publish")]
    HostPublish,
}
impl From<Grant> for Permission {
    fn from(value: Grant) -> Self {
        match value {
            Grant::Read => Self::Read,
            Grant::WorkspaceWrite => Self::WorkspaceWrite,
            Grant::ChangeWrite => Self::ChangeWrite,
            Grant::QueueRequest => Self::QueueRequest,
            Grant::RepositoryWrite => Self::RepositoryWrite,
            Grant::RuntimeControl => Self::RuntimeControl,
            Grant::ValidationReport => Self::ValidationReport,
            Grant::HostPublish => Self::HostPublish,
        }
    }
}
#[derive(Subcommand)]
enum Admin {
    /// Install/upgrade the checked schema in the configured, existing schema.
    Migrate,
    RepositoryCreate {
        #[command(flatten)]
        scope: Repository,
        #[arg(long, value_enum)]
        authority: Mode,
    },
    PrincipalCreate {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        principal: String,
        #[arg(long, value_enum)]
        kind: Kind,
    },
    /// Replace permissions. Revoking all requires an explicit flag.
    AccessSet {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        principal: String,
        #[arg(
            long,
            value_enum,
            required_unless_present = "revoke_all",
            conflicts_with = "revoke_all"
        )]
        permission: Vec<Grant>,
        #[arg(long)]
        revoke_all: bool,
    },
    /// Write a new credential file (0600), never a secret on stdout or argv.
    TokenIssue {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        principal: String,
        #[arg(long, value_enum, required = true)]
        permission: Vec<Grant>,
        #[arg(long, default_value_t=3600, value_parser=clap::value_parser!(i32).range(1..=7_776_000))]
        ttl_seconds: i32,
        #[arg(long)]
        output: PathBuf,
    },
    TokenRevoke {
        #[command(flatten)]
        scope: Repository,
        #[arg(long)]
        token_id: Uuid,
    },
    PrincipalDisable {
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        principal: String,
    },
}
#[derive(Args)]
struct Mutation {
    /// Preserve this key on retry, including after a timeout or restart.
    #[arg(long)]
    key: String,
    #[arg(long, value_enum)]
    authority: Mode,
    #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
}
#[derive(Subcommand)]
enum Agent {
    RepositoryInfo,
    /// Import a completed source pack; keep the same state file to resume.
    SourceImport {
        #[arg(long)]
        pack: PathBuf,
        #[arg(long)]
        state: PathBuf,
        #[arg(long, value_enum)]
        authority: Mode,
        #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
        epoch: i64,
        #[arg(long,default_value_t=3600,value_parser=clap::value_parser!(i32).range(1..=3600))]
        ttl_seconds: i32,
    },
    /// Begin a resumable native source upload; does not publish a branch.
    SourceBegin {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
        #[arg(long, default_value_t = 100_000)]
        max_objects: i64,
        #[arg(long, default_value_t = 4_294_967_296)]
        max_bytes: i64,
        #[arg(long, default_value_t = 3600)]
        ttl_seconds: i32,
    },
    /// Send one canonical object frame, children first.
    SourcePut {
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        object: String,
        #[arg(long)]
        file: PathBuf,
    },
    SourceFinish {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
        #[arg(long)]
        commit: String,
    },
    SourceAbort {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        upload: Uuid,
    },
    SourceStatus {
        #[arg(long)]
        upload: Uuid,
    },
    /// Store a full or explicit sparse source-delivery profile.
    ProfilePut {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long, required_unless_present = "paths", conflicts_with = "paths")]
        full: bool,
        #[arg(long = "path", required_unless_present = "full")]
        paths: Vec<OsString>,
    },
    ProfileGet {
        #[arg(long)]
        profile_digest: String,
    },
    /// Enqueue preparation from a registered native base; does not imply READY.
    WorkspaceCreate {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        workspace: Uuid,
        #[arg(long)]
        base: String,
        #[arg(long)]
        profile_digest: String,
    },
    WorkspaceStatus {
        #[arg(long)]
        workspace: Uuid,
    },
    /// Begin draining; physical deletion needs the later runtime binding.
    WorkspaceDelete {
        #[command(flatten)]
        mutation: Mutation,
        #[arg(long)]
        workspace: Uuid,
        #[arg(long, value_parser=clap::value_parser!(i64).range(1..))]
        generation: i64,
    },
    OperationStatus {
        #[arg(long)]
        key: String,
    },
}
impl Agent {
    fn json(self) -> Result<Value> {
        Ok(match self {
            Self::RepositoryInfo => json!({"method":"repository_info"}),
            Self::SourceBegin {
                mutation,
                upload,
                max_objects,
                max_bytes,
                ttl_seconds,
            } => {
                json!({"method":"source_begin","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string(),"max_objects":max_objects,"max_bytes":max_bytes,"ttl_seconds":ttl_seconds})
            }
            Self::SourceFinish {
                mutation,
                upload,
                commit,
            } => {
                json!({"method":"source_finish","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string(),"commit":commit})
            }
            Self::SourceAbort { mutation, upload } => {
                json!({"method":"source_abort","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"upload":upload.to_string()})
            }
            Self::SourceStatus { upload } => {
                json!({"method":"source_status","upload":upload.to_string()})
            }
            Self::SourcePut { .. } | Self::SourceImport { .. } => return Err(Error::Config), // binary path below
            Self::ProfilePut {
                mutation,
                full,
                paths,
            } => {
                let profile = if full {
                    treeharbor_core::SourceProfile::full()
                } else {
                    let paths = paths
                        .into_iter()
                        .map(|path| treeharbor_core::RepoPath::new(path.as_bytes().to_vec()))
                        .collect::<treeharbor_core::Result<Vec<_>>>()
                        .map_err(|_| Error::Profile)?;
                    treeharbor_core::SourceProfile::sparse(paths).map_err(|_| Error::Profile)?
                };
                json!({"method":"source_profile_put","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"profile":treeharbor_metadata::profiles::describe(&profile)})
            }
            Self::ProfileGet { profile_digest } => {
                json!({"method":"source_profile_get","profile_digest":profile_digest})
            }
            Self::WorkspaceStatus { workspace } => {
                json!({"method":"workspace_status","workspace":workspace.to_string()})
            }
            Self::OperationStatus { key } => json!({"method":"operation_status","key":key}),
            Self::WorkspaceCreate {
                mutation,
                workspace,
                base,
                profile_digest,
            } => {
                json!({"method":"workspace_create","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"workspace":workspace.to_string(),"base":base,"profile_digest":profile_digest})
            }
            Self::WorkspaceDelete {
                mutation,
                workspace,
                generation,
            } => {
                json!({"method":"workspace_delete","key":mutation.key,"authority":Authority::from(mutation.authority).as_str(),"epoch":mutation.epoch,"workspace":workspace.to_string(),"generation":generation})
            }
        })
    }
}
pub async fn run(cli: Cli) -> Result<bool> {
    match cli.command {
        Command::SourcePack {
            directory,
            output,
            paths,
            author_name,
            author_email,
            message,
            timestamp_seconds,
            max_objects,
            max_bytes,
        } => {
            let paths = paths
                .into_iter()
                .map(|p| treeharbor_core::RepoPath::new(p.as_bytes().to_vec()))
                .collect::<treeharbor_core::Result<Vec<_>>>()
                .map_err(|_| Error::Profile)?;
            let result = tokio::task::spawn_blocking(move || {
                crate::pack::capture(crate::pack::Capture {
                    directory: &directory,
                    output: &output,
                    paths,
                    author_name,
                    author_email,
                    message,
                    timestamp_seconds,
                    limits: crate::pack::Limits {
                        objects: max_objects,
                        bytes: max_bytes,
                    },
                })
            })
            .await
            .map_err(|_| Error::ImportWorker)??;
            println!(
                "{}",
                json!({"pack":result.directory.to_str(),"pack_path_hex":crate::pack::hex(result.directory.as_os_str().as_bytes()),"commit":result.commit().to_string(),"snapshot":result.snapshot().to_string(),"objects":result.object_count(),"encoded_bytes":result.encoded_bytes(),"filesystem_entries":result.filesystem_entries()})
            );
            Ok(true)
        }
        Command::Serve { config } => {
            let config = Config::load(&config)?;
            let mut terminate =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
            let mut interrupt =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
            let server = Server::bind(config).await?;
            println!("{}", json!({"status":"listening","socket":server.socket()}));
            server
                .run(async {
                    tokio::select! { _=terminate.recv()=>{}, _=interrupt.recv()=>{} }
                })
                .await?;
            Ok(true)
        }
        Command::Client {
            socket,
            credential_file,
            timeout_ms,
            action,
        } => {
            let credential = Credential::load(&credential_file)?;
            if let Agent::SourceImport {
                pack,
                state,
                authority,
                epoch,
                ttl_seconds,
            } = action
            {
                let pack = tokio::task::spawn_blocking(move || crate::pack::Pack::open(&pack))
                    .await
                    .map_err(|_| Error::ImportWorker)??;
                let reply = crate::import::send(crate::import::Request {
                    socket: &socket,
                    credential: &credential,
                    pack,
                    state: &state,
                    authority: authority.into(),
                    epoch,
                    ttl_seconds,
                    timeout: Duration::from_millis(timeout_ms),
                })
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            if let Agent::SourcePut {
                upload,
                object,
                file,
            } = action
            {
                let id: treeharbor_core::ObjectId = object.parse().map_err(|_| Error::Config)?;
                let bytes = files::read_private(
                    &file,
                    treeharbor_core::FRAME_HEADER + treeharbor_core::MAX_PAYLOAD,
                )?;
                let reply = client::send_object(
                    &socket,
                    &credential,
                    upload,
                    id,
                    bytes,
                    Duration::from_millis(timeout_ms),
                )
                .await?;
                println!("{}", reply.body);
                return Ok(reply.status.is_success());
            }
            let body = credential.request(action.json()?).to_string().into_bytes();
            let reply = client::send(
                &socket,
                &credential.token,
                body,
                Duration::from_millis(timeout_ms),
            )
            .await?;
            println!("{}", reply.body);
            Ok(reply.status.is_success())
        }
        Command::Admin { config, action } => {
            let database = Database::new(Config::load(&config)?)?;
            let mut session = database.connect(!matches!(&action, Admin::Migrate)).await?;
            // The DB role and private config are the trust boundary; this audit
            // label records the local OS uid, not an authenticated human name.
            let operator = format!("local-uid:{}", rustix::process::geteuid().as_raw());
            let db = &mut session.metadata;
            let value = match action {
                Admin::Migrate => {
                    db.migrate().await.map_err(|_| Error::Administration)?;
                    json!({"status":"migrated"})
                }
                Admin::RepositoryCreate { scope, authority } => {
                    db.create_repository(&scope.scope()?, authority.into())
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"provisioned","repository":scope.repository.to_string()})
                }
                Admin::PrincipalCreate {
                    tenant,
                    principal,
                    kind,
                } => {
                    db.provision_principal(&tenant, &principal, kind.into(), &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"provisioned","principal":principal})
                }
                Admin::AccessSet {
                    scope,
                    principal,
                    permission,
                    ..
                } => {
                    let permissions: Vec<_> = permission.into_iter().map(Into::into).collect();
                    db.set_repository_access(&scope.scope()?, &principal, &permissions, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"access_replaced"})
                }
                Admin::TokenIssue {
                    scope,
                    principal,
                    permission,
                    ttl_seconds,
                    output,
                } => {
                    // Validate the destination before minting; atomic no-clobber
                    // publication still handles a racing creation safely.
                    files::private_directory(output.parent().ok_or(Error::UnsafeFile)?)?;
                    match std::fs::symlink_metadata(&output) {
                        Ok(_) => return Err(Error::UnsafeFile),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                        Err(error) => return Err(error.into()),
                    }
                    let scope = scope.scope()?;
                    let permissions: Vec<_> = permission.into_iter().map(Into::into).collect();
                    let issued = db
                        .issue_repository_token(
                            &scope,
                            &principal,
                            &permissions,
                            ttl_seconds,
                            &operator,
                        )
                        .await
                        .map_err(|_| Error::Administration)?;
                    let document = json!({"version":1,"tenant":scope.tenant(),"repository":scope.repository().to_string(),"token_id":issued.id.to_string(),"token":issued.secret.expose()});
                    if files::write_private(&output, document.to_string().as_bytes()).is_err() {
                        return Err(
                            if db
                                .revoke_repository_token(&scope, issued.id, &operator)
                                .await
                                .is_ok()
                            {
                                Error::CredentialWriteRevoked { token: issued.id }
                            } else {
                                Error::CredentialWriteUncertain { token: issued.id }
                            },
                        );
                    }
                    json!({"status":"issued","token_id":issued.id.to_string(),"credential_file":output})
                }
                Admin::TokenRevoke { scope, token_id } => {
                    db.revoke_repository_token(&scope.scope()?, token_id, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"revoked","token_id":token_id.to_string()})
                }
                Admin::PrincipalDisable { tenant, principal } => {
                    db.disable_principal(&tenant, &principal, &operator)
                        .await
                        .map_err(|_| Error::Administration)?;
                    json!({"status":"disabled","principal":principal})
                }
            };
            println!("{value}");
            Ok(true)
        }
    }
}
