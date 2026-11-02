//! Provider observations do not attest receiver persistence or event processing.
use super::*;
use crate::deliveries::{DeliveryAttempt, DeliveryDetails, callback_url};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreDeliveryReport {
    pub version: u8,
    pub app_id: u64,
    pub installation_id: u64,
    pub repository_id: u64,
    pub callback_url: String,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: DeliveryDetails,
    pub after: DeliveryDetails,
}
impl RestoreDeliveryReport {
    pub fn validate(&self, expected: &DeliveryAttempt) -> Result<()> {
        positive(self.app_id)?;
        callback_url(&self.callback_url)?;
        expected.validate(
            self.installation_id,
            self.repository_id,
            self.finished_at_unix_ms,
        )?;
        if self.version != 1
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 120_000
        {
            return Err(InvalidEvidence);
        }
        for d in [&self.before, &self.after] {
            d.validate(self.installation_id, self.repository_id)?;
            if !d.attempt.same_attempt(expected)
                || d.observed_at_unix_ms < self.started_at_unix_ms
                || d.observed_at_unix_ms > self.finished_at_unix_ms
            {
                return Err(InvalidEvidence);
            }
        }
        if self.before.observed_at_unix_ms > self.after.observed_at_unix_ms {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn capture_consistent(&self) -> bool {
        self.before.attempt == self.after.attempt
            && self.before.payload_sha256 == self.after.payload_sha256
    }
}
