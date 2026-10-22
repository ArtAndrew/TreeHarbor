//! Mechanical tree difference, not three-way merge or publication authority.
use crate::{
    ChangeOperation, Changes, Directory, EntryMode, EntryRef, Error, Object, ObjectId, ObjectStore,
    RepoPath, Result,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug)]
pub struct TreeDiffLimits {
    pub directories: usize,
    pub entries: usize,
    pub path_bytes: usize,
    pub read_bytes: usize,
    pub operations: usize,
}
impl Default for TreeDiffLimits {
    fn default() -> Self {
        Self {
            directories: 20_000,
            entries: 200_000,
            path_bytes: 32 << 20,
            read_bytes: 128 << 20,
            operations: crate::MAX_ENTRIES,
        }
    }
}
struct Reader<'a, S> {
    store: &'a S,
    remaining: TreeDiffLimits,
}
fn charge(budget: &mut usize, amount: usize) -> Result<()> {
    *budget = budget.checked_sub(amount).ok_or(Error::TooLarge)?;
    Ok(())
}
impl<S: ObjectStore> Reader<'_, S> {
    fn get(&mut self, id: ObjectId) -> Result<Object> {
        let object = self.store.get(id)?;
        let frame = object.encode()?;
        charge(&mut self.remaining.read_bytes, frame.len())?;
        if ObjectId::hash_frame(&frame) != id {
            return Err(Error::InvalidObject("tree diff read identity"));
        }
        Ok(object)
    }
    fn root(&mut self, snapshot: ObjectId) -> Result<ObjectId> {
        match self.get(snapshot)? {
            Object::Snapshot { root } => Ok(root),
            _ => Err(Error::WrongKind),
        }
    }
    fn directory(&mut self, id: ObjectId) -> Result<Directory> {
        charge(&mut self.remaining.directories, 1)?;
        match self.get(id)? {
            Object::Directory(d) => {
                charge(&mut self.remaining.entries, d.entries().len())?;
                Ok(d)
            }
            _ => Err(Error::WrongKind),
        }
    }
}
/// Produces nonoverlapping exact-before operations from two protected snapshots.
/// Equal subtrees are skipped; blobs are never read and no objects are written.
/// Inputs must be complete validated source graphs held by the caller. This is
/// not a substitute for upload/restore graph verification or a GC read lease.
/// The caller replays the result before publishing a claimed snapshot.
pub fn diff_snapshots(
    store: &impl ObjectStore,
    before: ObjectId,
    after: ObjectId,
    limits: TreeDiffLimits,
) -> Result<Changes> {
    let mut reader = Reader {
        store,
        remaining: TreeDiffLimits {
            operations: limits.operations.min(crate::MAX_ENTRIES),
            ..limits
        },
    };
    let before_root = reader.root(before)?;
    let after_root = reader.root(after)?;
    let b = reader.directory(before_root)?;
    if before_root == after_root {
        return Changes::new(vec![]);
    }
    let a = reader.directory(after_root)?;
    let mut pending = vec![(Vec::new(), b, a)];
    let mut operations = Vec::new();
    while let Some((prefix, b, a)) = pending.pop() {
        let b: BTreeMap<_, _> = b
            .entries()
            .iter()
            .map(|e| (e.name.clone(), e.value))
            .collect();
        let a: BTreeMap<_, _> = a
            .entries()
            .iter()
            .map(|e| (e.name.clone(), e.value))
            .collect();
        let names: BTreeSet<_> = b.keys().chain(a.keys()).collect();
        for name in names {
            let expected = b.get(name).copied();
            let replacement = a.get(name).copied();
            if expected == replacement {
                continue;
            }
            let mut path = prefix.clone();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(name.as_bytes());
            charge(&mut reader.remaining.path_bytes, path.len())?;
            let path = RepoPath::new(path)?;
            if let (
                Some(EntryRef {
                    mode: EntryMode::Directory,
                    target: before,
                }),
                Some(EntryRef {
                    mode: EntryMode::Directory,
                    target: after,
                }),
            ) = (expected, replacement)
            {
                let old = reader.directory(before)?;
                let new = reader.directory(after)?;
                // Descending into an empty result would let leaf deletion prune
                // the directory, losing the explicit empty-directory entry.
                if !new.entries().is_empty() {
                    pending.push((path.as_bytes().to_vec(), old, new));
                    continue;
                }
            }
            charge(&mut reader.remaining.operations, 1)?;
            operations.push(ChangeOperation {
                path,
                expected,
                replacement,
            });
        }
    }
    Changes::with_subtrees(operations)
}
