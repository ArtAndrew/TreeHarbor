//! Historical check metadata captured during restore, not validation authority.
use super::*;
use crate::check_publication::{Conclusion, Target};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckIdentity {
    pub id: u64,
    pub node_id: String,
}
impl CheckIdentity {
    pub fn validate(&self) -> Result<()> {
        positive(self.id)?;
        text(&self.node_id, 256)?;
        if !self.node_id.is_ascii() || self.node_id.bytes().any(|b| b.is_ascii_whitespace()) {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckSnapshot {
    pub identity: CheckIdentity,
    pub app_id: u64,
    pub name: String,
    pub head_revision: String,
    pub external_id: Option<String>,
    pub status: String,
    pub conclusion: Option<String>,
}
impl CheckSnapshot {
    pub fn validate(&self, identity: &CheckIdentity) -> Result<()> {
        self.identity.validate()?;
        positive(self.app_id)?;
        text(&self.name, 256)?;
        revision(&self.head_revision)?;
        text(&self.status, 32)?;
        if self
            .external_id
            .as_ref()
            .is_some_and(|s| s.len() > 1024 || s.chars().any(char::is_control))
        {
            return Err(InvalidEvidence);
        }
        if let Some(s) = &self.conclusion {
            text(s, 32)?;
        }
        if self.identity != *identity {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn matches(&self, t: &Target) -> Result<bool> {
        t.validate()?;
        self.validate(&self.identity)?;
        Ok(self.app_id == t.app_id
            && self.name == t.name()?
            && self.head_revision == t.source.head()?
            && self.external_id.as_deref() == Some(t.external_id()?.as_str())
            && self.status == "completed"
            && serde_json::from_value::<Conclusion>(serde_json::json!(self.conclusion)).ok()
                == Some(t.conclusion))
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreCheckReport {
    pub version: u8,
    pub app_id: u64,
    pub installation_id: u64,
    pub repository_id: u64,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: CheckSnapshot,
    pub after: CheckSnapshot,
    pub token_revoked: bool,
}
impl RestoreCheckReport {
    pub fn validate(&self, t: &Target, identity: &CheckIdentity) -> Result<()> {
        t.validate()?;
        positive(self.installation_id)?;
        self.before.validate(identity)?;
        self.after.validate(identity)?;
        if self.version != 1
            || self.app_id != t.app_id
            || self.repository_id != t.source.pull().repository_id
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 120_000
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn classification(&self, t: &Target, identity: &CheckIdentity) -> Result<&'static str> {
        self.validate(t, identity)?;
        Ok(if self.before != self.after {
            "changed_during_capture"
        } else if self.after.matches(t)? {
            "matches_retained_target"
        } else {
            "differs_from_retained_target"
        })
    }
}
