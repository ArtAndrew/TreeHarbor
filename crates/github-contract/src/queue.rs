//! Queue delivery observations. No observation here proves final merged content.
use super::*;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueTarget {
    pub pull: InspectionTarget,
    pub expected_base_revision: String,
    pub expected_rules: RuleObservation,
}
impl QueueTarget {
    pub fn validate(&self) -> Result<()> {
        self.pull.validate()?;
        revision(&self.expected_base_revision)?;
        self.expected_rules.validate()
    }
    pub fn admits_capture(&self, r: &InspectionReport) -> bool {
        self.validate().is_ok()
            && r.selected_checks_passed(&self.pull)
            && r.before.state == "open"
            && !r.before.draft
            && !r.before.merged
            && r.before.base_revision == self.expected_base_revision
            && r.rules_before == self.expected_rules
    }
}
pub(crate) fn node(s: &str) -> Result<()> {
    text(s, 256)?;
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"=_-".contains(&b))
    {
        Ok(())
    } else {
        Err(InvalidEvidence)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueConfiguration {
    pub id: String,
    pub merge_method: String,
    pub merging_strategy: String,
    pub maximum_entries_to_build: u32,
    pub maximum_entries_to_merge: u32,
    pub minimum_entries_to_merge: u32,
    pub minimum_entries_wait_minutes: u32,
    pub check_timeout_minutes: u32,
}
impl QueueConfiguration {
    pub fn validate(&self) -> Result<()> {
        node(&self.id)?;
        if !matches!(self.merge_method.as_str(), "MERGE" | "SQUASH" | "REBASE")
            || !matches!(self.merging_strategy.as_str(), "ALLGREEN" | "HEADGREEN")
            || !(1..=100).contains(&self.maximum_entries_to_build)
            || !(1..=100).contains(&self.maximum_entries_to_merge)
            || !(1..=self.maximum_entries_to_merge).contains(&self.minimum_entries_to_merge)
            || self.minimum_entries_wait_minutes > 1440
            || !(1..=1440).contains(&self.check_timeout_minutes)
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GitCommitObservation {
    pub revision: String,
    pub tree: String,
}
impl GitCommitObservation {
    pub fn validate(&self) -> Result<()> {
        revision(&self.revision)?;
        // Both are Git object IDs; the type is established by the provider query.
        revision(&self.tree)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueEntryObservation {
    pub id: String,
    pub state: String,
    pub base: Option<GitCommitObservation>,
    pub head: Option<GitCommitObservation>,
}
impl QueueEntryObservation {
    fn validate(&self) -> Result<()> {
        node(&self.id)?;
        if !matches!(
            self.state.as_str(),
            "QUEUED" | "AWAITING_CHECKS" | "LOCKED" | "MERGEABLE" | "UNMERGEABLE"
        ) {
            return Err(InvalidEvidence);
        }
        for commit in [&self.base, &self.head].into_iter().flatten() {
            commit.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueSnapshot {
    pub pull: PullState,
    pub queue_enabled: bool,
    pub configuration: Option<QueueConfiguration>,
    pub entry: Option<QueueEntryObservation>,
    pub final_commit: Option<GitCommitObservation>,
}
impl QueueSnapshot {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        t.validate()?;
        self.pull.validate(&t.pull)?;
        if let Some(c) = &self.configuration {
            c.validate()?;
        }
        if let Some(e) = &self.entry {
            e.validate()?;
            if !self.queue_enabled
                || self.configuration.is_none()
                || self.pull.merged
                || self.pull.state != "open"
                || self.pull.draft
            {
                return Err(InvalidEvidence);
            }
        }
        if let Some(c) = &self.final_commit {
            c.validate()?;
            if !self.pull.merged {
                return Err(InvalidEvidence);
            }
        }
        if self.pull.merged && self.final_commit.is_none() {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn can_request(&self, t: &QueueTarget) -> bool {
        self.validate(t).is_ok()
            && self.queue_enabled
            && self.configuration.is_some()
            && self.entry.is_none()
            && self.pull.state == "open"
            && !self.pull.draft
            && !self.pull.merged
            && self.pull.head_revision == t.pull.head_revision
            && self.pull.base_revision == t.expected_base_revision
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueReceipt {
    pub http_status: u16,
    pub status: String,
    pub request_uuid: Option<String>,
    pub expected_head_revision: Option<String>,
    pub merge_action: Option<String>,
    pub bypass_rules: Option<bool>,
    pub final_revision: Option<String>,
}
impl QueueReceipt {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        t.validate()?;
        if !matches!(self.http_status, 200 | 202 | 400 | 409) {
            return Err(InvalidEvidence);
        }
        match self.status.as_str() {
            "pending" => {
                let raw = self.request_uuid.as_ref().ok_or(InvalidEvidence)?;
                let id: uuid::Uuid = raw.parse().map_err(|_| InvalidEvidence)?;
                if !matches!(self.http_status, 202 | 409)
                    || id.is_nil()
                    || id.to_string() != *raw
                    || self.expected_head_revision.as_ref() != Some(&t.pull.head_revision)
                    || self.merge_action.as_deref() != Some("merge_queue")
                    || self.bypass_rules == Some(true)
                    || (self.http_status == 409 && self.bypass_rules != Some(false))
                    || self.final_revision.is_some()
                {
                    return Err(InvalidEvidence);
                }
            }
            "enqueued" | "merged" | "failed" => {
                if self.http_status != if self.status == "failed" { 400 } else { 200 }
                    || self.request_uuid.is_some()
                    || self.expected_head_revision.is_some()
                    || self.merge_action.is_some()
                    || self.bypass_rules.is_some()
                {
                    return Err(InvalidEvidence);
                }
                if self.status == "merged" {
                    revision(self.final_revision.as_ref().ok_or(InvalidEvidence)?)?;
                } else if self.final_revision.is_some() {
                    return Err(InvalidEvidence);
                }
            }
            _ => return Err(InvalidEvidence),
        }
        Ok(())
    }
}
/// A result read is tied to the exact retained provider UUID. A 404 cannot
/// distinguish expiry from hidden/unavailable data and never proves failure.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueAsyncResult {
    pub request_uuid: String,
    pub http_status: u16,
    pub outcome: QueueAsyncOutcome,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum QueueAsyncOutcome {
    Pending {
        expected_head_revision: String,
        merge_action: String,
        bypass_rules: bool,
    },
    Enqueued,
    Failed,
    Merged {
        final_revision: String,
    },
    Unavailable,
}
impl QueueAsyncResult {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        t.validate()?;
        let id: uuid::Uuid = self.request_uuid.parse().map_err(|_| InvalidEvidence)?;
        if id.is_nil() || id.to_string() != self.request_uuid {
            return Err(InvalidEvidence);
        }
        if self.http_status
            != if self.outcome == QueueAsyncOutcome::Unavailable {
                404
            } else {
                200
            }
        {
            return Err(InvalidEvidence);
        }
        match &self.outcome {
            QueueAsyncOutcome::Pending {
                expected_head_revision,
                merge_action,
                bypass_rules,
            } => {
                if expected_head_revision != &t.pull.head_revision
                    || merge_action != "merge_queue"
                    || *bypass_rules
                {
                    return Err(InvalidEvidence);
                }
            }
            QueueAsyncOutcome::Merged { final_revision } => revision(final_revision)?,
            _ => (),
        }
        Ok(())
    }
    pub fn terminal(&self) -> bool {
        matches!(
            self.outcome,
            QueueAsyncOutcome::Enqueued
                | QueueAsyncOutcome::Failed
                | QueueAsyncOutcome::Merged { .. }
        )
    }
    pub fn state(&self) -> &'static str {
        match self.outcome {
            QueueAsyncOutcome::Pending { .. } => "host_pending",
            QueueAsyncOutcome::Enqueued => "host_enqueued",
            QueueAsyncOutcome::Failed => "request_failed",
            QueueAsyncOutcome::Merged { .. } => "host_merged_unverified",
            QueueAsyncOutcome::Unavailable => "result_unavailable",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueObservation {
    pub version: u8,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub snapshot: Option<QueueSnapshot>,
    pub receipt: Option<QueueReceipt>,
    pub async_result: Option<QueueAsyncResult>,
    /// Version 2 positive completion evidence from the sole enqueue invocation.
    /// Version 1 reports remain readable and cannot assert this fact.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mutation_not_sent: bool,
    pub token_revoked: bool,
}
fn is_false(value: &bool) -> bool {
    !value
}
impl QueueObservation {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        t.validate()?;
        if !matches!(self.version, 1 | 2)
            || (self.mutation_not_sent
                && (self.version != 2 || self.receipt.is_some() || self.async_result.is_some()))
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 180_000
        {
            return Err(InvalidEvidence);
        }
        if let Some(s) = &self.snapshot {
            s.validate(t)?;
        }
        if let Some(r) = &self.receipt {
            r.validate(t)?;
        }
        if let Some(r) = &self.async_result {
            r.validate(t)?;
            if self.receipt.is_some() {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
    /// Historical observation only; no variant here is a verified final merge.
    pub fn state(&self, t: &QueueTarget) -> &'static str {
        if self.validate(t).is_err() {
            return "uncertain";
        }
        if let Some(s) = &self.snapshot {
            if s.pull.head_revision != t.pull.head_revision {
                return "stale";
            }
            if s.pull.merged {
                return "host_merged_unverified";
            }
            if s.pull.state == "closed" {
                return "closed";
            }
            if let Some(e) = &s.entry {
                return if e.head.is_some() && e.base.is_some() {
                    "validating"
                } else {
                    "host_enqueued"
                };
            }
        }
        if let Some(r) = &self.async_result {
            return r.state();
        }
        if self.mutation_not_sent {
            return "not_sent";
        }
        // A receipt does not establish continued membership of a mutable queue.
        match self.receipt.as_ref().map(|r| r.status.as_str()) {
            Some("pending") => "host_pending",
            Some("enqueued") => "host_enqueued",
            Some("merged") => "host_merged_unverified",
            Some("failed") => "request_failed",
            _ => "uncertain",
        }
    }
}
