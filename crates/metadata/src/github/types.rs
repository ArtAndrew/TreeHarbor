use crate::{Error, Result, identifier};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Upstream {
    pub installation_id: u64,
    pub repository_id: u64,
    pub owner_id: u64,
    pub owner: String,
    pub name: String,
}
fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
fn positive(id: u64) -> bool {
    id > 0 && id <= i64::MAX as u64
}
impl Upstream {
    pub fn validate(&self) -> Result<()> {
        if ![self.installation_id, self.repository_id, self.owner_id]
            .into_iter()
            .all(positive)
            || !component(&self.owner)
            || !component(&self.name)
        {
            return Err(Error::Invalid("GitHub repository identity"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BindingSpec {
    pub version: u8,
    pub app_id: u64,
    pub client_id: String,
    pub bridge: String,
    pub upstream: Upstream,
}
impl BindingSpec {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || !positive(self.app_id)
            || self.client_id.is_empty()
            || self.client_id.len() > 128
            || !self
                .client_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_.".contains(&b))
        {
            return Err(Error::Invalid("GitHub App identity"));
        }
        identifier(&self.bridge, 128)?;
        self.upstream.validate()
    }
}
/// Exact v1 observation returned by the trusted GitHub App bridge. The database
/// authenticates the assigned reporter, not the GitHub HTTP exchange itself.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    pub version: u8,
    pub api_version: String,
    pub observed_at_unix: u64,
    pub app_id: u64,
    pub binding: Upstream,
    pub repository_node_id: String,
    pub owner_type: String,
    pub private: bool,
    pub default_branch: String,
    pub archived: bool,
    pub disabled: bool,
    pub read_connection_verified: bool,
    pub probe_token_revoked: bool,
    pub queue_eligibility: String,
    pub queue_configuration_verified: bool,
    pub publication_ready: bool,
}
impl Observation {
    pub fn validate(&self, spec: &BindingSpec) -> Result<()> {
        let queue = match (self.owner_type.as_str(), self.private) {
            ("User", _) => "personal_repository_unsupported",
            ("Organization", false) => "public_organization_eligible",
            ("Organization", true) => "private_organization_plan_required",
            _ => return Err(Error::Invalid("GitHub owner type")),
        };
        if self.version != 1
            || self.api_version != "2026-03-10"
            || self.app_id != spec.app_id
            || self.binding != spec.upstream
            || self.repository_node_id.is_empty()
            || self.repository_node_id.len() > 256
            || !self
                .repository_node_id
                .bytes()
                .all(|b| b.is_ascii_graphic())
            || self.default_branch.is_empty()
            || self.default_branch.len() > 1024
            || self.default_branch.chars().any(char::is_control)
            || !self.read_connection_verified
            || !self.probe_token_revoked
            || self.queue_configuration_verified
            || self.publication_ready
            || self.queue_eligibility != queue
            || self.observed_at_unix == 0
            || self.observed_at_unix > i64::MAX as u64
        {
            return Err(Error::Invalid("GitHub connection observation"));
        }
        Ok(())
    }
}
