use crate::{Error, OwnerType, RepositoryBinding, Result, auth::Secret};
use serde::Deserialize;
use std::collections::BTreeMap;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

#[derive(Clone, Copy)]
pub(crate) enum Capability {
    Source,
    Branch,
    PullRead,
    PullWrite,
    Evidence,
    QueueRead,
    QueueWrite,
    QueueResult,
}
impl Capability {
    pub fn permissions(self) -> BTreeMap<String, String> {
        let mut permissions = BTreeMap::from([
            (
                "contents".into(),
                match self {
                    Self::Source
                    | Self::PullRead
                    | Self::PullWrite
                    | Self::Evidence
                    | Self::QueueRead => "read",
                    Self::Branch | Self::QueueWrite | Self::QueueResult => "write",
                }
                .into(),
            ),
            ("metadata".into(), "read".into()),
        ]);
        if matches!(
            self,
            Self::PullRead | Self::PullWrite | Self::Evidence | Self::QueueRead | Self::QueueWrite
        ) {
            permissions.insert(
                "pull_requests".into(),
                if matches!(self, Self::PullWrite) {
                    "write"
                } else {
                    "read"
                }
                .into(),
            );
        }
        if matches!(self, Self::Evidence | Self::QueueRead | Self::QueueWrite) {
            permissions.insert("checks".into(), "read".into());
        }
        if matches!(self, Self::QueueRead | Self::QueueWrite) {
            permissions.insert("merge_queues".into(), "read".into());
        }
        permissions
    }
}

// GitHub can add fields. Required identity/security fields must not default.
#[derive(Deserialize)]
pub(crate) struct Account {
    pub id: u64,
    pub login: String,
    #[serde(rename = "type")]
    pub kind: OwnerType,
}
#[derive(Deserialize)]
pub(crate) struct Installation {
    pub id: u64,
    pub app_id: u64,
    pub account: Account,
    pub suspended_at: serde_json::Value,
}
impl Installation {
    pub fn check(&self, binding: &RepositoryBinding, app_id: u64) -> Result<()> {
        if self.id != binding.installation_id
            || self.app_id != app_id
            || self.account.id != binding.owner_id
            || !self.account.login.eq_ignore_ascii_case(&binding.owner)
        {
            return Err(Error::Identity);
        }
        if !self.suspended_at.is_null() {
            return Err(Error::Suspended);
        }
        Ok(())
    }
}
#[derive(Deserialize)]
pub(crate) struct Repository {
    pub id: u64,
    pub node_id: String,
    pub name: String,
    pub owner: Account,
    pub private: bool,
    pub default_branch: String,
    pub archived: bool,
    pub disabled: bool,
}
impl Repository {
    pub fn check(&self, binding: &RepositoryBinding, owner_type: OwnerType) -> Result<()> {
        if self.id != binding.repository_id
            || self.owner.id != binding.owner_id
            || !self.owner.login.eq_ignore_ascii_case(&binding.owner)
            || !self.name.eq_ignore_ascii_case(&binding.name)
            || self.owner.kind != owner_type
        {
            return Err(Error::Identity);
        }
        if self.node_id.is_empty()
            || self.node_id.len() > 256
            || !self.node_id.is_ascii()
            || self.node_id.bytes().any(|b| b.is_ascii_control())
            || self.default_branch.is_empty()
            || self.default_branch.len() > 1024
            || self.default_branch.chars().any(char::is_control)
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
}
#[derive(Deserialize)]
pub(crate) struct RepositoryId {
    pub id: u64,
}
#[derive(Deserialize)]
pub(crate) struct Token {
    pub token: Secret,
    pub expires_at: String,
    pub permissions: BTreeMap<String, String>,
    pub repositories: Option<Vec<RepositoryId>>,
}
impl Token {
    pub fn check(&self, repository: u64, now: u64) -> Result<u64> {
        self.check_for(repository, now, Capability::Source)
    }
    pub fn check_for(&self, repository: u64, now: u64, capability: Capability) -> Result<u64> {
        let expected = capability.permissions();
        if self.permissions != expected
            || self
                .repositories
                .as_ref()
                .is_some_and(|r| r.len() != 1 || r[0].id != repository)
        {
            return Err(Error::TokenScope);
        }
        let expiry = OffsetDateTime::parse(&self.expires_at, &Rfc3339)
            .ok()
            .and_then(|t| u64::try_from(t.unix_timestamp()).ok())
            .ok_or(Error::TokenLifetime)?;
        if expiry <= now.saturating_add(60) || expiry > now.saturating_add(3660) {
            return Err(Error::TokenLifetime);
        }
        self.token.header()?;
        Ok(expiry)
    }
}
#[derive(Deserialize)]
pub(crate) struct TokenRepositories {
    pub total_count: u64,
    pub repositories: Vec<RepositoryId>,
}
impl TokenRepositories {
    pub fn check(&self, repository: u64) -> Result<()> {
        if self.total_count != 1
            || self.repositories.len() != 1
            || self.repositories[0].id != repository
        {
            return Err(Error::TokenScope);
        }
        Ok(())
    }
}
