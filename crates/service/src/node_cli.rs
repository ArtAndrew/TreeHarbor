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
    },
    /// Reopen an acknowledged placement without changing agent files.
    Prepared {
        #[command(flatten)]
        node: Arguments,
        #[arg(long)]
        workspace: Uuid,
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
    let (args, workspace, max_workspace_bytes) = match action {
        Action::PrepareOnce {
            node,
            max_workspace_bytes,
        } => (node, None, max_workspace_bytes),
        Action::Prepared { node, workspace } => {
            (node, Some(workspace), Limits::default().max_workspace_bytes)
        }
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
    let node = Node::open(
        &args.root,
        credential.scope.tenant(),
        auth.principal(),
        args.storage,
        args.parallelism,
    )?;
    let limits = Limits {
        max_workspace_bytes,
        ..Limits::default()
    };
    let (workspace, tree) = if let Some(workspace) = workspace {
        (
            workspace,
            node.open_prepared(&mut session.metadata, &auth, workspace, limits)
                .await?,
        )
    } else {
        let storage = config.storage.as_ref().ok_or(Error::Config)?;
        let store = ManagedStore::open(
            &storage.root,
            TenantId::new(credential.scope.tenant()).map_err(|_| Error::Config)?,
            &storage.backend,
        )
        .map_err(|_| Error::Config)?;
        let mut heartbeat = database.connect(true).await?;
        let Some(plan) = node.claim(&mut session.metadata, &auth).await? else {
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
    };
    println!(
        "{}",
        json!({"workspace":workspace.to_string(),"state":"files_ready","runtime":null,
        "path":tree.path(),"manifest":tree.manifest_path(),"snapshot":tree.snapshot().to_string(),
        "recovered_baseline":tree.statistics().is_none()})
    );
    Ok(true)
}
