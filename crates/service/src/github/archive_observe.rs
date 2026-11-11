//! Capture a complete archive inventory for a scoped server request.
use super::{publish::journal::Journal, *};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    time::{SystemTime, UNIX_EPOCH},
};
use treeharbor_github::ArchivedRepository;
use treeharbor_metadata::github::{
    ArchiveInventoryDigest, ArchiveObservation, ArchiveObservationContext,
};
pub(super) mod preserve;

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
        let archive = &self.inventory.archive;
        if self.inventory.version != 1 || archive.version != 1 || !archive.token_revoked {
            return Err(Error::PublicationState);
        }
        let mut heads = BTreeMap::new();
        for head in &archive.heads {
            if head.prefix != "refs/heads/"
                || head.target.kind != "Commit"
                || heads
                    .insert(
                        format!("{}{}", head.prefix, head.name),
                        format!("git:sha1:{}", head.target.oid),
                    )
                    .is_some()
            {
                return Err(Error::PublicationState);
            }
        }
        Ok(ArchiveObservation {
            version: 1,
            request_key: request_key.to_owned(),
            binding: self.binding,
            app_id: archive.app_id,
            upstream: serde_json::from_value(json!(archive.binding))
                .map_err(|_| Error::PublicationState)?,
            repository_node_id: archive.repository_node_id.clone(),
            default_branch: archive.default_branch.clone(),
            started_at_unix: archive.started_at_unix,
            finished_at_unix: archive.finished_at_unix,
            token_revoked: archive.token_revoked,
            heads,
            tags: ArchiveInventoryDigest::tags(&self.inventory.tags)
                .map_err(|_| Error::PublicationState)?,
            open_pulls: ArchiveInventoryDigest::open_pulls(&self.inventory.open_pulls)
                .map_err(|_| Error::PublicationState)?,
        })
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
    let (app, config) = load(&options.config)?;
    matches_config(&config, &context.specification)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::PublicationState)?
        .as_secs();
    let delay = request.observe_after_unix.saturating_sub(now);
    if delay > 5 || now >= request.expires_at_unix {
        return Err(Error::PublicationState);
    }
    if delay != 0 {
        tokio::time::sleep(Duration::from_secs(delay)).await;
    }
    let inventory = Connection::new(app)?
        .observe_archived_repository(&config.repository)
        .await?;
    Ok(Capture {
        binding: request.binding,
        inventory,
    })
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
