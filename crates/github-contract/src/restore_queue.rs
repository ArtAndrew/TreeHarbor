//! Read-only queue captures during recovery, never settlement of an old mutation.
use super::*;
use crate::queue::{QueueSnapshot, QueueTarget};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreQueueReport {
    pub version: u8,
    pub app_id: u64,
    pub installation_id: u64,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: QueueSnapshot,
    pub after: QueueSnapshot,
    pub token_revoked: bool,
}
impl RestoreQueueReport {
    pub fn validate(&self, target: &QueueTarget) -> Result<()> {
        positive(self.app_id)?;
        positive(self.installation_id)?;
        self.before.validate(target)?;
        self.after.validate(target)?;
        if self.version != 1
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 120_000
            || self.before.configuration.is_some()
            || self.after.configuration.is_some()
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn classification(&self, target: &QueueTarget) -> Result<&'static str> {
        self.validate(target)?;
        Ok(if self.before != self.after {
            "changed_during_capture"
        } else if self.after.pull.head_revision != target.pull.head_revision {
            "head_differs"
        } else if self.after.pull.merged {
            "host_merged_unverified"
        } else if self.after.entry.is_some() {
            "queued"
        } else if self.after.pull.state == "closed" {
            "closed_unmerged"
        } else if !self.after.queue_enabled {
            "queue_disabled"
        } else {
            "not_in_queue"
        })
    }
}
