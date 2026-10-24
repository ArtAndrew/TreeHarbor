//! Immutable evidence shapes and conservative evaluation, without HTTP or credentials.
pub mod deliveries;
pub mod dequeue;
pub mod group_attempts;
pub mod group_checks;
pub mod groups;
pub mod queue;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use treeharbor_core::{GitOid, RevisionRef};

#[derive(Debug, thiserror::Error)]
#[error("invalid GitHub evidence identity, bounds or capture")]
pub struct InvalidEvidence;
type Result<T> = std::result::Result<T, InvalidEvidence>;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct RequiredCheck {
    pub name: String,
    pub app_id: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InspectionTarget {
    pub repository_id: u64,
    pub pull_number: u64,
    pub pull_node: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_revision: String,
    pub required: Vec<RequiredCheck>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullState {
    pub repository_id: u64,
    pub number: u64,
    pub node_id: String,
    pub head_ref: String,
    pub base_ref: String,
    pub head_revision: String,
    pub base_revision: String,
    pub state: String,
    pub draft: bool,
    pub merged: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RuleObservation {
    pub sha256: String,
    pub count: u32,
    pub merge_queue: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckRun {
    pub id: u64,
    pub app_id: u64,
    pub name: String,
    pub head_revision: String,
    pub status: String,
    pub conclusion: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub required: RequiredCheck,
    pub total_count: u64,
    /// At most two latest results; multiple matches are ambiguous, not success.
    pub runs: Vec<CheckRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct InspectionReport {
    pub version: u8,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: PullState,
    pub after: PullState,
    pub rules_before: RuleObservation,
    pub rules_after: RuleObservation,
    pub checks: Vec<CheckResult>,
    pub token_revoked: bool,
}

fn positive(n: u64) -> Result<()> {
    if n == 0 || n > i64::MAX as u64 {
        Err(InvalidEvidence)
    } else {
        Ok(())
    }
}
fn text(s: &str, limit: usize) -> Result<()> {
    if s.trim().is_empty() || s.len() > limit || s.chars().any(char::is_control) {
        Err(InvalidEvidence)
    } else {
        Ok(())
    }
}
fn revision(s: &str) -> Result<()> {
    match s.parse::<RevisionRef>() {
        Ok(RevisionRef::Git(id @ GitOid::Sha1(bytes)))
            if bytes != [0; 20] && RevisionRef::Git(id).to_string() == s =>
        {
            Ok(())
        }
        _ => Err(InvalidEvidence),
    }
}
fn branch(s: &str) -> Result<()> {
    if s.is_empty()
        || s.len() > 128
        || s.split('/').any(|p| {
            p.is_empty()
                || p.len() > 64
                || !p.as_bytes()[0].is_ascii_alphanumeric()
                || p.ends_with('.')
                || p.ends_with(".lock")
                || p.contains("..")
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
    {
        Err(InvalidEvidence)
    } else {
        Ok(())
    }
}
impl RequiredCheck {
    pub fn validate(&self) -> Result<()> {
        text(&self.name, 256)?;
        positive(self.app_id)
    }
}
impl InspectionTarget {
    pub fn validate(&self) -> Result<()> {
        positive(self.repository_id)?;
        positive(self.pull_number)?;
        text(&self.pull_node, 256)?;
        if !self
            .pull_node
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"=_-".contains(&b))
        {
            return Err(InvalidEvidence);
        }
        branch(&self.head_ref)?;
        branch(&self.base_ref)?;
        revision(&self.head_revision)?;
        if !self.head_ref.starts_with("treeharbor/")
            || self.head_ref == self.base_ref
            || self.required.is_empty()
            || self.required.len() > 16
        {
            return Err(InvalidEvidence);
        }
        let mut seen = BTreeSet::new();
        for r in &self.required {
            r.validate()?;
            if !seen.insert(r) {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
}
impl PullState {
    pub fn validate(&self, t: &InspectionTarget) -> Result<()> {
        revision(&self.head_revision)?;
        revision(&self.base_revision)?;
        if self.repository_id != t.repository_id
            || self.number != t.pull_number
            || self.node_id != t.pull_node
            || self.head_ref != t.head_ref
            || self.base_ref != t.base_ref
            || !matches!(self.state.as_str(), "open" | "closed")
            || (self.merged && self.state != "closed")
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
impl RuleObservation {
    fn validate(&self) -> Result<()> {
        if self.count > 100
            || self.sha256.len() != 64
            || !self
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            Err(InvalidEvidence)
        } else {
            Ok(())
        }
    }
}
impl InspectionReport {
    pub fn validate(&self, t: &InspectionTarget) -> Result<()> {
        t.validate()?;
        self.before.validate(t)?;
        self.after.validate(t)?;
        self.rules_before.validate()?;
        self.rules_after.validate()?;
        if self.version != 1
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 120_000
            || self.checks.len() != t.required.len()
        {
            return Err(InvalidEvidence);
        }
        let mut ids = BTreeSet::new();
        for (result, required) in self.checks.iter().zip(&t.required) {
            if result.required != *required
                || result.runs.len() > 2
                || result.runs.len() as u64 != result.total_count.min(2)
            {
                return Err(InvalidEvidence);
            }
            for run in &result.runs {
                positive(run.id)?;
                text(&run.status, 32)?;
                if let Some(c) = &run.conclusion {
                    text(c, 32)?;
                }
                if !ids.insert(run.id)
                    || run.name != required.name
                    || run.app_id != required.app_id
                    || run.head_revision != t.head_revision
                {
                    return Err(InvalidEvidence);
                }
            }
        }
        Ok(())
    }
    /// A capture-time comparison, never a promise about future provider state.
    pub fn consistent(&self, t: &InspectionTarget) -> bool {
        self.validate(t).is_ok()
            && self.before == self.after
            && self.before.head_revision == t.head_revision
            && self.rules_before == self.rules_after
    }
    /// Only the explicitly selected App/name pairs; not all GitHub policies.
    pub fn selected_checks_passed(&self, t: &InspectionTarget) -> bool {
        self.consistent(t)
            && self.checks.iter().all(|c| {
                c.total_count == 1
                    && c.runs[0].status == "completed"
                    && c.runs[0].conclusion.as_deref() == Some("success")
            })
    }
}
