//! Delivery metadata is distinct from signed callback content and merge evidence.
use crate::{InvalidEvidence, Result, positive};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Numeric IDs distinguish delivery attempts; GUIDs correlate their callbacks.
/// Status is a provider observation, never proof that this receiver retained it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryAttempt {
    pub id: u64,
    pub guid: Uuid,
    pub installation_id: u64,
    pub repository_id: u64,
    pub delivered_at_unix_ms: i64,
    pub redelivery: bool,
    pub action: String,
    pub status_code: Option<u16>,
    pub throttled_at_unix_ms: Option<i64>,
}
impl DeliveryAttempt {
    pub fn validate(
        &self,
        installation_id: u64,
        repository_id: u64,
        observed_ms: i64,
    ) -> Result<()> {
        positive(installation_id)?;
        positive(repository_id)?;
        if self.id == 0
            || self.id > i64::MAX as u64
            || self.guid.is_nil()
            || self.installation_id != installation_id
            || self.repository_id != repository_id
            || !matches!(self.action.as_str(), "checks_requested" | "destroyed")
            || self.delivered_at_unix_ms <= 0
            || observed_ms <= 0
            || self.delivered_at_unix_ms > observed_ms.saturating_add(15000)
            || self
                .status_code
                .is_some_and(|c| c != 0 && !(100..=599).contains(&c))
            || self
                .throttled_at_unix_ms
                .is_some_and(|t| t <= 0 || t > observed_ms.saturating_add(15000))
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn same_attempt(&self, other: &Self) -> bool {
        self.id == other.id
            && self.guid == other.guid
            && self.installation_id == other.installation_id
            && self.repository_id == other.repository_id
            && self.delivered_at_unix_ms == other.delivered_at_unix_ms
            && self.redelivery == other.redelivery
            && self.action == other.action
    }
}
