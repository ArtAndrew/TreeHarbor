//! Exact candidate publication, independent of checks and merge authorization.
use crate::{groups::*, queue::*, *};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub membership_sequence: i64,
    pub member: QueueMember,
}
impl Target {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        t.validate()?;
        if !(1..=4096).contains(&self.membership_sequence)
            || self.member.pull_node != t.pull.pull_node
            || self.member.pull_number != t.pull.pull_number
            || self.member.source_repository_id != t.pull.repository_id
            || self.member.source_revision != t.pull.head_revision
            || self.member.base_ref != t.pull.base_ref
            || self.member.position == 0
            || !self.member.has_exact_merge_parents()
        {
            return Err(InvalidEvidence);
        }
        self.member.entry.validate()?;
        let c = self.member.candidate.as_ref().ok_or(InvalidEvidence)?;
        c.validate()?;
        if self.member.entry.head.as_ref() != Some(&c.commit) {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn validate_source(&self, t: &QueueTarget, g: &GroupCapture) -> Result<()> {
        self.validate(t)?;
        if !g.consistent(t) || g.before.selected(t) != Some(&self.member) {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn candidate(&self) -> Result<&CommitParents> {
        self.member.candidate.as_ref().ok_or(InvalidEvidence)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub pull: PullState,
    pub main: GitCommitObservation,
    pub final_commit: Option<CommitParents>,
}
impl Snapshot {
    fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.pull.validate(&t.pull)?;
        self.main.validate()?;
        if self.pull.merged != self.final_commit.is_some() {
            return Err(InvalidEvidence);
        }
        if let Some(c) = &self.final_commit {
            c.validate()?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Comparison {
    pub base: GitCommitObservation,
    /// Immutable head OID in the authenticated request, never a mutable branch name.
    pub head_revision: String,
    pub merge_base: GitCommitObservation,
    pub status: String,
    pub ahead_by: u32,
    pub behind_by: u32,
    pub total_commits: u32,
}
impl Comparison {
    fn validate(&self) -> Result<()> {
        self.base.validate()?;
        self.merge_base.validate()?;
        revision(&self.head_revision)?;
        if self.ahead_by > i32::MAX as u32
            || self.behind_by > i32::MAX as u32
            || self.total_commits != self.ahead_by
        {
            return Err(InvalidEvidence);
        }
        let same = self.base.revision == self.head_revision;
        let valid = match self.status.as_str() {
            "identical" => {
                same && self.ahead_by == 0 && self.behind_by == 0 && self.merge_base == self.base
            }
            "ahead" => {
                !same && self.ahead_by > 0 && self.behind_by == 0 && self.merge_base == self.base
            }
            "behind" => {
                !same
                    && self.ahead_by == 0
                    && self.behind_by > 0
                    && self.merge_base.revision == self.head_revision
            }
            "diverged" => {
                !same
                    && self.ahead_by > 0
                    && self.behind_by > 0
                    && self.merge_base.revision != self.base.revision
                    && self.merge_base.revision != self.head_revision
            }
            _ => false,
        };
        if !valid {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub target: Target,
    pub before: Snapshot,
    pub comparison: Comparison,
    pub after: Snapshot,
}
impl Report {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.target.validate(t)?;
        self.before.validate(t)?;
        self.after.validate(t)?;
        self.comparison.validate()?;
        let c = self.target.candidate()?;
        if self.comparison.base != c.commit
            || self.comparison.head_revision != self.before.main.revision
            || serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 24 * 1024
        {
            return Err(InvalidEvidence);
        }
        let mut trees = BTreeMap::new();
        let mut parents = BTreeMap::new();
        let mut commits = vec![
            &self.before.main,
            &self.after.main,
            &self.comparison.base,
            &self.comparison.merge_base,
        ];
        for c in std::iter::once(c)
            .chain(self.before.final_commit.iter())
            .chain(self.after.final_commit.iter())
        {
            commits.push(&c.commit);
            commits.extend(&c.parents);
            if parents
                .insert(&c.commit.revision, &c.parents)
                .is_some_and(|p| p != &c.parents)
            {
                return Err(InvalidEvidence);
            }
        }
        for c in commits {
            if trees
                .insert(&c.revision, &c.tree)
                .is_some_and(|tree| tree != &c.tree)
            {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
    /// Historical content/ancestry fact under provider trust; never admission or CI proof.
    pub fn exact_candidate_published(&self, t: &QueueTarget) -> bool {
        self.validate(t).is_ok()
            && self.before == self.after
            && self.before.pull.head_revision == t.pull.head_revision
            && self.before.pull.merged
            && self.before.final_commit.as_ref() == self.target.member.candidate.as_ref()
            && matches!(self.comparison.status.as_str(), "identical" | "ahead")
    }
}
