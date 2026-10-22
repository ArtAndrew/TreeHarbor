use crate::{Error, Limits, Result, check_deadline};
use std::{collections::BTreeMap, fs::File, io::Write, path::Path, time::Instant};
use treeharbor_core::{EntryMode, Object, ObjectId, ObjectStore, RepoPath};

pub(crate) struct Graph {
    pub commits: BTreeMap<ObjectId, usize>,
    pub marks: usize,
    pub objects: usize,
    pub bytes: u64,
}
struct Output {
    file: File,
    bytes: u64,
    limit: u64,
}
impl Output {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.bytes = self
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or(Error::Limit)?;
        if self.bytes > self.limit {
            return Err(Error::Limit);
        }
        self.file.write_all(bytes)?;
        Ok(())
    }
    fn data(&mut self, bytes: &[u8]) -> Result<()> {
        self.write(format!("data {}\n", bytes.len()).as_bytes())?;
        self.write(bytes)?;
        self.write(b"\n")
    }
}
fn get(store: &impl ObjectStore, id: ObjectId) -> Result<Object> {
    let object = store.get(id)?;
    if object.id()? != id {
        return Err(Error::InvalidProjection);
    }
    Ok(object)
}
/// Iterative child-first traversal validates *all* typed edges, not only Git
/// projection edges. An inconsistent store or missing delta fails the export.
fn ordered(
    store: &impl ObjectStore,
    refs: &BTreeMap<String, ObjectId>,
    limits: Limits,
    deadline: Instant,
) -> Result<(Vec<ObjectId>, u64)> {
    let mut state = BTreeMap::<ObjectId, (u8, bool)>::new();
    let mut pending = Vec::new();
    for id in refs.values() {
        pending.push((*id, 5, false));
    }
    let mut result = Vec::new();
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while let Some((id, expected, finish)) = pending.pop() {
        check_deadline(deadline)?;
        if finish {
            state.get_mut(&id).ok_or(Error::InvalidProjection)?.1 = true;
            result.push(id);
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
        let object = get(store, id)?;
        if object.kind() != expected {
            return Err(Error::InvalidProjection);
        }
        bytes = bytes
            .checked_add(object.encode()?.len() as u64)
            .ok_or(Error::Limit)?;
        if bytes > limits.source_bytes {
            return Err(Error::Limit);
        }
        let refs = object.references();
        edges = edges.checked_add(refs.len()).ok_or(Error::Limit)?;
        if edges > limits.edges {
            return Err(Error::Limit);
        }
        state.insert(id, (expected, false));
        pending.push((id, expected, true));
        pending.extend(refs.into_iter().map(|(id, kind)| (id, kind, false)));
    }
    Ok((result, bytes))
}

pub(crate) fn write(
    store: &impl ObjectStore,
    refs: &BTreeMap<String, ObjectId>,
    path: &Path,
    limits: Limits,
    deadline: Instant,
) -> Result<Graph> {
    let (order, bytes) = ordered(store, refs, limits, deadline)?;
    let mut out = Output {
        file: File::create(path)?,
        bytes: 0,
        limit: limits.stream_bytes,
    };
    let mut marks = BTreeMap::new();
    let mut commits = BTreeMap::new();
    let mut visits = 0usize;
    for id in &order {
        check_deadline(deadline)?;
        match get(store, *id)? {
            Object::Blob(bytes) => {
                let mark = marks.len() + 1;
                marks.insert(*id, mark);
                out.write(format!("blob\nmark :{mark}\n").as_bytes())?;
                out.data(&bytes)?;
            }
            Object::NativeCommit(c) => {
                if c.timestamp_seconds < 0
                    || c.message.contains('\0')
                    || c.author_name.trim() != c.author_name
                    || c.author_name.bytes().any(|b| b.is_ascii_control())
                    || c.author_email
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                {
                    return Err(Error::Unsupported(
                        "Git-incompatible author, date or message",
                    ));
                }
                let mark = marks.len() + 1;
                marks.insert(*id, mark);
                commits.insert(*id, mark);
                // Always reset: otherwise fast-import implicitly adds the previous
                // branch tip as a parent to a second independent root.
                out.write(b"reset refs/treeharbor/export-staging\n\ncommit refs/treeharbor/export-staging\n")?;
                out.write(format!("mark :{mark}\n").as_bytes())?;
                let offset = c.timezone_minutes.unsigned_abs();
                let identity = format!(
                    "{} <{}> {} {}{:02}{:02}\n",
                    c.author_name,
                    c.author_email,
                    c.timestamp_seconds,
                    if c.timezone_minutes < 0 { '-' } else { '+' },
                    offset / 60,
                    offset % 60
                );
                out.write(format!("author {identity}committer {identity}").as_bytes())?;
                out.data(c.message.as_bytes())?;
                for (i, parent) in c.parents.iter().enumerate() {
                    let mark = commits.get(parent).ok_or(Error::InvalidProjection)?;
                    out.write(
                        format!("{} :{mark}\n", if i == 0 { "from" } else { "merge" }).as_bytes(),
                    )?;
                }
                out.write(b"deleteall\n")?;
                let Object::Snapshot { root } = get(store, c.snapshot)? else {
                    return Err(Error::InvalidProjection);
                };
                let mut pending = vec![(Vec::new(), root)];
                while let Some((prefix, id)) = pending.pop() {
                    check_deadline(deadline)?;
                    visits = visits.checked_add(1).ok_or(Error::Limit)?;
                    if visits > limits.path_visits {
                        return Err(Error::Limit);
                    }
                    let Object::Directory(dir) = get(store, id)? else {
                        return Err(Error::InvalidProjection);
                    };
                    if !prefix.is_empty() && dir.entries().is_empty() {
                        return Err(Error::Unsupported("nested empty directory"));
                    }
                    for entry in dir.entries() {
                        visits = visits.checked_add(1).ok_or(Error::Limit)?;
                        if visits > limits.path_visits {
                            return Err(Error::Limit);
                        }
                        let mut path = prefix.clone();
                        path.extend_from_slice(entry.name.as_bytes());
                        RepoPath::new(path.clone())?;
                        if entry.value.mode == EntryMode::Directory {
                            path.push(b'/');
                            pending.push((path, entry.value.target));
                        } else {
                            let mark = marks
                                .get(&entry.value.target)
                                .ok_or(Error::InvalidProjection)?;
                            out.write(
                                format!("M {:o} :{mark} \"", entry.value.mode as u32).as_bytes(),
                            )?;
                            // Fixed-width octal C quoting preserves every path byte,
                            // including LF, quotes, spaces and non-UTF-8 names.
                            for b in path {
                                out.write(format!("\\{b:03o}").as_bytes())?;
                            }
                            out.write(b"\"\n")?;
                        }
                    }
                }
                out.write(b"\n")?;
            }
            _ => {}
        }
    }
    for (name, id) in refs {
        let mark = commits.get(id).ok_or(Error::InvalidProjection)?;
        out.write(format!("reset {name}\nfrom :{mark}\n\n").as_bytes())?;
    }
    out.write(b"reset refs/treeharbor/export-staging\n\ndone\n")?;
    out.file.sync_all()?;
    Ok(Graph {
        commits,
        marks: marks.len(),
        objects: order.len(),
        bytes,
    })
}
