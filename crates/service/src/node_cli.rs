use crate::{Error, Result, client::Credential, config::Config, database::Database};
use clap::{Args, Subcommand};
use serde_json::json;
use std::path::PathBuf;
use treeharbor_materializer::Limits;
use treeharbor_metadata::access::Permission;
use treeharbor_node::{Node, Prepare};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

#[derive(Subcommand)]
pub enum Action {
    /// Create a new private storage identity; never reuse an existing path.
    Initialize {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        tenant: String,
        #[arg(long)]
        principal: String,
    },
    /// Prepare one queued native workspace and acknowledge durable file readiness.
    PrepareOnce {
        #[command(flatten)]
        node: Arguments,
        #[arg(long, default_value_t=4_294_967_296, value_parser=clap::value_parser!(u64).range(1..=68_719_476_736))]
        max_workspace_bytes: u64,
        /// Use an operator-registered bounded volume for a future sandbox run.
        #[arg(long)]
        slot: Option<Uuid>,
        /// Claim only this workspace; unavailable work returns idle without fallback.
        #[arg(long)]
        workspace: Option<Uuid>,
    },
    /// Reopen an acknowledged placement without changing agent files.
    Prepared {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        workspace: Uuid,
    },
    /// Register an empty placement on an existing bounded ext4 mount (Linux).
    VolumeRegister {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        slot: Uuid,
        #[arg(long)]
        mount: PathBuf,
        #[arg(long)]
        placement: PathBuf,
        #[arg(long, value_parser=clap::value_parser!(u64).range(67_108_864..=68_719_476_736))]
        limit_bytes: u64,
    },
    /// Capture one owner-requested, removed managed run and publish its snapshot.
    SealOnce {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        engine_socket: PathBuf,
    },
    /// Delete an explicitly drained workspace after proving managed runtime removal.
    CleanupOnce {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        workspace: Uuid,
        /// Required if the workspace has a managed execution journal.
        #[arg(long)]
        engine_socket: Option<PathBuf>,
    },
    /// Execute or recover one persisted run using a trusted local Docker/runsc.
    RunOnce {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        engine_socket: PathBuf,
        /// Claim only this execution; unavailable work returns idle without fallback.
        #[arg(long)]
        run: Option<Uuid>,
    },
}
#[derive(Args)]
pub struct Arguments {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    storage: Uuid,
    #[arg(long, default_value_t = 4)]
    parallelism: usize,
}
pub async fn run(action: Action) -> Result<bool> {
    if let Action::Initialize {
        root,
        tenant,
        principal,
    } = action
    {
        let node = Node::create(&root, &tenant, &principal, 4)?;
        println!(
            "{}",
            serde_json::to_string(node.identity()).map_err(|_| Error::Config)?
        );
        return Ok(true);
    }
    let args = match &action {
        Action::PrepareOnce { node, .. }
        | Action::Prepared { node, .. }
        | Action::VolumeRegister { node, .. }
        | Action::RunOnce { node, .. }
        | Action::CleanupOnce { node, .. }
        | Action::SealOnce { node, .. } => node,
        Action::Initialize { .. } => unreachable!(),
    };
    let config = Config::load(&args.config)?;
    let credential = Credential::load(&args.credential_file)?;
    let database = Database::new(config.clone())?;
    let mut session = database.connect(true).await?;
    let auth = session
        .metadata
        .authorize(
            &credential.token,
            &credential.scope,
            Permission::RuntimeControl,
        )
        .await
        .map_err(|_| Error::Credential)?;
    // A completed receipt is an authenticated metadata read. Do not acquire the
    // node root or old volume: a successor may already be executing there.
    if let Action::CleanupOnce { workspace, .. } = &action
        && let Some(result) = session
            .metadata
            .completed_workspace_cleanup(&auth, *workspace, args.storage)
            .await
            .map_err(|_| Error::Administration)?
    {
        println!("{result}");
        return Ok(true);
    }
    let node = Node::open(
        &args.root,
        credential.scope.tenant(),
        auth.principal(),
        args.storage,
        args.parallelism,
    )?;
    let (workspace, tree) = match action {
        Action::CleanupOnce {
            workspace,
            engine_socket,
            ..
        } => {
            let result = if let Some(plan) = node
                .claim_cleanup(&mut session.metadata, &auth, workspace)
                .await?
            {
                let mut heartbeat = database.connect(true).await?;
                node.cleanup_workspace(
                    &mut session.metadata,
                    &mut heartbeat.metadata,
                    &auth,
                    &plan,
                    engine_socket.as_deref(),
                )
                .await?
            } else {
                session
                    .metadata
                    .completed_workspace_cleanup(&auth, workspace, node.identity().storage())
                    .await
                    .map_err(|_| Error::Administration)?
                    .unwrap_or_else(|| json!({"workspace":workspace.to_string(),"state":"busy"}))
            };
            println!("{result}");
            return Ok(true);
        }
        Action::VolumeRegister {
            slot,
            mount,
            placement,
            limit_bytes,
            ..
        } => {
            let registration = node.register_volume(slot, &mount, &placement, limit_bytes)?;
            session
                .metadata
                .register_runtime_volume(
                    credential.scope.tenant(),
                    auth.principal(),
                    &treeharbor_metadata::runs::RuntimeVolume {
                        slot: treeharbor_metadata::preparations::StorageSlot {
                            storage: registration.storage(),
                            slot,
                        },
                        digest: registration.digest()?,
                        limit_bytes,
                    },
                    &format!("local-uid:{}", rustix::process::geteuid().as_raw()),
                )
                .await
                .map_err(|_| Error::Administration)?;
            println!(
                "{}",
                json!({"state":"registered","slot":slot.to_string(),"volume_digest":treeharbor_core::ObjectId::from_bytes(registration.digest()?).to_string()})
            );
            return Ok(true);
        }
        Action::SealOnce { engine_socket, .. } => {
            let Some(plan) = node.claim_seal(&mut session.metadata, &auth).await? else {
                println!("{}", json!({"state":"idle"}));
                return Ok(true);
            };
            let storage = config.storage.as_ref().ok_or(Error::Config)?;
            let store = ManagedStore::open(
                &storage.root,
                TenantId::new(credential.scope.tenant()).map_err(|_| Error::Config)?,
                &storage.backend,
            )
            .map_err(|_| Error::Config)?;
            let mut heartbeat = database.connect(true).await?;
            let limits = Limits {
                max_workspace_bytes: plan.context().profile().definition().workspace_bytes,
                ..Limits::default()
            };
            let result = node
                .seal_workspace(
                    &mut session.metadata,
                    &mut heartbeat.metadata,
                    &auth,
                    &plan,
                    &engine_socket,
                    &store,
                    limits,
                )
                .await?;
            println!("{result}");
            return Ok(true);
        }
        Action::RunOnce {
            engine_socket, run, ..
        } => {
            let plan = match run {
                Some(run) => {
                    node.claim_run_for(&mut session.metadata, &auth, run)
                        .await?
                }
                None => node.claim_run(&mut session.metadata, &auth).await?,
            };
            let Some(plan) = plan else {
                println!("{}", json!({"state":"idle"}));
                return Ok(true);
            };
            let mut heartbeat = database.connect(true).await?;
            let limits = Limits {
                max_workspace_bytes: plan.profile().definition().workspace_bytes,
                ..Limits::default()
            };
            let result = node
                .execute_run(
                    &mut session.metadata,
                    &mut heartbeat.metadata,
                    &auth,
                    &plan,
                    &engine_socket,
                    limits,
                )
                .await?;
            println!("{result}");
            return Ok(true);
        }
        Action::Prepared { workspace, .. } => (
            workspace,
            node.open_prepared(&mut session.metadata, &auth, workspace, Limits::default())
                .await?,
        ),
        Action::PrepareOnce {
            max_workspace_bytes,
            slot,
            workspace,
            ..
        } => {
            let max_workspace_bytes = if let Some(slot) = slot {
                max_workspace_bytes.min(node.registered_volume(slot)?.limit_bytes())
            } else {
                max_workspace_bytes
            };
            let limits = Limits {
                max_workspace_bytes,
                ..Limits::default()
            };
            let storage = config.storage.as_ref().ok_or(Error::Config)?;
            let store = ManagedStore::open(
                &storage.root,
                TenantId::new(credential.scope.tenant()).map_err(|_| Error::Config)?,
                &storage.backend,
            )
            .map_err(|_| Error::Config)?;
            let mut heartbeat = database.connect(true).await?;
            let claimed = if let Some(workspace) = workspace {
                node.claim_for(&mut session.metadata, &auth, workspace, slot)
                    .await?
            } else if let Some(slot) = slot {
                node.claim_on_slot(&mut session.metadata, &auth, slot)
                    .await?
            } else {
                node.claim(&mut session.metadata, &auth).await?
            };
            let Some(plan) = claimed else {
                println!("{}", json!({"state":"idle"}));
                return Ok(true);
            };
            let tree = node
                .prepare(
                    &mut session.metadata,
                    &mut heartbeat.metadata,
                    Prepare {
                        authorization: &auth,
                        plan: &plan,
                        store: &store,
                        limits,
                    },
                )
                .await?;
            (plan.workspace(), tree)
        }
        Action::Initialize { .. } => unreachable!(),
    };
    println!(
        "{}",
        json!({"workspace":workspace.to_string(),"state":"files_ready","runtime":null,
        "path":tree.path(),"manifest":tree.manifest_path(),"snapshot":tree.snapshot().to_string(),
        "recovered_baseline":tree.statistics().is_none()})
    );
    Ok(true)
}
