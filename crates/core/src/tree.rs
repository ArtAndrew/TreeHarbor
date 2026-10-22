use crate::{
    Changes, Directory, Entry, EntryMode, EntryRef, Error, Name, Object, ObjectId, ObjectStore,
    RepoPath, Result,
};
use std::collections::{BTreeMap, BTreeSet};

fn directory(store: &impl ObjectStore, id: ObjectId) -> Result<Directory> {
    match store.get(id)? {
        Object::Directory(d) => Ok(d),
        _ => Err(Error::WrongKind),
    }
}

pub fn lookup(
    store: &impl ObjectStore,
    root: ObjectId,
    path: &RepoPath,
) -> Result<Option<EntryRef>> {
    let parts: Vec<_> = path.components().collect();
    let mut current = root;
    for (i, part) in parts.iter().enumerate() {
        let Some(entry) = directory(store, current)?.find(part) else {
            return Ok(None);
        };
        if i == parts.len() - 1 {
            return Ok(Some(entry));
        }
        if entry.mode != EntryMode::Directory {
            return Err(Error::InvalidPath("ancestor is a leaf"));
        }
        current = entry.target;
    }
    Err(Error::InvalidPath("empty path"))
}

/// Writes immutable intermediate objects, but never publishes a root. Callers
/// must reserve objects against GC and atomically publish/pin in metadata.
pub fn apply_changes(
    store: &impl ObjectStore,
    root: ObjectId,
    changes: &Changes,
) -> Result<ObjectId> {
    directory(store, root)?;
    for op in changes.operations() {
        if lookup(store, root, &op.path)? != op.expected {
            return Err(Error::PredicateMismatch);
        }
        if let Some(entry) = op.replacement {
            let kind = store.get(entry.target)?.kind();
            let expected = if entry.mode == EntryMode::Directory {
                2
            } else {
                1
            };
            if kind != expected {
                return Err(Error::WrongKind);
            }
        }
    }
    let mut result = root;
    for op in changes.operations() {
        let parts: Vec<_> = op.path.components().collect();
        result = replace_path(store, result, &parts, op.replacement)?;
    }
    Ok(result)
}

fn replace_path(
    store: &impl ObjectStore,
    root: ObjectId,
    parts: &[&[u8]],
    replacement: Option<EntryRef>,
) -> Result<ObjectId> {
    let dir = directory(store, root)?;
    let mut entries: BTreeMap<_, _> = dir
        .entries()
        .iter()
        .map(|e| (e.name.clone(), e.value))
        .collect();
    let name = Name::new(parts[0].to_vec())?;
    if parts.len() == 1 {
        if let Some(entry) = replacement {
            entries.insert(name, entry);
        } else {
            entries.remove(&name);
        }
    } else {
        let child = match entries.get(&name) {
            Some(e) if e.mode == EntryMode::Directory => e.target,
            Some(_) => return Err(Error::InvalidPath("ancestor is a leaf")),
            None => store.put(&Object::Directory(Directory::new(vec![])?))?,
        };
        let new_child = replace_path(store, child, &parts[1..], replacement)?;
        if directory(store, new_child)?.entries().is_empty() {
            entries.remove(&name);
        } else {
            entries.insert(
                name,
                EntryRef {
                    mode: EntryMode::Directory,
                    target: new_child,
                },
            );
        }
    }
    store.put(&Object::Directory(Directory::new(
        entries
            .into_iter()
            .map(|(name, value)| Entry { name, value })
            .collect(),
    )?))
}

/// Iterative, bounded traversal across all edges, including every commit parent.
/// This verifier is an oracle for acceptance/restore; it is not a GC algorithm.
pub fn validate_graph(
    store: &impl ObjectStore,
    root: ObjectId,
    max_objects: usize,
) -> Result<BTreeSet<ObjectId>> {
    let mut seen = BTreeSet::new();
    let mut pending = vec![(root, None)];
    let mut kinds = BTreeMap::new();
    while let Some((id, expected_kind)) = pending.pop() {
        let kind = if let Some(kind) = kinds.get(&id) {
            *kind
        } else {
            if seen.len() >= max_objects {
                return Err(Error::TooLarge);
            }
            let object = store.get(id)?;
            if object.id()? != id {
                return Err(Error::InvalidObject("store returned wrong object"));
            }
            let kind = object.kind();
            seen.insert(id);
            kinds.insert(id, kind);
            let refs = object.references();
            // Keep the verifier bounded even for an untrusted graph with many
            // edges pointing at one node. Large restores use a separate budget.
            if pending.len().saturating_add(refs.len()) > max_objects.saturating_mul(2) {
                return Err(Error::TooLarge);
            }
            pending.extend(refs.into_iter().map(|(id, kind)| (id, Some(kind))));
            kind
        };
        if expected_kind.is_some_and(|expected| expected != kind) {
            return Err(Error::WrongKind);
        }
    }
    Ok(seen)
}
