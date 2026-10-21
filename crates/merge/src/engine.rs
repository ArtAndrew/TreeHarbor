use crate::{
    Conflict, ConflictKind, Error, Limits, MergedSnapshot, Outcome, Result, Unsupported,
    checked_put,
    renames::{self, Tree},
};
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::{
    Directory, Entry, EntryMode, EntryRef, Name, Object, ObjectId, ObjectStore, RepoPath,
};

struct Context<'a, S> {
    store: &'a S,
    limits: Limits,
    directories: usize,
    entries: usize,
    path_bytes: usize,
    read_bytes: usize,
    write_bytes: usize,
    diff_work: usize,
    staged: BTreeMap<ObjectId, Object>,
    order: Vec<ObjectId>,
}

fn charge(used: &mut usize, amount: usize, limit: usize, name: &'static str) -> Result<()> {
    *used = used
        .checked_add(amount)
        .filter(|v| *v <= limit)
        .ok_or(Error::Limit(name))?;
    Ok(())
}

impl<S: ObjectStore> Context<'_, S> {
    fn get(&mut self, id: ObjectId) -> Result<Object> {
        let object = self.store.get(id)?;
        let frame = object.encode()?;
        charge(
            &mut self.read_bytes,
            frame.len(),
            self.limits.max_read_bytes,
            "read bytes",
        )?;
        if ObjectId::hash_frame(&frame) != id {
            return Err(treeharbor_core::Error::InvalidObject("wrong merge read identity").into());
        }
        Ok(object)
    }

    fn stage(&mut self, object: Object) -> Result<ObjectId> {
        let frame = object.encode()?;
        let id = ObjectId::hash_frame(&frame);
        if !self.staged.contains_key(&id) {
            charge(
                &mut self.write_bytes,
                frame.len(),
                self.limits.max_write_bytes,
                "write bytes",
            )?;
            self.order.push(id);
            self.staged.insert(id, object);
        }
        Ok(id)
    }

    fn root(&mut self, snapshot: ObjectId) -> Result<ObjectId> {
        match self.get(snapshot)? {
            Object::Snapshot { root } => Ok(root),
            _ => Err(treeharbor_core::Error::WrongKind.into()),
        }
    }

    fn directory(&mut self, id: ObjectId) -> Result<Directory> {
        charge(
            &mut self.directories,
            1,
            self.limits.max_directories,
            "directories",
        )?;
        match self.get(id)? {
            Object::Directory(dir) => {
                charge(
                    &mut self.entries,
                    dir.entries().len(),
                    self.limits.max_entries,
                    "entries",
                )?;
                Ok(dir)
            }
            _ => Err(treeharbor_core::Error::WrongKind.into()),
        }
    }

    fn flatten(&mut self, root: ObjectId) -> Result<Tree> {
        let mut result = Tree::new();
        let mut pending = vec![(Vec::new(), root)];
        while let Some((prefix, id)) = pending.pop() {
            let dir = self.directory(id)?;
            if dir.entries().is_empty() && !prefix.is_empty() {
                return Err(Error::Unsupported(Unsupported::EmptyDirectory(
                    RepoPath::new(prefix)?,
                )));
            }
            for entry in dir.entries() {
                let mut path = prefix.clone();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(entry.name.as_bytes());
                let path = RepoPath::new(path)?;
                charge(
                    &mut self.path_bytes,
                    path.as_bytes().len(),
                    self.limits.max_path_bytes,
                    "path bytes",
                )?;
                if entry.name.as_bytes() == b".gitattributes" {
                    return Err(Error::Unsupported(Unsupported::Attributes));
                }
                if entry.value.mode == EntryMode::Directory {
                    pending.push((path.as_bytes().to_vec(), entry.value.target));
                } else {
                    result.insert(path, entry.value);
                }
            }
        }
        Ok(result)
    }

    fn blob(&mut self, id: ObjectId) -> Result<Vec<u8>> {
        match self.get(id)? {
            Object::Blob(bytes) => Ok(bytes),
            _ => Err(treeharbor_core::Error::WrongKind.into()),
        }
    }

    fn merge_entry(
        &mut self,
        base: Option<EntryRef>,
        current: Option<EntryRef>,
        proposed: Option<EntryRef>,
    ) -> Result<std::result::Result<Option<EntryRef>, ConflictKind>> {
        if current == proposed {
            return Ok(Ok(current));
        }
        if current == base {
            return Ok(Ok(proposed));
        }
        if proposed == base {
            return Ok(Ok(current));
        }
        let (Some(left), Some(right)) = (current, proposed) else {
            return Ok(Err(ConflictKind::ModifyDelete));
        };
        let is_link = |e: EntryRef| e.mode == EntryMode::Symlink;
        if is_link(left) != is_link(right) || base.is_some_and(|b| is_link(b) != is_link(left)) {
            return Ok(Err(ConflictKind::Type));
        }
        if is_link(left) {
            return Ok(Err(ConflictKind::Symlink));
        }
        let mode = if left.mode == right.mode || base.is_some_and(|b| right.mode == b.mode) {
            left.mode
        } else if base.is_some_and(|b| left.mode == b.mode) {
            right.mode
        } else {
            return Ok(Err(ConflictKind::Mode));
        };
        let target =
            if left.target == right.target || base.is_some_and(|b| right.target == b.target) {
                left.target
            } else if base.is_some_and(|b| left.target == b.target) {
                right.target
            } else {
                let ancestor = match base {
                    Some(b) => self.blob(b.target)?,
                    None => Vec::new(),
                };
                let ours = self.blob(left.target)?;
                let theirs = self.blob(right.target)?;
                if [&ancestor, &ours, &theirs].iter().any(|b| b.contains(&0)) {
                    return Ok(Err(ConflictKind::Binary));
                }
                let bytes = ancestor
                    .len()
                    .saturating_add(ours.len())
                    .saturating_add(theirs.len());
                if bytes > self.limits.max_text_bytes {
                    return Err(Error::Limit("text bytes"));
                }
                let lines: usize = [&ancestor, &ours, &theirs]
                    .into_iter()
                    .map(|b| {
                        b.iter().filter(|c| **c == b'\n').count()
                            + usize::from(!b.is_empty() && b.last() != Some(&b'\n'))
                    })
                    .sum();
                if lines > self.limits.max_text_lines {
                    return Err(Error::Limit("text lines"));
                }
                charge(
                    &mut self.diff_work,
                    lines.saturating_mul(lines),
                    self.limits.max_diff_work,
                    "text work",
                )?;
                match diffy::merge_bytes(&ancestor, &ours, &theirs) {
                    Ok(bytes) => self.stage(Object::Blob(bytes))?,
                    Err(_) => return Ok(Err(ConflictKind::Content)),
                }
            };
        Ok(Ok(Some(EntryRef { mode, target })))
    }
}

#[derive(Default)]
struct Node {
    files: BTreeMap<Name, EntryRef>,
    directories: BTreeMap<Name, Node>,
}

impl Node {
    fn insert(&mut self, path: &RepoPath, entry: EntryRef) -> Result<()> {
        let mut node = self;
        let mut parts = path.components().peekable();
        while let Some(part) = parts.next() {
            let name = Name::new(part.to_vec())?;
            if parts.peek().is_some() {
                node = node.directories.entry(name).or_default();
            } else {
                node.files.insert(name, entry);
            }
        }
        Ok(())
    }

    fn stage<S: ObjectStore>(self, context: &mut Context<'_, S>) -> Result<ObjectId> {
        let mut entries: Vec<_> = self
            .files
            .into_iter()
            .map(|(name, value)| Entry { name, value })
            .collect();
        for (name, node) in self.directories {
            entries.push(Entry {
                name,
                value: EntryRef {
                    mode: EntryMode::Directory,
                    target: node.stage(context)?,
                },
            });
        }
        context.stage(Object::Directory(Directory::new(entries)?))
    }
}

fn record(
    conflicts: &mut Vec<Conflict>,
    path: &RepoPath,
    related_path: Option<RepoPath>,
    kind: ConflictKind,
    base: Option<EntryRef>,
    current: Option<EntryRef>,
    proposed: Option<EntryRef>,
) {
    conflicts.push(Conflict {
        path: path.clone(),
        related_path,
        kind,
        base,
        current,
        proposed,
    });
}

/// Merge an explicit base with the current target and an immutable proposal.
/// A clean result is only a new computation input for candidate construction.
/// Input graphs must be protected, verified source graphs; unchanged blobs are
/// reused by identity and are not traversed. No refs or validation records change.
pub fn merge_snapshots(
    store: &impl ObjectStore,
    base: ObjectId,
    current: ObjectId,
    proposed: ObjectId,
    limits: Limits,
) -> Result<Outcome> {
    let mut cx = Context {
        store,
        limits,
        directories: 0,
        entries: 0,
        path_bytes: 0,
        read_bytes: 0,
        write_bytes: 0,
        diff_work: 0,
        staged: BTreeMap::new(),
        order: Vec::new(),
    };
    let [b, l, r] = [cx.root(base)?, cx.root(current)?, cx.root(proposed)?];
    let receipt = |result| {
        Outcome::Clean(MergedSnapshot {
            base,
            current,
            proposed,
            result,
        })
    };
    if l == r || b == r || b == l {
        // The trusted graph contract permits whole-tree reuse without re-reading
        // all leaves. Still reject a snapshot whose root is not a directory.
        for root in BTreeSet::from([b, l, r]) {
            cx.directory(root)?;
        }
        return Ok(receipt(if b == l { proposed } else { current }));
    }
    let ancestor = cx.flatten(b)?;
    let left = cx.flatten(l)?;
    let right = cx.flatten(r)?;
    let lr = renames::detect(&ancestor, &left)?;
    let rr = renames::detect(&ancestor, &right)?;
    let consumed_l: BTreeSet<_> = lr.values().collect();
    let consumed_r: BTreeSet<_> = rr.values().collect();
    let mut result = Tree::new();
    let mut conflicts = Vec::new();
    for (path, before) in &ancestor {
        let lp = lr.get(path).unwrap_or(path);
        let rp = rr.get(path).unwrap_or(path);
        let lv = left.get(lp).copied();
        let rv = right.get(rp).copied();
        if lp != path && rp != path && lp != rp {
            record(
                &mut conflicts,
                lp,
                Some(rp.clone()),
                ConflictKind::RenameRename,
                Some(*before),
                lv,
                rv,
            );
            continue;
        }
        let destination = if lp != path { lp } else { rp };
        if (lp != path && rv.is_none()) || (rp != path && lv.is_none()) {
            record(
                &mut conflicts,
                destination,
                Some(path.clone()),
                ConflictKind::RenameDelete,
                Some(*before),
                lv,
                rv,
            );
            continue;
        }
        match cx.merge_entry(Some(*before), lv, rv)? {
            Ok(Some(entry)) => {
                if let Some(existing) = result.insert(destination.clone(), entry) {
                    record(
                        &mut conflicts,
                        destination,
                        Some(path.clone()),
                        ConflictKind::DestinationCollision,
                        None,
                        Some(existing),
                        Some(entry),
                    );
                }
            }
            Ok(None) => {}
            Err(kind) => record(
                &mut conflicts,
                destination,
                (destination != path).then(|| path.clone()),
                kind,
                Some(*before),
                lv,
                rv,
            ),
        }
    }
    let additions: BTreeSet<_> = left
        .keys()
        .chain(right.keys())
        .filter(|p| !ancestor.contains_key(*p))
        .collect();
    for path in additions {
        let lv = (!consumed_l.contains(path))
            .then(|| left.get(path).copied())
            .flatten();
        let rv = (!consumed_r.contains(path))
            .then(|| right.get(path).copied())
            .flatten();
        if lv.is_none() && rv.is_none() {
            continue;
        }
        if let Some(existing) = result.get(path) {
            record(
                &mut conflicts,
                path,
                None,
                ConflictKind::DestinationCollision,
                None,
                Some(*existing),
                lv.or(rv),
            );
            continue;
        }
        match cx.merge_entry(None, lv, rv)? {
            Ok(Some(entry)) => {
                result.insert(path.clone(), entry);
            }
            Ok(None) => {}
            Err(kind) => record(&mut conflicts, path, None, kind, None, lv, rv),
        }
    }
    // Check every prefix, not just sorted neighbors: `a-1` sorts between `a`
    // and `a/b`, and must not conceal a file/directory collision.
    for path in result.keys() {
        for (i, byte) in path.as_bytes().iter().enumerate() {
            if *byte != b'/' {
                continue;
            }
            let prefix = RepoPath::new(path.as_bytes()[..i].to_vec())?;
            if result.contains_key(&prefix) {
                record(
                    &mut conflicts,
                    &prefix,
                    Some(path.clone()),
                    ConflictKind::FileDirectory,
                    ancestor.get(&prefix).copied(),
                    left.get(&prefix).copied(),
                    right.get(&prefix).copied(),
                );
            }
        }
    }
    if !conflicts.is_empty() {
        conflicts.sort_by(|a, b| {
            a.path
                .cmp(&b.path)
                .then(a.related_path.cmp(&b.related_path))
        });
        return Ok(Outcome::Conflicted(conflicts));
    }
    let mut node = Node::default();
    for (path, entry) in result {
        node.insert(&path, entry)?;
    }
    let root = node.stage(&mut cx)?;
    let snapshot = cx.stage(Object::Snapshot { root })?;
    // Children were staged before parents; conflict/limit computation never writes.
    // Store failure here can leave unreachable objects, never a published ref.
    for id in &cx.order {
        checked_put(store, &cx.staged[id])?;
    }
    Ok(receipt(snapshot))
}

pub(crate) fn snapshot_delta(
    store: &impl ObjectStore,
    before: ObjectId,
    after: ObjectId,
    limits: Limits,
) -> Result<treeharbor_core::Changes> {
    use treeharbor_core::{ChangeOperation, Changes};
    let mut cx = Context {
        store,
        limits,
        directories: 0,
        entries: 0,
        path_bytes: 0,
        read_bytes: 0,
        write_bytes: 0,
        diff_work: 0,
        staged: BTreeMap::new(),
        order: Vec::new(),
    };
    let b = cx.root(before)?;
    let a = cx.root(after)?;
    if b == a {
        return Ok(Changes::new(vec![])?);
    }
    let before = cx.flatten(b)?;
    let after = cx.flatten(a)?;
    let paths: BTreeSet<_> = before.keys().chain(after.keys()).collect();
    let mut operations = Vec::new();
    for path in paths {
        let expected = before.get(path).copied();
        let replacement = after.get(path).copied();
        if expected != replacement {
            operations.push(ChangeOperation {
                path: path.clone(),
                expected,
                replacement,
            });
        }
    }
    // Existing v1 leaf operations cannot represent an ancestor/descendant
    // replacement atomically. Reject it; never write an empty invented delta.
    Ok(Changes::new(operations)?)
}
