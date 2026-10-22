//! Complete history of one pinned Git commit; no ref authority or network fetch.
use crate::{
    Error, HashAlgorithm, Limits, Result, check_deadline, check_limits, origin, process, stream,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Instant,
};
use treeharbor_core::{
    Commit, Directory, Entry, EntryMode, EntryRef, GitOid, GitOrigin, MAX_ENTRIES, MAX_PARENTS,
    MAX_PAYLOAD, Name, Object, ObjectId, ObjectStore, TreeDiffLimits, diff_snapshots,
    prepare_change,
};

pub struct Import<'a> {
    pub git: &'a Path,
    pub bundle: &'a Path,
    /// Existing private directory owned by the operator, outside any agent tree.
    pub scratch: &'a Path,
    pub commit: GitOid,
    pub limits: Limits,
}
pub struct Imported {
    pub commit: ObjectId,
    pub snapshot: ObjectId,
    /// Exact reachable native closure in child-before-parent order.
    pub objects: Vec<ObjectId>,
    pub encoded_bytes: u64,
    pub filesystem_entries: usize,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Blob,
    Tree,
    Commit,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Blob => "blob",
            Self::Tree => "tree",
            Self::Commit => "commit",
        }
    }
}
struct Record {
    kind: Kind,
    length: usize,
    offset: u64,
}
struct Source {
    file: File,
    records: BTreeMap<Vec<u8>, Record>,
    algorithm: HashAlgorithm,
    deadline: Instant,
}
fn line(reader: &mut impl BufRead) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    (&mut *reader).take(256).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(line);
    }
    if line.pop() != Some(b'\n') {
        return Err(Error::Limit);
    }
    Ok(line)
}
impl Source {
    fn open(
        file: File,
        oids: &[Vec<u8>],
        algorithm: HashAlgorithm,
        limits: Limits,
        deadline: Instant,
    ) -> Result<Self> {
        let mut reader = BufReader::new(file);
        let mut records = BTreeMap::new();
        let mut bytes = 0u64;
        for expected in oids {
            check_deadline(deadline)?;
            let header = line(&mut reader)?;
            let fields: Vec<_> = header.split(|b| *b == b' ').collect();
            if fields.len() != 3 || origin::parse_oid(fields[0], algorithm)? != *expected {
                return Err(Error::InvalidProjection);
            }
            let kind = match fields[1] {
                b"blob" => Kind::Blob,
                b"tree" => Kind::Tree,
                b"commit" => Kind::Commit,
                _ => return Err(Error::Unsupported("Git object type")),
            };
            let size = std::str::from_utf8(fields[2]).map_err(|_| Error::InvalidProjection)?;
            let length: usize = size.parse().map_err(|_| Error::InvalidProjection)?;
            if length > MAX_PAYLOAD || length.to_string() != size {
                return Err(Error::Limit);
            }
            bytes = bytes.checked_add(length as u64).ok_or(Error::Limit)?;
            if bytes > limits.source_bytes {
                return Err(Error::Limit);
            }
            let offset = reader.stream_position()?;
            reader.seek(SeekFrom::Current(length as i64))?;
            let mut delimiter = [0];
            reader.read_exact(&mut delimiter)?;
            if delimiter != *b"\n"
                || records
                    .insert(
                        expected.clone(),
                        Record {
                            kind,
                            length,
                            offset,
                        },
                    )
                    .is_some()
            {
                return Err(Error::InvalidProjection);
            }
        }
        let mut extra = [0];
        if reader.read(&mut extra)? != 0 {
            return Err(Error::InvalidProjection);
        }
        Ok(Self {
            file: reader.into_inner(),
            records,
            algorithm,
            deadline,
        })
    }
    fn get(&mut self, oid: &[u8], kind: Kind) -> Result<Vec<u8>> {
        check_deadline(self.deadline)?;
        let r = self.records.get(oid).ok_or(Error::InvalidProjection)?;
        if r.kind != kind {
            return Err(Error::InvalidProjection);
        }
        self.file.seek(SeekFrom::Start(r.offset))?;
        let mut raw = vec![0; r.length];
        self.file.read_exact(&mut raw)?;
        if origin::hash(kind.name(), &raw, self.algorithm)? != oid {
            return Err(Error::InvalidProjection);
        }
        Ok(raw)
    }
}
struct Writes {
    seen: BTreeSet<ObjectId>,
    bytes: u64,
}
struct Budget<'a, S> {
    store: &'a S,
    limits: Limits,
    deadline: Instant,
    writes: RefCell<Writes>,
}
impl<S: ObjectStore> ObjectStore for Budget<'_, S> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        if Instant::now() >= self.deadline {
            return Err(treeharbor_core::Error::TooLarge);
        }
        let object = self.store.get(id)?;
        if object.id()? != id {
            return Err(treeharbor_core::Error::InvalidObject(
                "import read identity",
            ));
        }
        Ok(object)
    }
    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        if Instant::now() >= self.deadline {
            return Err(treeharbor_core::Error::TooLarge);
        }
        let frame = object.encode()?;
        let id = ObjectId::hash_frame(&frame);
        let mut writes = self.writes.borrow_mut();
        if !writes.seen.contains(&id) {
            let bytes = writes
                .bytes
                .checked_add(frame.len() as u64)
                .ok_or(treeharbor_core::Error::TooLarge)?;
            if writes.seen.len() >= self.limits.objects || bytes > self.limits.source_bytes {
                return Err(treeharbor_core::Error::TooLarge);
            }
            writes.seen.insert(id);
            writes.bytes = bytes;
        }
        let actual = self.store.put(object)?;
        if actual != id {
            return Err(treeharbor_core::Error::InvalidObject(
                "import write identity",
            ));
        }
        Ok(id)
    }
}
struct TreeEntry {
    name: Name,
    mode: EntryMode,
    oid: Vec<u8>,
}
fn tree(raw: &[u8], algorithm: HashAlgorithm) -> Result<Vec<TreeEntry>> {
    let mut at = 0;
    let mut entries = Vec::new();
    let mut previous = None;
    while at < raw.len() {
        if entries.len() >= MAX_ENTRIES {
            return Err(Error::Limit);
        }
        let space = raw[at..]
            .iter()
            .position(|b| *b == b' ')
            .ok_or(Error::InvalidProjection)?
            + at;
        let mode = match &raw[at..space] {
            b"40000" => EntryMode::Directory,
            b"100644" => EntryMode::File,
            b"100755" => EntryMode::Executable,
            b"120000" => EntryMode::Symlink,
            _ => return Err(Error::Unsupported("Git entry mode (including submodules)")),
        };
        let zero = raw[space + 1..]
            .iter()
            .position(|b| *b == 0)
            .ok_or(Error::InvalidProjection)?
            + space
            + 1;
        let name = Name::new(raw[space + 1..zero].to_vec())?;
        let mut key = name.as_bytes().to_vec();
        if mode == EntryMode::Directory {
            key.push(b'/');
        }
        if previous.as_ref().is_some_and(|old| *old >= key) {
            return Err(Error::InvalidProjection);
        }
        previous = Some(key);
        let end = zero + 1 + algorithm.hex_len() / 2;
        let oid = raw
            .get(zero + 1..end)
            .ok_or(Error::InvalidProjection)?
            .to_vec();
        entries.push(TreeEntry { name, mode, oid });
        at = end;
    }
    Ok(entries)
}
struct ParsedCommit {
    tree: Vec<u8>,
    parents: Vec<Vec<u8>>,
    author: origin::Author,
    message: String,
}
fn commit(raw: &[u8], algorithm: HashAlgorithm) -> Result<ParsedCommit> {
    let end = raw
        .windows(2)
        .position(|w| w == b"\n\n")
        .ok_or(Error::InvalidProjection)?;
    let mut lines = raw[..end].split(|b| *b == b'\n').peekable();
    let tree = origin::parse_oid(
        lines
            .next()
            .and_then(|l| l.strip_prefix(b"tree "))
            .ok_or(Error::InvalidProjection)?,
        algorithm,
    )?;
    let mut parents = Vec::new();
    while let Some(line) = lines.peek().and_then(|l| l.strip_prefix(b"parent ")) {
        if parents.len() >= MAX_PARENTS {
            return Err(Error::Limit);
        }
        parents.push(origin::parse_oid(line, algorithm)?);
        lines.next();
    }
    let author = origin::parse_author(
        lines
            .next()
            .and_then(|l| l.strip_prefix(b"author "))
            .ok_or(Error::InvalidProjection)?,
    )?;
    let message = std::str::from_utf8(&raw[end + 2..])
        .map_err(|_| Error::Unsupported("non-UTF8 Git message"))?
        .into();
    Ok(ParsedCommit {
        tree,
        parents,
        author,
        message,
    })
}
fn git_oid(bytes: &[u8], algorithm: HashAlgorithm) -> Result<GitOid> {
    match algorithm {
        HashAlgorithm::Sha1 => Ok(GitOid::Sha1(
            bytes.try_into().map_err(|_| Error::InvalidProjection)?,
        )),
        HashAlgorithm::Sha256 => Ok(GitOid::Sha256(
            bytes.try_into().map_err(|_| Error::InvalidProjection)?,
        )),
    }
}
fn convert(
    store: &impl ObjectStore,
    source: &mut Source,
    target: Vec<u8>,
    limits: Limits,
    deadline: Instant,
) -> Result<Imported> {
    let budget = Budget {
        store,
        limits,
        deadline,
        writes: RefCell::new(Writes {
            seen: BTreeSet::new(),
            bytes: 0,
        }),
    };
    let empty = budget.put(&Object::Directory(Directory::new(vec![])?))?;
    let empty_snapshot = budget.put(&Object::Snapshot { root: empty })?;
    let mut pending = vec![(target.clone(), Kind::Commit, false)];
    let mut state = BTreeMap::<Vec<u8>, (Kind, Option<ObjectId>)>::new();
    let mut edges = 0usize;
    let mut visits = 0usize;
    let mut last_entries = 0;
    while let Some((oid, kind, finish)) = pending.pop() {
        check_deadline(deadline)?;
        if !finish && let Some((seen, done)) = state.get(&oid) {
            if *seen != kind || done.is_none() {
                return Err(Error::InvalidProjection);
            }
            continue;
        }
        let raw = if !finish && kind == Kind::Blob {
            Vec::new()
        } else {
            source.get(&oid, kind)?
        };
        if !finish {
            if state.len() >= limits.objects {
                return Err(Error::Limit);
            }
            state.insert(oid.clone(), (kind, None));
            let children = match kind {
                Kind::Blob => vec![],
                Kind::Tree => tree(&raw, source.algorithm)?
                    .into_iter()
                    .map(|e| {
                        (
                            e.oid,
                            if e.mode == EntryMode::Directory {
                                Kind::Tree
                            } else {
                                Kind::Blob
                            },
                        )
                    })
                    .collect(),
                Kind::Commit => {
                    let c = commit(&raw, source.algorithm)?;
                    c.parents
                        .into_iter()
                        .map(|p| (p, Kind::Commit))
                        .chain([(c.tree, Kind::Tree)])
                        .collect::<Vec<_>>()
                }
            };
            edges = edges.checked_add(children.len()).ok_or(Error::Limit)?;
            if edges > limits.edges {
                return Err(Error::Limit);
            }
            pending.push((oid, kind, true));
            pending.extend(children.into_iter().map(|(id, kind)| (id, kind, false)));
            continue;
        }
        let mapped = |oid: &[u8]| -> Result<ObjectId> {
            state
                .get(oid)
                .and_then(|(_, id)| *id)
                .ok_or(Error::InvalidProjection)
        };
        let native = match kind {
            Kind::Blob => {
                if raw.starts_with(b"version https://git-lfs.github.com/spec/v1\n")
                    || raw.starts_with(b"version https://git-lfs.github.com/spec/v1\r\n")
                {
                    return Err(Error::Unsupported(
                        "Git LFS pointer; hydration is not implemented",
                    ));
                }
                budget.put(&Object::Blob(raw))?
            }
            Kind::Tree => {
                let entries = tree(&raw, source.algorithm)?
                    .into_iter()
                    .map(|e| {
                        Ok(Entry {
                            name: e.name,
                            value: EntryRef {
                                mode: e.mode,
                                target: mapped(&e.oid)?,
                            },
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                budget.put(&Object::Directory(Directory::new(entries)?))?
            }
            Kind::Commit => {
                let parsed = commit(&raw, source.algorithm)?;
                let parents = parsed
                    .parents
                    .iter()
                    .map(|p| mapped(p))
                    .collect::<Result<Vec<_>>>()?;
                let root = mapped(&parsed.tree)?;
                last_entries = stream::check_paths(&budget, root, &mut visits, limits, deadline)?;
                let snapshot = budget.put(&Object::Snapshot { root })?;
                let base = if let Some(parent) = parents.first() {
                    let Object::NativeCommit(c) = budget.get(*parent)? else {
                        return Err(Error::InvalidProjection);
                    };
                    c.snapshot
                } else {
                    empty_snapshot
                };
                let delta = diff_snapshots(&budget, base, snapshot, TreeDiffLimits::default())?;
                let replay = prepare_change(&budget, base, &delta)?;
                if replay.result_snapshot() != snapshot {
                    return Err(Error::InvalidProjection);
                }
                let origin = GitOrigin {
                    oid: git_oid(&oid, source.algorithm)?,
                    raw: budget.put(&Object::Blob(raw.clone()))?,
                };
                let c = Commit {
                    git_origin: Some(Box::new(origin)),
                    parents,
                    snapshot,
                    changes: replay.operations(),
                    author_name: parsed.author.name,
                    author_email: parsed.author.email,
                    timestamp_seconds: parsed.author.seconds,
                    timezone_minutes: parsed.author.timezone,
                    message: parsed.message,
                };
                origin::verify(&c, &raw, &parsed.tree, &parsed.parents, source.algorithm)?;
                budget.put(&Object::NativeCommit(c))?
            }
        };
        state.get_mut(&oid).ok_or(Error::InvalidProjection)?.1 = Some(native);
    }
    let root = state
        .get(&target)
        .and_then(|(_, id)| *id)
        .ok_or(Error::InvalidProjection)?;
    let Object::NativeCommit(c) = budget.get(root)? else {
        return Err(Error::InvalidProjection);
    };
    let (objects, encoded_bytes) = stream::ordered(
        &budget,
        &BTreeMap::from([("refs/heads/import".into(), root)]),
        limits,
        deadline,
    )?;
    Ok(Imported {
        commit: root,
        snapshot: c.snapshot,
        objects,
        encoded_bytes,
        filesystem_entries: last_entries,
    })
}

/// Produces native immutable objects, never publishes a ref or authorizes code.
/// Partial writes on error belong to caller-owned staging storage, not a live repo.
/// Input is a complete local bundle; no source config, checkout or network access.
pub fn import(store: &impl ObjectStore, request: Import<'_>) -> Result<Imported> {
    check_limits(request.limits, request.git)?;
    let meta = fs::symlink_metadata(request.scratch)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Configuration);
    }
    let deadline = Instant::now() + request.limits.timeout;
    let scratch = tempfile::Builder::new()
        .prefix(".treeharbor-import-")
        .tempdir_in(request.scratch)?;
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700))?;
    let input = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(request.bundle)?;
    if !input.metadata()?.is_file() || input.metadata()?.len() > request.limits.stream_bytes {
        return Err(Error::Configuration);
    }
    let bundle = scratch.path().join("source.bundle");
    let mut copied = File::create(&bundle)?;
    let mut input = input.take(request.limits.stream_bytes + 1);
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    loop {
        check_deadline(deadline)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > request.limits.stream_bytes {
            return Err(Error::Limit);
        }
        copied.write_all(&buffer[..n])?;
    }
    drop(copied);
    let (algorithm, target) = match request.commit {
        GitOid::Sha1(oid) => (HashAlgorithm::Sha1, oid.to_vec()),
        GitOid::Sha256(oid) => (HashAlgorithm::Sha256, oid.to_vec()),
    };
    let repo = scratch.path().join("source.git");
    let template = scratch.path().join("empty-template");
    fs::create_dir(&repo)?;
    fs::create_dir(&template)?;
    let git = process::Git::new(request.git.canonicalize()?, scratch.path(), deadline);
    let mut template_arg = std::ffi::OsString::from("--template=");
    template_arg.push(&template);
    git.run(
        Some(&repo),
        "init importer",
        &[
            "init".into(),
            "--bare".into(),
            "--quiet".into(),
            format!("--object-format={}", algorithm.name()).into(),
            template_arg,
            repo.as_os_str().to_owned(),
        ],
        None,
        4096,
    )?;
    git.run(
        Some(&repo),
        "verify source bundle",
        &[
            "bundle".into(),
            "verify".into(),
            "--quiet".into(),
            bundle.as_os_str().to_owned(),
        ],
        None,
        1 << 20,
    )?;
    git.run(
        Some(&repo),
        "unbundle source",
        &[
            "bundle".into(),
            "unbundle".into(),
            bundle.as_os_str().to_owned(),
        ],
        None,
        1 << 20,
    )?;
    let target_hex = origin::hex(&target);
    git.run(
        Some(&repo),
        "fsck source",
        &[
            "fsck".into(),
            "--strict".into(),
            "--no-reflogs".into(),
            "--no-dangling".into(),
            target_hex.clone().into(),
        ],
        None,
        1 << 20,
    )?;
    let listing = git.capture(
        Some(&repo),
        "list source objects",
        &[
            "rev-list".into(),
            "--objects".into(),
            "--no-object-names".into(),
            target_hex.into(),
        ],
        None,
        request.limits.objects as u64 * 66,
    )?;
    let mut reader = BufReader::new(listing);
    let mut oids = Vec::new();
    let mut seen = BTreeSet::new();
    let input_path = scratch.path().join("objects.input");
    let mut commands = File::create(&input_path)?;
    loop {
        let line = line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        check_deadline(deadline)?;
        let oid = origin::parse_oid(&line, algorithm)?;
        if oids.len() >= request.limits.objects || !seen.insert(oid.clone()) {
            return Err(Error::Limit);
        }
        commands.write_all(&line)?;
        commands.write_all(b"\n")?;
        oids.push(oid);
    }
    drop(commands);
    let output = git.capture(
        Some(&repo),
        "read source objects",
        &["cat-file".into(), "--batch".into()],
        Some(&input_path),
        request.limits.source_bytes + request.limits.objects as u64 * 100,
    )?;
    let mut source = Source::open(output, &oids, algorithm, request.limits, deadline)?;
    convert(store, &mut source, target, request.limits, deadline)
}
