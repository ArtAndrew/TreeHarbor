use crate::Result;
use treeharbor_core::{RepoPath, SourceProfile};

#[derive(Clone, Debug)]
pub struct Selection {
    pub(crate) paths: Option<Vec<RepoPath>>,
}
impl Selection {
    pub fn full() -> Self {
        Self { paths: None }
    }
    /// Explicit path/subtree selection; no glob or inferred build dependencies.
    pub fn sparse(paths: Vec<RepoPath>) -> Result<Self> {
        Ok(Self::from_profile(&SourceProfile::sparse(paths)?))
    }
    /// Use the exact persisted profile; no node-local reinterpretation of its ID.
    pub fn from_profile(profile: &SourceProfile) -> Self {
        Self {
            paths: profile.paths().map(<[RepoPath]>::to_vec),
        }
    }
    pub(crate) fn relevant(&self, path: &RepoPath) -> bool {
        self.paths.as_ref().is_none_or(|paths| {
            paths
                .iter()
                .any(|p| p == path || p.is_ancestor_of(path) || path.is_ancestor_of(p))
        })
    }
    pub(crate) fn union(&self, other: &Self) -> Result<Self> {
        match (&self.paths, &other.paths) {
            (Some(a), Some(b)) => Self::sparse(a.iter().chain(b).cloned().collect()),
            _ => Ok(Self::full()),
        }
    }
}
