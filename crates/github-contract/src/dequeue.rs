//! Explicit PR-scoped dequeue evidence. Neither absence nor acknowledgment is a
//! guarantee that a concurrent actor cannot merge or re-enqueue the PR later.
use super::{queue::*, *};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DequeueReceipt {
    pub request_id: String,
    pub repository_id: u64,
    pub pull_node: String,
    pub pull_number: u64,
    pub queue_id: String,
    pub entry_id: String,
    pub head_revision: String,
}
impl DequeueReceipt {
    pub fn validate(&self, target: &QueueTarget, request: uuid::Uuid) -> Result<()> {
        target.validate()?;
        if request.is_nil()
            || self.request_id != request.to_string()
            || self.repository_id != target.pull.repository_id
            || self.pull_node != target.pull.pull_node
            || self.pull_number != target.pull.pull_number
        {
            return Err(InvalidEvidence);
        }
        node(&self.queue_id)?;
        node(&self.entry_id)?;
        // The provider mutation is PR-scoped and has no expected-head input.
        revision(&self.head_revision)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DequeueObservation {
    pub version: u8,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: Option<QueueSnapshot>,
    pub receipt: Option<DequeueReceipt>,
    pub after: Option<QueueSnapshot>,
    /// Positive evidence from the sole dispatch invocation, never inferred from
    /// absence observed after a restart or a missing provider response.
    pub mutation_not_sent: bool,
    pub token_revoked: bool,
}
impl DequeueObservation {
    pub fn validate(&self, target: &QueueTarget, request: uuid::Uuid) -> Result<()> {
        target.validate()?;
        if request.is_nil()
            || !matches!(self.version, 1 | 2)
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 180_000
            || (self.mutation_not_sent && self.receipt.is_some())
        {
            return Err(InvalidEvidence);
        }
        for snapshot in [&self.before, &self.after].into_iter().flatten() {
            if self.version == 1 && snapshot.queue_id.is_some() {
                return Err(InvalidEvidence);
            }
            snapshot.validate(target)?;
        }
        if let Some(receipt) = &self.receipt {
            receipt.validate(target, request)?;
            if self
                .before
                .as_ref()
                .and_then(|s| s.entry.as_ref())
                .is_none()
            {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
    /// Set by the coordinator from durable enqueue history, never by this report.
    fn removal_observed(&self, enqueue_settled: bool) -> bool {
        enqueue_settled
            && self
                .after
                .as_ref()
                .is_some_and(|s| !s.pull.merged && s.entry.is_none())
    }
    pub fn changed_during_dequeue(&self) -> bool {
        self.receipt
            .as_ref()
            .zip(self.before.as_ref())
            .is_some_and(|(r, s)| {
                s.pull.head_revision != r.head_revision
                    || s.entry.as_ref().is_none_or(|e| e.id != r.entry_id)
                    || s.queue_id
                        .as_ref()
                        .or(s.configuration.as_ref().map(|q| &q.id))
                        .is_none_or(|id| *id != r.queue_id)
            })
    }
    pub fn state(
        &self,
        target: &QueueTarget,
        request: uuid::Uuid,
        enqueue_settled: bool,
    ) -> &'static str {
        if self.validate(target, request).is_err() {
            return "uncertain";
        }
        if let Some(s) = &self.after {
            if s.pull.merged {
                return if s.pull.head_revision == target.pull.head_revision {
                    "host_merged_unverified"
                } else {
                    "other_head_merged"
                };
            }
            if s.entry.is_some() {
                return "host_queued";
            }
            if self.removal_observed(enqueue_settled) {
                return "removal_observed";
            }
            return "enqueue_unresolved";
        }
        if self.receipt.is_some() {
            "dequeue_acknowledged"
        } else if self.mutation_not_sent {
            "not_sent"
        } else {
            "uncertain"
        }
    }
}
