use crate::{Error, Result, Unsupported};
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::{EntryMode, EntryRef, ObjectId, RepoPath};

pub(crate) type Tree = BTreeMap<RepoPath, EntryRef>;

pub(crate) fn detect(base: &Tree, side: &Tree) -> Result<BTreeMap<RepoPath, RepoPath>> {
    // Group by immutable content and file kind, allowing an executable-bit change.
    fn key(entry: &EntryRef) -> (ObjectId, bool) {
        (entry.target, entry.mode == EntryMode::Symlink)
    }
    let mut removed: BTreeMap<_, Vec<_>> = BTreeMap::new();
    let mut added: BTreeMap<_, Vec<_>> = BTreeMap::new();
    for (path, entry) in base.iter().filter(|(path, _)| !side.contains_key(*path)) {
        removed.entry(key(entry)).or_default().push(path);
    }
    for (path, entry) in side.iter().filter(|(path, _)| !base.contains_key(*path)) {
        added.entry(key(entry)).or_default().push(path);
    }
    let mut renames = BTreeMap::new();
    let mut destinations = BTreeSet::new();
    for (id, sources) in &removed {
        let Some(targets) = added.get(id) else {
            continue;
        };
        if sources.len() != 1 || targets.len() != 1 {
            return Err(Error::Unsupported(Unsupported::AmbiguousRename));
        }
        fn parent(path: &RepoPath) -> &[u8] {
            let bytes = path.as_bytes();
            &bytes[..bytes.iter().rposition(|b| *b == b'/').unwrap_or(0)]
        }
        if parent(sources[0]) != parent(targets[0]) {
            // Directory-move propagation can change paths not present on this
            // side. Do not silently return a path-based approximation of Git.
            return Err(Error::Unsupported(Unsupported::CrossDirectoryRename));
        }
        renames.insert(sources[0].clone(), targets[0].clone());
        destinations.insert(targets[0]);
    }
    let unmatched_removed = removed
        .values()
        .flatten()
        .any(|path| !renames.contains_key(*path));
    let unmatched_added = added
        .values()
        .flatten()
        .any(|path| !destinations.contains(path));
    if unmatched_removed && unmatched_added {
        return Err(Error::Unsupported(Unsupported::PossibleEditedRename));
    }
    Ok(renames)
}
