use crate::{Error, Result};
use treeharbor_core::RepoPath;

#[derive(Clone, Debug)]
pub struct Selection {
    pub(crate) paths: Option<Vec<RepoPath>>,
}
impl Selection {
    pub fn full() -> Self {
        Self { paths: None }
    }
    /// Explicit path/subtree selection; no glob or inferred build dependencies.
    pub fn sparse(mut paths: Vec<RepoPath>) -> Result<Self> {
        if paths.is_empty() || paths.len() > 1024 {
            return Err(Error::Configuration);
        }
        paths.sort();
        paths.dedup();
        let mut normalized: Vec<RepoPath> = Vec::new();
        for path in paths {
            if !normalized.iter().any(|p| p.is_ancestor_of(&path)) {
                normalized.push(path);
            }
        }
        Ok(Self {
            paths: Some(normalized),
        })
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
