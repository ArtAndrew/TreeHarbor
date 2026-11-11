//! Complete history of pinned Git commits; no ref authority or network fetch.
use crate::{
    Error, HashAlgorithm, Limits, Result, WorkspacePackIdentity, WorkspacePackScope, bundle,
    check_branch_refs, check_deadline, check_limits, origin, process, retained, stream, valid_ref,
};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
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

pub struct BundleInput<'a> {
    pub git: &'a Path,
    pub bundle: &'a Path,
    /// Existing private directory owned by the operator, outside any agent tree.
    pub scratch: &'a Path,
    pub limits: Limits,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ImportedTip {
    pub origin: GitOid,
    pub commit: ObjectId,
    pub snapshot: ObjectId,
    pub filesystem_entries: usize,
}

pub struct ImportedBranches {
    pub refs: BTreeMap<String, ImportedTip>,
    /// Union of the complete native closures, child before parent.
    pub objects: Vec<ObjectId>,
    pub encoded_bytes: u64,
}

const IMPORT_REF: &str = "refs/heads/import";

enum Selection<'a> {
    Commit(GitOid, Option<&'a WorkspacePackScope>),
    Branches(&'a BTreeMap<String, GitOid>),
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
    bytes: u64,
}
pub(crate) fn line(reader: &mut impl BufRead) -> Result<Vec<u8>> {
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
            bytes,
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

fn oid_parts(oid: &GitOid) -> (HashAlgorithm, &[u8]) {
    match oid {
        GitOid::Sha1(bytes) => (HashAlgorithm::Sha1, bytes),
        GitOid::Sha256(bytes) => (HashAlgorithm::Sha256, bytes),
    }
}

fn convert(
    store: &impl ObjectStore,
    source: &mut Source,
    targets: &BTreeSet<Vec<u8>>,
    refs: &BTreeMap<String, GitOid>,
    limits: Limits,
    deadline: Instant,
) -> Result<ImportedBranches> {
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
    let mut pending: Vec<_> = targets
        .iter()
        .map(|target| (target.clone(), Kind::Commit, false))
        .collect();
    let mut state = BTreeMap::<Vec<u8>, (Kind, Option<ObjectId>)>::new();
    let mut edges = 0usize;
    let mut visits = 0usize;
    let mut tip_entries = BTreeMap::new();
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
                let entries = stream::check_paths(&budget, root, &mut visits, limits, deadline)?;
                if targets.contains(&oid) {
                    tip_entries.insert(oid.clone(), entries);
                }
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
    let mut tips = BTreeMap::new();
    let mut roots = BTreeMap::new();
    for (name, origin) in refs {
        let (_, target) = oid_parts(origin);
        let root = state
            .get(target)
            .and_then(|(_, id)| *id)
            .ok_or(Error::InvalidProjection)?;
        let Object::NativeCommit(commit) = budget.get(root)? else {
            return Err(Error::InvalidProjection);
        };
        let filesystem_entries = *tip_entries.get(target).ok_or(Error::InvalidProjection)?;
        roots.insert(name.clone(), root);
        tips.insert(
            name.clone(),
            ImportedTip {
                origin: *origin,
                commit: root,
                snapshot: commit.snapshot,
                filesystem_entries,
            },
        );
    }
    let (objects, encoded_bytes) = stream::ordered(&budget, &roots, limits, deadline)?;
    Ok(ImportedBranches {
        refs: tips,
        objects,
        encoded_bytes,
    })
}

/// Produces native immutable objects, never publishes a ref or authorizes code.
/// Partial writes on error belong to caller-owned staging storage, not a live repo.
/// Input is a complete local bundle; no source config, checkout or network access.
pub fn import(store: &impl ObjectStore, request: Import<'_>) -> Result<Imported> {
    import_one(store, request, None)
}

/// Also retain the complete original Git pack when its verified object set
/// exactly matches the imported revision. Retention is optional and grants
/// no access to the accepted source. The caller owns the private output.
pub fn import_with_workspace_pack(
    store: &impl ObjectStore,
    request: Import<'_>,
    scope: &WorkspacePackScope,
) -> Result<Imported> {
    import_one(store, request, Some(scope))
}

/// Verify the complete advertised branch inventory against an independently
/// captured map, then import the union of all branch histories in one pass.
/// An optional advertised HEAD must point to one of those branches. Other refs
/// are rejected. No refs or authority are changed; use caller-owned staging.
pub fn import_branches(
    store: &impl ObjectStore,
    request: BundleInput<'_>,
    expected: &BTreeMap<String, GitOid>,
) -> Result<ImportedBranches> {
    import_inner(store, request, Selection::Branches(expected))
}

fn import_one(
    store: &impl ObjectStore,
    request: Import<'_>,
    scope: Option<&WorkspacePackScope>,
) -> Result<Imported> {
    let ImportedBranches {
        mut refs,
        objects,
        encoded_bytes,
    } = import_inner(
        store,
        BundleInput {
            git: request.git,
            bundle: request.bundle,
            scratch: request.scratch,
            limits: request.limits,
        },
        Selection::Commit(request.commit, scope),
    )?;
    let tip = refs.remove(IMPORT_REF).ok_or(Error::InvalidProjection)?;
    Ok(Imported {
        commit: tip.commit,
        snapshot: tip.snapshot,
        objects,
        encoded_bytes,
        filesystem_entries: tip.filesystem_entries,
    })
}

fn verify_bundle_refs(
    mut unbundled: File,
    algorithm: HashAlgorithm,
    expected: &BTreeMap<String, GitOid>,
) -> Result<()> {
    // The Git command already bounded this file before any native writes.
    let mut listing = String::new();
    unbundled.read_to_string(&mut listing)?;
    let mut head = None;
    let mut found = BTreeSet::new();
    for line in listing.split_terminator('\n') {
        let (oid, name) = line.split_once(' ').ok_or(Error::InvalidProjection)?;
        let oid = git_oid(&origin::parse_oid(oid.as_bytes(), algorithm)?, algorithm)?;
        if name == "HEAD" {
            if head.replace(oid).is_some() {
                return Err(Error::InvalidProjection);
            }
            continue;
        }
        if !valid_ref(name) {
            return Err(Error::Unsupported(
                "bundle advertises a non-branch or unsupported ref",
            ));
        }
        if expected.get(name) != Some(&oid) || !found.insert(name) {
            return Err(Error::RefInventoryMismatch);
        }
    }
    if found.len() != expected.len()
        || head.is_some_and(|oid| !expected.values().any(|v| *v == oid))
    {
        return Err(Error::RefInventoryMismatch);
    }
    Ok(())
}

fn import_inner(
    store: &impl ObjectStore,
    request: BundleInput<'_>,
    selection: Selection<'_>,
) -> Result<ImportedBranches> {
    check_limits(request.limits, request.git)?;
    let single;
    let refs = match selection {
        Selection::Commit(oid, _) => {
            single = BTreeMap::from([(IMPORT_REF.into(), oid)]);
            &single
        }
        Selection::Branches(expected) => expected,
    };
    check_branch_refs(refs)?;
    let first = refs.values().next().ok_or(Error::Configuration)?;
    let algorithm = oid_parts(first).0;
    if refs.values().any(|oid| oid_parts(oid).0 != algorithm) {
        return Err(Error::Configuration);
    }
    let targets: BTreeSet<_> = refs.values().map(|oid| oid_parts(oid).1.to_vec()).collect();
    let deadline = Instant::now() + request.limits.timeout;
    let scratch = bundle::scratch(request.scratch, ".treeharbor-import-")?;
    let bundle = scratch.path().join("source.bundle");
    bundle::copy(
        request.bundle,
        &bundle,
        request.limits.stream_bytes,
        deadline,
    )?;
    let git = process::Git::new(request.git.canonicalize()?, scratch.path(), deadline);
    let (repo, unbundled) = bundle::unpack(&git, scratch.path(), &bundle, algorithm, 1 << 20)?;
    if let Selection::Branches(expected) = selection {
        verify_bundle_refs(unbundled, algorithm, expected)?;
    }
    let target_hex = targets
        .iter()
        .map(|oid| std::ffi::OsString::from(origin::hex(oid)));
    let mut fsck = vec![
        "fsck".into(),
        "--strict".into(),
        "--no-reflogs".into(),
        "--no-dangling".into(),
    ];
    fsck.extend(target_hex.clone());
    git.run(Some(&repo), "fsck source", &fsck, None, 1 << 20)?;
    let mut rev_list = vec![
        "rev-list".into(),
        "--objects".into(),
        "--no-object-names".into(),
    ];
    rev_list.extend(target_hex);
    let listing = git.capture(
        Some(&repo),
        "list source objects",
        &rev_list,
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
    let imported = convert(store, &mut source, &targets, refs, request.limits, deadline)?;
    if let Selection::Commit(origin, Some(scope)) = selection
        && let Ok(pack) = retained::exact_pack(&git, &repo, algorithm, &seen, deadline)
    {
        let tip = &imported.refs[IMPORT_REF];
        let _ = retained::save(
            request.scratch,
            pack,
            WorkspacePackIdentity {
                scope,
                native: tip.commit,
                snapshot: tip.snapshot,
                origin,
            },
            retained::Bounds {
                objects: imported.objects.len().max(seen.len()),
                source_bytes: imported.encoded_bytes.max(source.bytes),
                edges: request.limits.edges,
                path_visits: request.limits.path_visits,
            },
            deadline,
        );
    }
    Ok(imported)
}
