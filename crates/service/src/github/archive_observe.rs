//! Capture a complete archive inventory for a scoped server request.
use super::{publish::journal::Journal, *};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use treeharbor_github::ArchivedRepository;
use treeharbor_metadata::github::{
    ArchiveObservation, ArchiveObservationContext, BindingSpec, HandoffTargetPlan,
};
pub(super) mod activate;
pub(super) mod preserve;
pub(super) mod proposals;
pub(super) mod retention;
pub(super) mod stage_proposals;

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long)]
    request_key: String,
    #[arg(long)]
    config: PathBuf,
    /// Existing private directory for the complete observation and its receipt.
    #[arg(long)]
    state: PathBuf,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    binding: Uuid,
    inventory: ArchivedRepository,
}

impl Capture {
    fn observation(&self, request_key: &str) -> Result<ArchiveObservation> {
        ArchiveObservation::from_inventory(self.binding, request_key, &self.inventory)
            .map_err(|_| Error::PublicationState)
    }
}

async fn capture(options: &Options, credential: &Credential) -> Result<Capture> {
    let response = command(
        &options.socket, credential,
        json!({"action":"archive_observe_context","epoch":options.epoch,"request_key":options.request_key}),
    ).await?;
    let context: ArchiveObservationContext =
        serde_json::from_value(response).map_err(|_| Error::PublicationState)?;
    let request = &context.request;
    if request.version != 1
        || request.tenant != credential.scope.tenant()
        || request.repository != credential.scope.repository()
        || request.key != options.request_key
        || request.epoch != options.epoch
        || request.binding.is_nil()
    {
        return Err(Error::PublicationState);
    }
    let inventory = read_inventory(
        &options.config,
        &context.specification,
        request.observe_after_unix,
        request.expires_at_unix,
    )
    .await?;
    Ok(Capture {
        binding: request.binding,
        inventory,
    })
}

async fn read_inventory(
    path: &Path,
    spec: &BindingSpec,
    observe_after: u64,
    expires: u64,
) -> Result<ArchivedRepository> {
    let (app, config) = load(path)?;
    matches_config(&config, spec)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::PublicationState)?
        .as_secs();
    let delay = observe_after.saturating_sub(now);
    if delay > 5 || now >= expires {
        return Err(Error::PublicationState);
    }
    if delay != 0 {
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
    Ok(Connection::new(app)?
        .observe_archived_repository(&config.repository)
        .await?)
}

pub(super) async fn run(options: Options) -> Result<bool> {
    let credential = Credential::load(&options.credential_file)?;
    let journal = Journal::open_inventory(
        &options.state,
        &json!({
            "version":1,"kind":"archive_observation","tenant":credential.scope.tenant(),
            "repository":credential.scope.repository(),"epoch":options.epoch,"request_key":options.request_key,
        }),
    )?;
    let captured = match journal.read_optional::<Capture>("inventory.json")? {
        Some(captured) => captured,
        None => {
            let captured = capture(&options, &credential).await?;
            journal.bind("inventory.json", &json!(captured))?;
            captured
        }
    };
    let observed = captured.observation(&options.request_key)?;
    let result = command(
        &options.socket,
        &credential,
        json!({
            "action":"archive_observe_record","epoch":options.epoch,"observation":observed,
        }),
    )
    .await?;
    let request = &result["request"];
    if result["version"] != 1
        || result["state"] != "observed"
        || request["tenant"] != credential.scope.tenant()
        || request["repository"] != json!(credential.scope.repository())
        || request["epoch"] != options.epoch
        || request["key"] != options.request_key
        || result["observation"] != json!(observed)
    {
        return Err(Error::PublicationState);
    }
    journal.bind("receipt.json", &result)?;
    println!("{result}");
    Ok(true)
}

async fn capture_handoff(options: &Options, credential: &Credential) -> Result<Capture> {
    let result = command(
        &options.socket,
        credential,
        json!({"action":"handoff_context","epoch":options.epoch,"plan_key":options.request_key}),
    )
    .await?;
    let plan: HandoffTargetPlan =
        serde_json::from_value(result).map_err(|_| Error::PublicationState)?;
    if plan.version != 1
        || plan.tenant != credential.scope.tenant()
        || plan.repository != credential.scope.repository()
        || plan.epoch != options.epoch
        || plan.key != options.request_key
        || plan.target.binding.is_nil()
    {
        return Err(Error::PublicationState);
    }
    let inventory = read_inventory(
        &options.config,
        &plan.target.specification,
        plan.observe_after_unix,
        plan.expires_at_unix,
    )
    .await?;
    Ok(Capture {
        binding: plan.target.binding,
        inventory,
    })
}

pub(super) async fn run_handoff(options: Options) -> Result<bool> {
    let credential = Credential::load(&options.credential_file)?;
    let journal = Journal::open_inventory(
        &options.state,
        &json!({"version":1,"kind":"github_handoff_observation","tenant":credential.scope.tenant(),
            "repository":credential.scope.repository(),"epoch":options.epoch,"plan_key":options.request_key}),
    )?;
    let captured = match journal.read_optional::<Capture>("inventory.json")? {
        Some(captured) => captured,
        None => {
            let captured = capture_handoff(&options, &credential).await?;
            journal.bind("inventory.json", &json!(captured))?;
            captured
        }
    };
    let observed = captured.observation(&options.request_key)?;
    let result = command(
        &options.socket,
        &credential,
        json!({"action":"handoff_observe","epoch":options.epoch,"observation":observed}),
    )
    .await?;
    let plan = &result["plan"];
    if result["version"] != 1
        || result["state"] != "observed"
        || result["observation"] != json!(observed)
        || plan["tenant"] != credential.scope.tenant()
        || plan["repository"] != json!(credential.scope.repository())
        || plan["epoch"] != options.epoch
        || plan["key"] != options.request_key
    {
        return Err(Error::PublicationState);
    }
    journal.bind("receipt.json", &result)?;
    println!("{result}");
    Ok(true)
}
