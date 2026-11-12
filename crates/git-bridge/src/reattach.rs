//! Continue an archived Git graph from its exact previously published native IDs.
use crate::{Error, Result, VerifiedBranchMappings, check_branch_refs, check_deadline, stream};
use std::{
    collections::{BTreeMap, BTreeSet},
    time::Instant,
};
use treeharbor_core::{Commit, Object, ObjectId, ObjectStore};

#[derive(Debug, PartialEq, Eq)]
pub struct ReattachedTip {
    pub head: ObjectId,
    pub snapshot: ObjectId,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReattachedBranches {
    pub tips: BTreeMap<String, ReattachedTip>,
    /// Newly encoded commits, in child-object-first publication order.
    pub commits: Vec<ObjectId>,
}

#[derive(Clone, Copy)]
struct Reattached {
    head: ObjectId,
    snapshot: ObjectId,
    ancestors: u128,
}

fn commit(store: &impl ObjectStore, id: ObjectId) -> Result<Commit> {
    match stream::get(store, id)? {
        Object::NativeCommit(commit) => Ok(commit),
        _ => Err(Error::InvalidProjection),
    }
}

/// Retain the native IDs of previously projected commits and re-encode only
/// imported commits whose parent IDs change. Both mappings must have been
/// verified against their complete typed graphs. The caller keeps both input
/// graphs pinned and provides a private scratch store for new commit frames.
/// Nothing from scratch is eligible for publication if this function fails.
pub fn reattach_branches(
    history: &impl ObjectStore,
    imported: &impl ObjectStore,
    scratch: &impl ObjectStore,
    previous: &VerifiedBranchMappings,
    archive: &VerifiedBranchMappings,
    deadline: Instant,
) -> Result<ReattachedBranches> {
    check_branch_refs(previous.tips())?;
    check_branch_refs(archive.tips())?;
    if previous.format != archive.format {
        return Err(Error::InvalidProjection);
    }
    let mut ancestry = BTreeMap::<ObjectId, u128>::new();
    for (index, (name, (old, _))) in previous.tips().iter().enumerate() {
        if !archive.tips().contains_key(name) {
            return Err(Error::RefHistoryDiverged(name.clone()));
        }
        *ancestry.entry(*old).or_default() |= 1 << index;
    }
    let mut historical = BTreeMap::new();
    for (id, git) in &previous.commits {
        check_deadline(deadline)?;
        if historical.insert(git, *id).is_some() {
            // Git's projection cannot identify which of two native commits to
            // preserve when both produce the same Git commit bytes.
            return Err(Error::InvalidProjection);
        }
    }

    let mut state = BTreeMap::<ObjectId, Option<Reattached>>::new();
    let mut pending: Vec<_> = archive
        .tips()
        .values()
        .map(|(head, _)| (*head, false))
        .collect();
    let mut commits = Vec::new();
    let mut seen_git = BTreeSet::new();
    while let Some((id, finish)) = pending.pop() {
        check_deadline(deadline)?;
        let git = archive.commits.get(&id).ok_or(Error::InvalidProjection)?;
        if !finish {
            if let Some(existing) = state.get(&id) {
                if existing.is_none() {
                    return Err(Error::InvalidProjection);
                }
                continue;
            }
            if !seen_git.insert(git) {
                return Err(Error::InvalidProjection);
            }
            let source = commit(imported, id)?;
            state.insert(id, None);
            pending.push((id, true));
            pending.extend(source.parents.into_iter().map(|parent| (parent, false)));
            continue;
        }

        let mut source = commit(imported, id)?;
        let mut ancestors = 0;
        let parents = source
            .parents
            .iter()
            .map(|parent| {
                let mapped = state
                    .get(parent)
                    .and_then(|entry| *entry)
                    .ok_or(Error::InvalidProjection)?;
                ancestors |= mapped.ancestors;
                Ok(mapped.head)
            })
            .collect::<Result<Vec<_>>>()?;
        let snapshot = source.snapshot;
        let head = if let Some(&old) = historical.get(git) {
            let anchor = commit(history, old)?;
            if anchor.snapshot != snapshot || anchor.parents != parents {
                return Err(Error::InvalidProjection);
            }
            ancestors |= ancestry.get(&old).copied().unwrap_or_default();
            old
        } else if source.parents == parents {
            id
        } else {
            source.parents = parents;
            let object = Object::NativeCommit(source);
            let expected = object.id()?;
            if scratch.put(&object)? != expected {
                return Err(Error::InvalidProjection);
            }
            commits.push(expected);
            expected
        };
        state.insert(
            id,
            Some(Reattached {
                head,
                snapshot,
                ancestors,
            }),
        );
    }

    for (index, name) in previous.tips().keys().enumerate() {
        let (imported_head, _) = archive.tips()[name];
        if state[&imported_head]
            .ok_or(Error::InvalidProjection)?
            .ancestors
            & (1 << index)
            == 0
        {
            return Err(Error::RefHistoryDiverged(name.clone()));
        }
    }
    let tips = archive
        .tips()
        .iter()
        .map(|(name, (id, _))| {
            let tip = state
                .get(id)
                .and_then(|entry| *entry)
                .ok_or(Error::InvalidProjection)?;
            Ok((
                name.clone(),
                ReattachedTip {
                    head: tip.head,
                    snapshot: tip.snapshot,
                },
            ))
        })
        .collect::<Result<_>>()?;
    Ok(ReattachedBranches { tips, commits })
}
