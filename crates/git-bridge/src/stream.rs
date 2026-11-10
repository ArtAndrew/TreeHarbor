use crate::{Error, HashAlgorithm, Limits, Result, check_deadline, origin, pack};
use std::{collections::BTreeMap, path::Path, time::Instant};
use treeharbor_core::{EntryMode, Object, ObjectId, ObjectStore, RepoPath};

pub(crate) struct Graph {
    pub commits: BTreeMap<ObjectId, String>,
    pub objects: usize,
    pub bytes: u64,
    pub has_origins: bool,
}
fn get(store: &impl ObjectStore, id: ObjectId) -> Result<Object> {
    checked_object(store, id).map(|(object, _)| object)
}

fn checked_object(store: &impl ObjectStore, id: ObjectId) -> Result<(Object, usize)> {
    let object = store.get(id)?;
    let frame = object.encode()?;
    if ObjectId::hash_frame(&frame) != id {
        return Err(Error::InvalidProjection);
    }
    Ok((object, frame.len()))
}

/// Iterative child-first traversal validates *all* typed edges, not only Git
/// projection edges. An inconsistent store or missing delta fails the export.
pub(crate) fn ordered(
    store: &impl ObjectStore,
    refs: &BTreeMap<String, ObjectId>,
    limits: Limits,
    deadline: Instant,
) -> Result<(Vec<ObjectId>, u64)> {
    let mut order = Vec::new();
    let bytes = visit_ordered(store, refs, limits, deadline, |id, _| {
        order.push(id);
        Ok(())
    })?;
    Ok((order, bytes))
}

fn visit_ordered(
    store: &impl ObjectStore,
    refs: &BTreeMap<String, ObjectId>,
    limits: Limits,
    deadline: Instant,
    mut visit: impl FnMut(ObjectId, Option<Object>) -> Result<()>,
) -> Result<u64> {
    let mut state = BTreeMap::<ObjectId, (u8, bool)>::new();
    let mut pending = Vec::new();
    for id in refs.values() {
        pending.push((*id, 5, false));
    }
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while let Some((id, expected, finish)) = pending.pop() {
        check_deadline(deadline)?;
        if finish {
            state.get_mut(&id).ok_or(Error::InvalidProjection)?.1 = true;
            visit(id, None)?;
            continue;
        }
        if let Some((kind, done)) = state.get(&id) {
            if *kind != expected || !done {
                return Err(Error::InvalidProjection);
            }
            continue;
        }
        if state.len() >= limits.objects {
            return Err(Error::Limit);
        }
        let (object, encoded_length) = checked_object(store, id)?;
        if object.kind() != expected {
            return Err(Error::InvalidProjection);
        }
        bytes = bytes
            .checked_add(encoded_length as u64)
            .ok_or(Error::Limit)?;
        if bytes > limits.source_bytes {
            return Err(Error::Limit);
        }
        let refs = object.references();
        edges = edges.checked_add(refs.len()).ok_or(Error::Limit)?;
        if edges > limits.edges {
            return Err(Error::Limit);
        }
        let leaf = refs.is_empty();
        state.insert(id, (expected, leaf));
        if leaf {
            // Project leaves from the bytes already checked by this traversal.
            // Keep only identities for unfinished ancestors, even on deep DAGs.
            visit(id, Some(object))?;
        } else {
            pending.push((id, expected, true));
            pending.extend(refs.into_iter().map(|(id, kind)| (id, kind, false)));
        }
    }
    Ok(bytes)
}

pub(crate) fn check_paths(
    store: &impl ObjectStore,
    root: ObjectId,
    visits: &mut usize,
    limits: Limits,
    deadline: Instant,
) -> Result<usize> {
    let mut entries = 0usize;
    let mut pending = vec![(Vec::new(), root)];
    while let Some((prefix, id)) = pending.pop() {
        check_deadline(deadline)?;
        *visits = visits.checked_add(1).ok_or(Error::Limit)?;
        if *visits > limits.path_visits {
            return Err(Error::Limit);
        }
        let Object::Directory(dir) = get(store, id)? else {
            return Err(Error::InvalidProjection);
        };
        if !prefix.is_empty() && dir.entries().is_empty() {
            return Err(Error::Unsupported("nested empty directory"));
        }
        for entry in dir.entries() {
            entries += 1;
            *visits = visits.checked_add(1).ok_or(Error::Limit)?;
            if *visits > limits.path_visits {
                return Err(Error::Limit);
            }
            let mut path = prefix.clone();
            path.extend_from_slice(entry.name.as_bytes());
            RepoPath::new(path.clone())?;
            if entry.value.mode == EntryMode::Directory {
                path.push(b'/');
                pending.push((path, entry.value.target));
            }
        }
    }
    Ok(entries)
}
pub(crate) fn write(
    store: &impl ObjectStore,
    refs: &BTreeMap<String, ObjectId>,
    path: &Path,
    algorithm: HashAlgorithm,
    limits: Limits,
    deadline: Instant,
) -> Result<Graph> {
    let mut pack = pack::Pack::new(path, algorithm, limits.stream_bytes, deadline)?;
    let mut ids = BTreeMap::<ObjectId, Vec<u8>>::new();
    let mut commits = BTreeMap::new();
    let mut visits = 0usize;
    let mut has_origins = false;
    let mut objects = 0;
    let bytes = visit_ordered(store, refs, limits, deadline, |id, leaf| {
        objects += 1;
        let object = match leaf {
            Some(checked) => checked,
            None => get(store, id)?,
        };
        let oid = match object {
            Object::Blob(bytes) => pack.put("blob", &bytes)?,
            Object::Directory(dir) => {
                // Git orders a directory name as if it had a trailing slash.
                let mut sorted: Vec<_> = dir
                    .entries()
                    .iter()
                    .map(|entry| {
                        let mut key = entry.name.as_bytes().to_vec();
                        if entry.value.mode == EntryMode::Directory {
                            key.push(b'/');
                        }
                        (key, entry)
                    })
                    .collect();
                sorted.sort_by(|a, b| a.0.cmp(&b.0));
                let mut raw = Vec::new();
                for (_, entry) in sorted {
                    raw.extend_from_slice(format!("{:o} ", entry.value.mode as u32).as_bytes());
                    raw.extend_from_slice(entry.name.as_bytes());
                    raw.push(0);
                    raw.extend_from_slice(
                        ids.get(&entry.value.target)
                            .ok_or(Error::InvalidProjection)?,
                    );
                }
                pack.put("tree", &raw)?
            }
            Object::Snapshot { root } => ids.get(&root).ok_or(Error::InvalidProjection)?.clone(),
            Object::NativeCommit(c) => {
                let Object::Snapshot { root } = get(store, c.snapshot)? else {
                    return Err(Error::InvalidProjection);
                };
                check_paths(store, root, &mut visits, limits, deadline)?;
                let tree = ids.get(&c.snapshot).ok_or(Error::InvalidProjection)?;
                let parents: Vec<_> = c
                    .parents
                    .iter()
                    .map(|p| ids.get(p).cloned().ok_or(Error::InvalidProjection))
                    .collect::<Result<_>>()?;
                let raw = if let Some(origin) = &c.git_origin {
                    has_origins = true;
                    let Object::Blob(raw) = get(store, origin.raw)? else {
                        return Err(Error::InvalidProjection);
                    };
                    origin::verify(&c, &raw, tree, &parents, algorithm)?;
                    raw
                } else {
                    origin::native_payload(&c, tree, &parents)?
                };
                let oid = pack.put("commit", &raw)?;
                commits.insert(id, origin::hex(&oid));
                oid
            }
            Object::ChangeOperations(_) => return Ok(()),
        };
        ids.insert(id, oid);
        Ok(())
    })?;
    pack.finish()?;
    if std::fs::metadata(path)?.len() > limits.stream_bytes {
        return Err(Error::Limit);
    }
    Ok(Graph {
        commits,
        objects,
        bytes,
        has_origins,
    })
}
