//! Submit one exact, private authority transfer request.
use super::{Credential, Error, Result, files, json};
use crate::client;
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};
use treeharbor_metadata::{commands::MAX_REQUEST_BYTES, github::ArchiveActivateSpec};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    /// Owner-private JSON with key, Connected epoch and activation specification.
    #[arg(long)]
    request: PathBuf,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    request_timeout_seconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    key: String,
    epoch: i64,
    specification: ArchiveActivateSpec,
}

pub(in crate::github) async fn run(options: Options) -> Result<bool> {
    let credential = Credential::load(&options.credential_file)?;
    let request: Request =
        serde_json::from_slice(&files::read_private(&options.request, MAX_REQUEST_BYTES)?)
            .map_err(|_| Error::PublicationState)?;
    let next = request
        .epoch
        .checked_add(1)
        .ok_or(Error::PublicationState)?;
    let reply = client::archive_operation(
        &options.socket,
        &credential,
        crate::archive_activate::PATH,
        json!({"key":request.key,"epoch":request.epoch,"specification":request.specification}),
        Duration::from_secs(options.request_timeout_seconds),
    )
    .await?;
    if reply.status.is_success() {
        let result = &reply.body["result"];
        if reply.body["version"] != 1
            || result["version"] != 1
            || result["state"] != "activated"
            || result["authority"] != "standalone"
            || result["previous_epoch"] != request.epoch
            || result["epoch"] != next
            || result["key"] != request.key
            || result["proposal_stage"] != request.specification.proposal_stage
            || result["observation_key"] != request.specification.observation_key
            || result["tenant"] != credential.scope.tenant()
            || result["repository"] != json!(credential.scope.repository())
        {
            return Err(Error::Http);
        }
    }
    println!("{}", reply.body);
    Ok(reply.status.is_success())
}
