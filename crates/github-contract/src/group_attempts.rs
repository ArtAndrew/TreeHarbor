//! Exact job/attempt provenance, separate from trusted definitions and merge policy.
use crate::{group_checks as gc, queue::*, *};

pub const MAX_JOBS: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub check_sequence: i64,
    pub side: gc::Side,
    pub suite_id: String,
    pub check_id: String,
}
impl Selection {
    pub fn validate(&self) -> Result<()> {
        if !(1..=4096).contains(&self.check_sequence) {
            return Err(InvalidEvidence);
        }
        node(&self.suite_id)?;
        node(&self.check_id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub check_sequence: i64,
    pub side: gc::Side,
    pub subject: GitCommitObservation,
    pub suite_id: String,
    pub app_id: u64,
    pub workflow: gc::WorkflowRun,
    pub check: gc::Run,
}
impl Target {
    pub fn selection(&self) -> Selection {
        Selection {
            check_sequence: self.check_sequence,
            side: self.side,
            suite_id: self.suite_id.clone(),
            check_id: self.check.id.clone(),
        }
    }
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        t.validate()?;
        self.selection().validate()?;
        if self.workflow.event != "merge_group"
            || !(1..=4096).contains(&self.check_sequence)
            || !t
                .pull
                .required
                .iter()
                .any(|r| r.name == self.check.name && r.app_id == self.app_id)
        {
            return Err(InvalidEvidence);
        }
        gc::Snapshot {
            suites: vec![gc::Suite {
                id: self.suite_id.clone(),
                repository_id: t.pull.repository_id,
                head_revision: self.subject.revision.clone(),
                app_id: Some(self.app_id),
                workflow: Some(self.workflow.clone()),
                runs: vec![self.check.clone()],
            }],
        }
        .validate(t.pull.repository_id, &self.subject)
    }
    pub fn validate_source(&self, t: &QueueTarget, source: &gc::GroupCheckReport) -> Result<()> {
        self.validate(t)?;
        if self.subject != source.checks.subject {
            return Err(InvalidEvidence);
        }
        source.checks.validate_snapshots(t.pull.repository_id)?;
        let snapshot = match self.side {
            gc::Side::Before => &source.checks.before,
            gc::Side::After => &source.checks.after,
        };
        let suite = snapshot
            .suites
            .iter()
            .find(|s| s.id == self.suite_id)
            .ok_or(InvalidEvidence)?;
        if suite.app_id != Some(self.app_id)
            || suite.workflow.as_ref() != Some(&self.workflow)
            || !suite.runs.iter().any(|r| r == &self.check)
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Resolved {
    pub check_id: u64,
    pub run_id: u64,
    pub workflow_id: u64,
    pub suite_id: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReferencedWorkflow {
    pub path: String,
    pub revision: String,
    pub reference: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Attempt {
    pub run_id: u64,
    pub node_id: String,
    pub workflow_id: u64,
    pub suite_id: u64,
    pub suite_node: String,
    pub repository_id: u64,
    pub head_revision: String,
    pub number: u32,
    pub event: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub path: String,
    pub referenced_workflows: Option<Vec<ReferencedWorkflow>>,
}
impl Attempt {
    fn validate(&self, t: &QueueTarget, selected: &Target, resolved: &Resolved) -> Result<()> {
        if self.run_id != resolved.run_id
            || self.node_id != selected.workflow.id
            || self.workflow_id != resolved.workflow_id
            || self.suite_id != resolved.suite_id
            || self.suite_node != selected.suite_id
            || self.repository_id != t.pull.repository_id
            || self.head_revision != selected.subject.revision
            || self.number == 0
            || self.event != selected.workflow.event
            || self
                .referenced_workflows
                .as_ref()
                .is_some_and(|v| v.len() > 20)
        {
            return Err(InvalidEvidence);
        }
        text(&self.path, 1024)?;
        status(&self.status, self.conclusion.as_deref())?;
        let mut paths = BTreeSet::new();
        for w in self.referenced_workflows.iter().flatten() {
            text(&w.path, 2048)?;
            revision(&w.revision)?;
            if !paths.insert(&w.path) {
                return Err(InvalidEvidence);
            }
            if let Some(r) = &w.reference {
                text(r, 1024)?;
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub id: u64,
    pub node_id: String,
    pub run_id: u64,
    pub attempt: u32,
    pub head_revision: String,
    pub check_run_id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub started_at_unix_ms: Option<i64>,
    pub completed_at_unix_ms: Option<i64>,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub database_id: u64,
    pub app_id: u64,
    pub suite_id: u64,
    pub head_revision: String,
    pub run: gc::Run,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub selected: Attempt,
    pub current: Attempt,
    pub jobs: Vec<Job>,
    pub check: Check,
}
fn status(s: &str, c: Option<&str>) -> Result<()> {
    if !matches!(
        s,
        "COMPLETED" | "IN_PROGRESS" | "PENDING" | "QUEUED" | "REQUESTED" | "WAITING"
    ) || (s == "COMPLETED") != c.is_some()
        || c.is_some_and(|c| {
            !matches!(
                c,
                "ACTION_REQUIRED"
                    | "CANCELLED"
                    | "FAILURE"
                    | "NEUTRAL"
                    | "SKIPPED"
                    | "STALE"
                    | "STARTUP_FAILURE"
                    | "SUCCESS"
                    | "TIMED_OUT"
            )
        })
    {
        return Err(InvalidEvidence);
    }
    Ok(())
}
fn times(a: Option<i64>, b: Option<i64>, finished: i64) -> Result<()> {
    if a.into_iter()
        .chain(b)
        .any(|n| n <= 0 || n > finished.saturating_add(15000))
        || a.zip(b).is_some_and(|(a, b)| b < a)
    {
        return Err(InvalidEvidence);
    }
    Ok(())
}
impl Snapshot {
    fn validate(
        &self,
        t: &QueueTarget,
        target: &Target,
        resolved: &Resolved,
        finished: i64,
    ) -> Result<()> {
        self.selected.validate(t, target, resolved)?;
        self.current.validate(t, target, resolved)?;
        if self.selected.number != target.workflow.observed_run_attempt
            || self.current.number < self.selected.number
            || self.jobs.len() > MAX_JOBS
        {
            return Err(InvalidEvidence);
        }
        let mut ids = BTreeSet::new();
        let mut nodes = BTreeSet::new();
        let mut checks = BTreeSet::new();
        for j in &self.jobs {
            positive(j.id)?;
            positive(j.check_run_id)?;
            node(&j.node_id)?;
            text(&j.name, 256)?;
            if j.run_id != resolved.run_id
                || j.attempt != self.selected.number
                || j.head_revision != target.subject.revision
                || !ids.insert(j.id)
                || !nodes.insert(&j.node_id)
                || !checks.insert(j.check_run_id)
            {
                return Err(InvalidEvidence);
            }
            status(&j.status, j.conclusion.as_deref())?;
            times(j.started_at_unix_ms, j.completed_at_unix_ms, finished)?;
        }
        let c = &self.check;
        positive(c.database_id)?;
        if c.database_id != resolved.check_id
            || c.app_id != target.app_id
            || c.suite_id != resolved.suite_id
            || c.head_revision != target.subject.revision
            || c.run.id != target.check.id
            || c.run.name != target.check.name
        {
            return Err(InvalidEvidence);
        }
        status(&c.run.status, c.run.conclusion.as_deref())?;
        times(
            c.run.started_at_unix_ms,
            c.run.completed_at_unix_ms,
            finished,
        )
    }
    fn matches_selected(&self, target: &Target) -> bool {
        self.check.run == target.check
            && self.jobs.iter().any(|j| {
                j.check_run_id == self.check.database_id
                    && j.name == self.check.run.name
                    && j.status == self.check.run.status
                    && j.conclusion == self.check.run.conclusion
                    && j.started_at_unix_ms == self.check.run.started_at_unix_ms
                    && j.completed_at_unix_ms == self.check.run.completed_at_unix_ms
            })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub target: Target,
    pub resolved: Resolved,
    pub before: Snapshot,
    pub after: Snapshot,
}
impl Report {
    pub fn validate(&self, t: &QueueTarget, finished: i64) -> Result<()> {
        self.target.validate(t)?;
        positive(self.resolved.check_id)?;
        positive(self.resolved.run_id)?;
        positive(self.resolved.workflow_id)?;
        positive(self.resolved.suite_id)?;
        self.before
            .validate(t, &self.target, &self.resolved, finished)?;
        self.after
            .validate(t, &self.target, &self.resolved, finished)?;
        if self.before.check.database_id != self.after.check.database_id {
            return Err(InvalidEvidence);
        }
        for job in &self.before.jobs {
            if let Some(other) = self.after.jobs.iter().find(|j| {
                j.id == job.id || j.node_id == job.node_id || j.check_run_id == job.check_run_id
            }) && (job.id != other.id
                || job.node_id != other.node_id
                || job.check_run_id != other.check_run_id
                || job.name != other.name)
            {
                return Err(InvalidEvidence);
            }
        }
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 48 * 1024 {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    /// Exact association only. Does not certify workflow trust, check success or final publication.
    pub fn attempt_linked(&self, t: &QueueTarget, finished: i64) -> bool {
        self.validate(t, finished).is_ok()
            && self.before == self.after
            && self.before.matches_selected(&self.target)
    }
    pub fn current_attempt_matches(&self) -> bool {
        self.before.current.number == self.target.workflow.observed_run_attempt
            && self.after.current.number == self.target.workflow.observed_run_attempt
    }
}
