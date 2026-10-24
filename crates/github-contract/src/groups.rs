//! Capture-time membership facts, never validation or permission to merge.
use super::{queue::*, *};
use std::collections::BTreeMap;

pub const MAX_MEMBERS: usize = 100;
pub const MAX_PARENTS: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CommitParents {
    pub commit: GitCommitObservation,
    /// Complete ordered parent list, not a truncated connection.
    pub parents: Vec<GitCommitObservation>,
}
impl CommitParents {
    pub fn validate(&self) -> Result<()> {
        self.commit.validate()?;
        if self.parents.len() > MAX_PARENTS {
            return Err(InvalidEvidence);
        }
        let mut seen = BTreeSet::new();
        for p in &self.parents {
            p.validate()?;
            if p.revision == self.commit.revision || !seen.insert(&p.revision) {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct QueueMember {
    pub entry: QueueEntryObservation,
    pub position: u32,
    pub pull_node: String,
    pub pull_number: u64,
    pub source_repository_id: u64,
    pub source_revision: String,
    pub base_ref: String,
    pub candidate: Option<CommitParents>,
}
impl QueueMember {
    fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.entry.validate()?;
        node(&self.pull_node)?;
        positive(self.pull_number)?;
        positive(self.source_repository_id)?;
        revision(&self.source_revision)?;
        if self.position == 0 || self.base_ref != t.pull.base_ref {
            return Err(InvalidEvidence);
        }
        if self.entry.head.as_ref() != self.candidate.as_ref().map(|c| &c.commit) {
            return Err(InvalidEvidence);
        }
        if let Some(c) = &self.candidate {
            c.validate()?;
        }
        Ok(())
    }
    /// Only this observed two-parent relationship. No inference for squash/rebase.
    pub fn has_exact_merge_parents(&self) -> bool {
        let (Some(base), Some(head)) = (&self.entry.base, &self.candidate) else {
            return false;
        };
        head.parents.len() == 2
            && head.parents[0] == *base
            && head.parents[1].revision == self.source_revision
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroupSnapshot {
    pub target: QueueSnapshot,
    /// Actual refs/heads/<target>, independently of the PR's baseRefOid.
    pub main: GitCommitObservation,
    pub members: Vec<QueueMember>,
}
impl GroupSnapshot {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.target.validate(t)?;
        self.main.validate()?;
        if self.members.len() > MAX_MEMBERS
            || (!self.members.is_empty() && self.target.queue_id.is_none())
        {
            return Err(InvalidEvidence);
        }
        let mut entries = BTreeSet::new();
        let mut pulls = BTreeSet::new();
        let mut numbers = BTreeSet::new();
        let mut position = 0;
        let mut selected = None;
        for m in &self.members {
            m.validate(t)?;
            if !entries.insert(&m.entry.id)
                || !pulls.insert(&m.pull_node)
                || !numbers.insert(m.pull_number)
                || m.position <= position
            {
                return Err(InvalidEvidence);
            }
            position = m.position;
            if m.pull_node == t.pull.pull_node || m.pull_number == t.pull.pull_number {
                if m.pull_node != t.pull.pull_node
                    || m.pull_number != t.pull.pull_number
                    || m.source_repository_id != t.pull.repository_id
                    || m.source_revision != self.target.pull.head_revision
                {
                    return Err(InvalidEvidence);
                }
                selected = Some(&m.entry);
            }
        }
        if selected != self.target.entry.as_ref() {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn selected(&self, t: &QueueTarget) -> Option<&QueueMember> {
        self.members
            .iter()
            .find(|m| m.pull_node == t.pull.pull_node)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct GroupCapture {
    pub before: GroupSnapshot,
    pub after: GroupSnapshot,
}
impl GroupCapture {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.before.validate(t)?;
        self.after.validate(t)?;
        let mut trees = BTreeMap::new();
        let mut parents = BTreeMap::new();
        for g in [&self.before, &self.after] {
            let mut commits = vec![&g.main];
            commits.extend(g.target.final_commit.iter());
            for m in &g.members {
                commits.extend(m.entry.base.iter());
                if let Some(c) = &m.candidate {
                    commits.push(&c.commit);
                    commits.extend(&c.parents);
                    if parents
                        .insert(&c.commit.revision, &c.parents)
                        .is_some_and(|old| old != &c.parents)
                    {
                        return Err(InvalidEvidence);
                    }
                }
            }
            for c in commits {
                if trees
                    .insert(&c.revision, &c.tree)
                    .is_some_and(|old| old != &c.tree)
                {
                    return Err(InvalidEvidence);
                }
            }
        }
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 48 * 1024 {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn consistent(&self, t: &QueueTarget) -> bool {
        self.validate(t).is_ok()
            && self.before == self.after
            && self.before.target.pull.head_revision == t.pull.head_revision
    }
}
