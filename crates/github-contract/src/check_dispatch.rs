//! Versioned identity of a queue-check dispatch decision, never merge authority.
//!
//! Structural validity does not establish trust. The metadata transaction must
//! resolve every reference, evaluate current authorization and retain this record
//! atomically with its one-shot dispatch grant. A hash is not a signature.
use crate::{InvalidEvidence, Result, check_publication as cp};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treeharbor_core::ObjectId;
use uuid::Uuid;

pub const DOMAIN: &[u8] = b"treeharbor-github-check-dispatch-policy\0v1\0";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PolicyProof {
    pub version: u8,
    pub tenant: String,
    pub repository: Uuid,
    pub queue: Uuid,
    pub check_run: Uuid,
    pub execution_run: Uuid,
    /// Actual first committed collection operation, not a worker's timestamp.
    pub collection_key: String,
    pub collected_at_unix_ms: i64,
    pub decided_at_unix_ms: i64,
    pub target: cp::Target,
    pub host_policy_sequence: i64,
    pub host_policy_digest: String,
    pub configuration_digest: String,
    /// Hash of the exact retained transport preflight; no wire format change.
    pub preflight_digest: String,
    pub capture_started_at_unix_ms: i64,
    pub capture_finished_at_unix_ms: i64,
}

fn digest(value: &str) -> Result<()> {
    let parsed: ObjectId = value.parse().map_err(|_| InvalidEvidence)?;
    if parsed.to_string() != value {
        return Err(InvalidEvidence);
    }
    Ok(())
}

impl PolicyProof {
    pub fn validate(&self) -> Result<()> {
        self.target.validate()?;
        if self.version != 1
            || self.tenant.is_empty()
            || self.tenant.len() > 64
            || !self
                .tenant
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || [
                self.repository,
                self.queue,
                self.check_run,
                self.execution_run,
            ]
            .iter()
            .any(Uuid::is_nil)
            || self.collection_key.is_empty()
            || self.collection_key.len() > 128
            || !self.collection_key.bytes().all(|b| b.is_ascii_graphic())
            || self.collected_at_unix_ms <= 0
            || self.decided_at_unix_ms < self.collected_at_unix_ms
            || !(1..=4096).contains(&self.host_policy_sequence)
            || self.target.version != 2
            || self.target.admission.is_none()
            || self.target.conclusion != cp::Conclusion::Success
            || !matches!(self.target.source, cp::Source::Group { .. })
            || self.capture_started_at_unix_ms < self.collected_at_unix_ms
            || self.capture_finished_at_unix_ms < self.capture_started_at_unix_ms
            || self
                .capture_finished_at_unix_ms
                .saturating_sub(self.capture_started_at_unix_ms)
                > 120_000
            || self.capture_started_at_unix_ms < self.decided_at_unix_ms.saturating_sub(120_000)
            || self.capture_finished_at_unix_ms > self.decided_at_unix_ms.saturating_add(15_000)
        {
            return Err(InvalidEvidence);
        }
        for value in [
            &self.host_policy_digest,
            &self.configuration_digest,
            &self.preflight_digest,
        ] {
            digest(value)?;
        }
        Ok(())
    }

    /// UTF-8 compact JSON with recursively sorted keys, prefixed by DOMAIN for
    /// hashing. Never reuse another evidence type's domain or serializer order.
    pub fn canonical(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let value = serde_json::to_value(self).map_err(|_| InvalidEvidence)?;
        serde_json::to_vec(&value).map_err(|_| InvalidEvidence)
    }

    pub fn digest(&self) -> Result<String> {
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(self.canonical()?);
        Ok(ObjectId::from_bytes(hash.finalize().into()).to_string())
    }
}
