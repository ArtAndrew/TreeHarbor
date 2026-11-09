use crate::{Error, Result, client::Credential, config::Config, database::Database, files};
use clap::{Args, Subcommand};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};
use treeharbor_materializer::{Limits, MaterializedTree};
use treeharbor_metadata::{
    Metadata,
    access::{Authorization, Permission},
};
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
    /// Prepare one queued workspace and acknowledge durable file readiness.
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
        /// Use this trusted absolute Git executable for an explicitly selected Git workspace.
        #[arg(long, requires = "workspace")]
        git: Option<PathBuf>,
        /// Local operator experiment with an exact prepared overlay placement.
        #[arg(long, hide = true, requires_all = ["workspace", "slot"])]
        cow_driver: Option<PathBuf>,
    },
    /// Local experiment with multiple guarded placements on one node.
    #[command(hide = true)]
    PrepareBatch {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        selection: PathBuf,
        #[arg(long)]
        git: Option<PathBuf>,
        #[arg(long)]
        cow_driver: PathBuf,
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
        /// Claim only this seal; unavailable work returns idle without fallback.
        #[arg(long)]
        seal: Option<Uuid>,
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
        #[arg(long, hide = true)]
        cow_driver: Option<PathBuf>,
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchPlacement {
    workspace: Uuid,
    slot: Uuid,
}

struct PreparationOptions<'a> {
    workspace: Option<Uuid>,
    slot: Option<Uuid>,
    git: Option<&'a Path>,
    cow_driver: Option<&'a Path>,
    max_workspace_bytes: u64,
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
        | Action::PrepareBatch { node, .. }
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
            cow_driver,
            ..
        } => {
            let result = if let Some(plan) = node
                .claim_cleanup(&mut session.metadata, &auth, workspace)
                .await?
            {
                let mut heartbeat = database.connect(true).await?;
                node.cleanup_workspace_with_driver(
                    &mut session.metadata,
                    &mut heartbeat.metadata,
                    &auth,
                    &plan,
                    engine_socket.as_deref(),
                    cow_driver.as_deref(),
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
        Action::SealOnce {
            engine_socket,
            seal,
            ..
        } => {
            let plan = match seal {
                Some(seal) => {
                    node.claim_seal_for(&mut session.metadata, &auth, seal)
                        .await?
                }
                None => node.claim_seal(&mut session.metadata, &auth).await?,
            };
            let Some(plan) = plan else {
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
            git,
            cow_driver,
            ..
        } => {
            let store = preparation_store(&config, &auth)?;
            let Some(result) = prepare_selected(
                &node,
                &database,
                &mut session.metadata,
                &auth,
                &store,
                PreparationOptions {
                    workspace,
                    slot,
                    git: git.as_deref(),
                    cow_driver: cow_driver.as_deref(),
                    max_workspace_bytes,
                },
            )
            .await?
            else {
                println!("{}", json!({"state":"idle"}));
                return Ok(true);
            };
            result
        }
        Action::PrepareBatch {
            node: args,
            selection,
            git,
            cow_driver,
        } => {
            let targets: Vec<BatchPlacement> =
                serde_json::from_slice(&files::read_private(&selection, 4096)?)
                    .map_err(|_| Error::Config)?;
            if !cfg!(target_os = "linux")
                || targets.is_empty()
                || targets.len() > args.parallelism.min(16)
            {
                return Err(Error::Config);
            }
            let mut workspaces = HashSet::new();
            let mut slots = HashSet::new();
            for target in &targets {
                if target.workspace.is_nil()
                    || target.slot.is_nil()
                    || !workspaces.insert(target.workspace)
                    || !slots.insert(target.slot)
                {
                    return Err(Error::Config);
                }
                node.registered_volume(target.slot)?;
            }
            let store = preparation_store(&config, &auth)?;
            drop(session);
            let mut tasks = tokio::task::JoinSet::new();
            for target in targets {
                let (node, database, auth, store, git, cow_driver) = (
                    node.clone(),
                    database.clone(),
                    auth.clone(),
                    store.clone(),
                    git.clone(),
                    cow_driver.clone(),
                );
                tasks.spawn(async move {
                    let workspace = target.workspace;
                    let prepared = async {
                        let mut session = database.connect(true).await?;
                        prepare_selected(
                            &node,
                            &database,
                            &mut session.metadata,
                            &auth,
                            &store,
                            PreparationOptions {
                                workspace: Some(workspace),
                                slot: Some(target.slot),
                                git: git.as_deref(),
                                cow_driver: Some(&cow_driver),
                                max_workspace_bytes: 256 * 1024 * 1024,
                            },
                        )
                        .await
                    }
                    .await;
                    match prepared {
                        Ok(Some((workspace, tree))) => readiness(workspace, &tree),
                        Ok(None) => json!({"workspace":workspace,"state":"idle"}),
                        Err(error) => {
                            json!({"workspace":workspace,"state":"error","error":error.to_string()})
                        }
                    }
                });
            }
            let mut results = Vec::new();
            while let Some(result) = tasks.join_next().await {
                results.push(result.map_err(|_| Error::Administration)?);
            }
            println!("{}", json!(results));
            return Ok(true);
        }
        Action::Initialize { .. } => unreachable!(),
    };
    println!("{}", readiness(workspace, &tree));
    Ok(true)
}

fn preparation_store(config: &Config, auth: &Authorization) -> Result<ManagedStore> {
    let storage = config.storage.as_ref().ok_or(Error::Config)?;
    ManagedStore::open(
        &storage.root,
        TenantId::new(auth.scope().tenant()).map_err(|_| Error::Config)?,
        &storage.backend,
    )
    .map_err(|_| Error::Config)
}

async fn prepare_selected(
    node: &Node,
    database: &Database,
    db: &mut Metadata,
    auth: &Authorization,
    store: &ManagedStore,
    options: PreparationOptions<'_>,
) -> Result<Option<(Uuid, MaterializedTree)>> {
    let PreparationOptions {
        workspace,
        slot,
        git,
        cow_driver,
        max_workspace_bytes,
    } = options;
    if cow_driver.is_some() && !cfg!(target_os = "linux") {
        return Err(Error::Config);
    }
    let max_workspace_bytes = if let Some(slot) = slot {
        max_workspace_bytes.min(node.registered_volume(slot)?.limit_bytes())
    } else {
        max_workspace_bytes
    };
    let limits = Limits {
        max_workspace_bytes,
        ..Limits::default()
    };
    let mut heartbeat = database.connect(true).await?;
    let claimed = if git.is_some() {
        node.claim_git_for(db, auth, workspace.ok_or(Error::Config)?, slot)
            .await?
    } else if let Some(workspace) = workspace {
        node.claim_for(db, auth, workspace, slot).await?
    } else if let Some(slot) = slot {
        node.claim_on_slot(db, auth, slot).await?
    } else {
        node.claim(db, auth).await?
    };
    let Some(plan) = claimed else { return Ok(None) };
    let request = Prepare {
        authorization: auth,
        plan: &plan,
        store,
        limits,
    };
    let tree = if let Some(driver) = cow_driver {
        node.prepare_cow(db, &mut heartbeat.metadata, request, git, driver)
            .await?
    } else if let Some(git) = git {
        node.prepare_git(db, &mut heartbeat.metadata, request, git)
            .await?
    } else {
        node.prepare(db, &mut heartbeat.metadata, request).await?
    };
    Ok(Some((plan.workspace(), tree)))
}

fn readiness(workspace: Uuid, tree: &MaterializedTree) -> Value {
    let mut result = json!({"workspace":workspace.to_string(),"state":"files_ready","runtime":null,
    "path":tree.path(),"manifest":tree.manifest_path(),"snapshot":tree.snapshot().to_string(),
    "recovered_baseline":tree.statistics().is_none(),
    "preparation_statistics":tree.statistics().map(|stats| json!({
        "source_objects":stats.source_objects,
        "source_encoded_bytes":stats.source_encoded_bytes,
        "source_bytes_written":stats.source_bytes_written,
        "local_bytes_copied":stats.local_bytes_copied,
        "workspace_bytes":stats.workspace_bytes,
        "filesystem_entries":stats.filesystem_entries,
        "read_batches":stats.read_batches,
        "phase_seconds":{
            "open_read_batches":stats.open_read_batches.as_secs_f64(),
            "build_batches":stats.build_batches.as_secs_f64(),
            "file_sync":stats.file_sync.as_secs_f64(),
            "volume_sync":stats.volume_sync.map(|duration| duration.as_secs_f64()),
            "finalize":stats.finalize.as_secs_f64(),
            "publish":stats.publish.as_secs_f64()
        }
    }))});
    if tree.is_external_view() {
        result["placement"] = json!("copy-on-write");
    }
    result
}
