//! GitHub App authentication for the isolated Connected bridge.
#![forbid(unsafe_code)]

mod auth;
mod transport;
mod wire;

pub use auth::AppCredentials;
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
pub use transport::Connection;

pub const API_VERSION: &str = "2026-03-10";

/// Operator-pinned identity. A rename requires an explicit configuration update;
/// a repository or owner replacement must never silently inherit this binding.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RepositoryBinding {
    pub installation_id: u64,
    pub repository_id: u64,
    pub owner_id: u64,
    pub owner: String,
    pub name: String,
}
impl RepositoryBinding {
    pub fn validate(&self) -> Result<()> {
        if [self.installation_id, self.repository_id, self.owner_id].contains(&0)
            || !component(&self.owner, 100)
            || !component(&self.name, 100)
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}
fn component(value: &str, max: usize) -> bool {
    !value.is_empty()
        && value.len() <= max
        && value != "."
        && value != ".."
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

/// This is an observation, not stored authority, a publication grant or proof
/// that branch rules and merge_group checks have been configured.
#[derive(Debug, Serialize)]
pub struct Probe {
    pub version: u8,
    pub api_version: &'static str,
    pub observed_at_unix: u64,
    pub app_id: u64,
    pub binding: RepositoryBinding,
    pub repository_node_id: String,
    pub owner_type: OwnerType,
    pub private: bool,
    pub default_branch: String,
    pub archived: bool,
    pub disabled: bool,
    pub read_connection_verified: bool,
    pub probe_token_revoked: bool,
    pub queue_eligibility: QueueEligibility,
    pub queue_configuration_verified: bool,
    pub publication_ready: bool,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum OwnerType {
    User,
    Organization,
}
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QueueEligibility {
    PersonalRepositoryUnsupported,
    PublicOrganizationEligible,
    PrivateOrganizationPlanRequired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Installation,
    Token,
    TokenRepositories,
    Repository,
    RevokeToken,
}
/// Never retains response bodies, URLs, private keys, tokens or upstream errors.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("invalid GitHub App configuration or pinned repository identity")]
    Configuration,
    #[error("invalid GitHub App RSA key or JWT signing failure")]
    Signing,
    #[error("system clock cannot safely issue GitHub App credentials")]
    Clock,
    #[error("GitHub request deadline exceeded; no automatic retry was attempted")]
    Deadline,
    #[error("GitHub connection failed at {stage:?}; outcome may be unknown")]
    Transport { stage: Stage },
    #[error(
        "GitHub returned HTTP {status} at {stage:?}; retry_after_seconds={retry_after_seconds:?}"
    )]
    Http {
        stage: Stage,
        status: u16,
        retry_after_seconds: Option<u64>,
    },
    #[error("GitHub response at {stage:?} exceeded its size limit")]
    Size { stage: Stage },
    #[error("invalid or unsupported GitHub response at {stage:?}")]
    Response { stage: Stage },
    #[error("GitHub installation, repository or owner differs from the pinned identity")]
    Identity,
    #[error("GitHub App installation is suspended")]
    Suspended,
    #[error(
        "GitHub installation token is not restricted to the requested repository and read permissions"
    )]
    TokenScope,
    #[error("GitHub installation token has insufficient or invalid remaining lifetime")]
    TokenLifetime,
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn now() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|_| Error::Clock)
}
