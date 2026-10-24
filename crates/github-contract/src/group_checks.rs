//! Provider check identities are observations, not native validations.
use crate::{groups::GroupCapture, queue::*, *};
use std::collections::BTreeMap;

pub const MAX_SUITES: usize = 20;
pub const MAX_RUNS: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowFile {
    pub path: String,
    pub repository_name: String,
    /// Recorded provider metadata only. Never an instruction to fetch this URL.
    pub repository_file_url: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRun {
    pub id: String,
    pub workflow_id: String,
    pub event: String,
    /// Current workflow attempt at observation; not a per-check attempt proof.
    pub observed_run_attempt: u32,
    pub file: Option<WorkflowFile>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub id: String,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Suite {
    pub id: String,
    pub repository_id: u64,
    pub head_revision: String,
    pub app_id: Option<u64>,
    pub workflow: Option<WorkflowRun>,
    pub runs: Vec<Run>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub suites: Vec<Suite>,
}
impl Snapshot {
    pub fn validate(&self, repository: u64, subject: &GitCommitObservation) -> Result<()> {
        positive(repository)?;
        subject.validate()?;
        if self.suites.len() > MAX_SUITES {
            return Err(InvalidEvidence);
        }
        let mut suites = BTreeSet::new();
        let mut workflows = BTreeSet::new();
        let mut runs = BTreeSet::new();
        for s in &self.suites {
            node(&s.id)?;
            if s.repository_id != repository
                || s.head_revision != subject.revision
                || !suites.insert(&s.id)
            {
                return Err(InvalidEvidence);
            }
            if let Some(app) = s.app_id {
                positive(app)?;
            }
            if let Some(w) = &s.workflow {
                node(&w.id)?;
                node(&w.workflow_id)?;
                text(&w.event, 64)?;
                if w.observed_run_attempt == 0
                    || !workflows.insert(&w.id)
                    || !w.event.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                {
                    return Err(InvalidEvidence);
                }
                if let Some(f) = &w.file {
                    text(&f.path, 1024)?;
                    text(&f.repository_name, 256)?;
                    text(&f.repository_file_url, 4096)?;
                    let u = url::Url::parse(&f.repository_file_url).map_err(|_| InvalidEvidence)?;
                    if u.scheme() != "https"
                        || u.host_str() != Some("github.com")
                        || !u.username().is_empty()
                        || u.password().is_some()
                        || u.port().is_some()
                        || u.query().is_some()
                        || u.fragment().is_some()
                        || u.as_str() != f.repository_file_url
                    {
                        return Err(InvalidEvidence);
                    }
                }
            }
            for r in &s.runs {
                node(&r.id)?;
                text(&r.name, 256)?;
                if !runs.insert(&r.id)
                    || runs.len() > MAX_RUNS
                    || !matches!(
                        r.status.as_str(),
                        "COMPLETED"
                            | "IN_PROGRESS"
                            | "PENDING"
                            | "QUEUED"
                            | "REQUESTED"
                            | "WAITING"
                    )
                {
                    return Err(InvalidEvidence);
                }
                if let Some(c) = &r.conclusion
                    && (r.status != "COMPLETED"
                        || !matches!(
                            c.as_str(),
                            "ACTION_REQUIRED"
                                | "CANCELLED"
                                | "FAILURE"
                                | "NEUTRAL"
                                | "SKIPPED"
                                | "STALE"
                                | "STARTUP_FAILURE"
                                | "SUCCESS"
                                | "TIMED_OUT"
                        ))
                {
                    return Err(InvalidEvidence);
                }
                if r.status == "COMPLETED" && r.conclusion.is_none()
                    || r.started_at_unix_ms.is_some_and(|t| t <= 0)
                    || r.completed_at_unix_ms.is_some_and(|t| t <= 0)
                    || r.started_at_unix_ms
                        .zip(r.completed_at_unix_ms)
                        .is_some_and(|(a, b)| b < a)
                {
                    return Err(InvalidEvidence);
                }
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroupChecks {
    pub subject: GitCommitObservation,
    pub before: Snapshot,
    pub after: Snapshot,
}
impl GroupChecks {
    pub fn validate_snapshots(&self, repository: u64) -> Result<()> {
        self.before.validate(repository, &self.subject)?;
        self.after.validate(repository, &self.subject)?;
        let mut identities = BTreeMap::new();
        let mut suites = BTreeMap::new();
        for view in [&self.before, &self.after] {
            for s in &view.suites {
                let identity = (
                    s.app_id,
                    s.workflow
                        .as_ref()
                        .map(|w| (&w.id, &w.workflow_id, &w.event)),
                );
                if suites
                    .insert(&s.id, identity)
                    .is_some_and(|old| old != identity)
                {
                    return Err(InvalidEvidence);
                }
                for r in &s.runs {
                    let identity = (&s.id, &r.name);
                    if identities
                        .insert(&r.id, identity)
                        .is_some_and(|old| old != identity)
                    {
                        return Err(InvalidEvidence);
                    }
                }
            }
        }
        Ok(())
    }
    pub fn validate_times(&self, finished: i64) -> Result<()> {
        for v in [&self.before, &self.after] {
            for s in &v.suites {
                for r in &s.runs {
                    if r.started_at_unix_ms
                        .into_iter()
                        .chain(r.completed_at_unix_ms)
                        .any(|t| t > finished.saturating_add(15000))
                    {
                        return Err(InvalidEvidence);
                    }
                }
            }
        }
        Ok(())
    }
    pub fn consistent(&self, repository: u64) -> bool {
        self.validate_snapshots(repository).is_ok() && self.before == self.after
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Before,
    After,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroupCheckTarget {
    pub membership_sequence: i64,
    pub side: Side,
    pub subject: GitCommitObservation,
}
impl GroupCheckTarget {
    pub fn validate(&self) -> Result<()> {
        if !(1..=4096).contains(&self.membership_sequence) {
            return Err(InvalidEvidence);
        }
        self.subject.validate()
    }
    pub fn validate_membership(&self, t: &QueueTarget, g: &GroupCapture) -> Result<()> {
        self.validate()?;
        g.validate(t)?;
        let s = match self.side {
            Side::Before => &g.before,
            Side::After => &g.after,
        };
        let m = s.selected(t).ok_or(InvalidEvidence)?;
        if m.source_revision != t.pull.head_revision
            || m.candidate.as_ref().map(|c| &c.commit) != Some(&self.subject)
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroupCheckReport {
    pub target: GroupCheckTarget,
    pub checks: GroupChecks,
}
impl GroupCheckReport {
    pub fn validate(&self, t: &QueueTarget, finished: i64) -> Result<()> {
        self.target.validate()?;
        if self.target.subject != self.checks.subject {
            return Err(InvalidEvidence);
        }
        self.checks.validate_snapshots(t.pull.repository_id)?;
        self.checks.validate_times(finished)?;
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 48 * 1024 {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
