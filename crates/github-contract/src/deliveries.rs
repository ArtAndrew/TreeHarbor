//! Delivery metadata is distinct from signed callback content and merge evidence.
use crate::{InvalidEvidence, Result, positive};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// v1 hashes sorted-key, compact UTF-8 JSON with a domain prefix. This compares
/// parsed provider content; it never replaces verification of the original body.
pub fn payload_digest(value: &serde_json::Value) -> Result<String> {
    fn sorted(v: &serde_json::Value, depth: usize) -> Result<serde_json::Value> {
        if depth > 64 {
            return Err(InvalidEvidence);
        }
        Ok(match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<_> = m.keys().collect();
                keys.sort();
                let mut out = serde_json::Map::new();
                for key in keys {
                    out.insert(key.clone(), sorted(&m[key], depth + 1)?);
                }
                serde_json::Value::Object(out)
            }
            serde_json::Value::Array(v) => serde_json::Value::Array(
                v.iter()
                    .map(|x| sorted(x, depth + 1))
                    .collect::<Result<_>>()?,
            ),
            _ => v.clone(),
        })
    }
    if !value.is_object() {
        return Err(InvalidEvidence);
    }
    let bytes = serde_json::to_vec(&sorted(value, 0)?).map_err(|_| InvalidEvidence)?;
    if bytes.len() > 8 * 1024 * 1024 {
        return Err(InvalidEvidence);
    }
    let mut h = Sha256::new();
    h.update(b"treeharbor.github.payload-json.v1\0");
    h.update(bytes);
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}
pub fn callback_url(value: &str) -> Result<()> {
    if value.len() > 2048 {
        return Err(InvalidEvidence);
    }
    let url = url::Url::parse(value).map_err(|_| InvalidEvidence)?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/github/events"
        || url.as_str() != value
    {
        return Err(InvalidEvidence);
    }
    Ok(())
}
pub fn cursor(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 1024 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(InvalidEvidence);
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryDetails {
    pub version: u8,
    pub attempt: DeliveryAttempt,
    pub observed_at_unix_ms: i64,
    pub payload_sha256: String,
}
impl DeliveryDetails {
    pub fn validate(&self, installation: u64, repository: u64) -> Result<()> {
        self.attempt
            .validate(installation, repository, self.observed_at_unix_ms)?;
        if self.version != 1
            || self.payload_sha256.len() != 64
            || !self
                .payload_sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DeliveryPageReport {
    pub version: u8,
    pub observed_at_unix_ms: i64,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub scanned: u16,
    pub attempts: Vec<DeliveryAttempt>,
}
impl DeliveryPageReport {
    pub fn validate(&self, installation: u64, repository: u64) -> Result<()> {
        if self.version != 1
            || self.observed_at_unix_ms <= 0
            || self.scanned > 100
            || self.attempts.len() > usize::from(self.scanned)
        {
            return Err(InvalidEvidence);
        }
        for value in [&self.cursor, &self.next_cursor].into_iter().flatten() {
            cursor(value)?;
        }
        if self.next_cursor.is_some() && self.cursor == self.next_cursor {
            return Err(InvalidEvidence);
        }
        let mut ids = std::collections::BTreeSet::new();
        for a in &self.attempts {
            a.validate(installation, repository, self.observed_at_unix_ms)?;
            if !ids.insert(a.id) {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
}

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
