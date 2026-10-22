use crate::{Error, GitOid, MAX_ENTRIES, MAX_PARENTS, Name, ObjectId, RepoPath, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum EntryMode {
    Directory = 0o040000,
    File = 0o100644,
    Executable = 0o100755,
    Symlink = 0o120000,
}
impl TryFrom<u32> for EntryMode {
    type Error = Error;
    fn try_from(value: u32) -> Result<Self> {
        match value {
            0o040000 => Ok(Self::Directory),
            0o100644 => Ok(Self::File),
            0o100755 => Ok(Self::Executable),
            0o120000 => Ok(Self::Symlink),
            _ => Err(Error::InvalidObject("unsupported entry mode")),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryRef {
    pub mode: EntryMode,
    pub target: ObjectId,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: Name,
    pub value: EntryRef,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Directory(Vec<Entry>);
impl Directory {
    pub fn new(mut entries: Vec<Entry>) -> Result<Self> {
        if entries.len() > MAX_ENTRIES {
            return Err(Error::TooLarge);
        }
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        if entries.windows(2).any(|w| w[0].name == w[1].name) {
            return Err(Error::InvalidObject("duplicate entry"));
        }
        Ok(Self(entries))
    }
    pub fn entries(&self) -> &[Entry] {
        &self.0
    }
    pub fn find(&self, name: &[u8]) -> Option<EntryRef> {
        self.0
            .binary_search_by(|e| e.name.as_bytes().cmp(name))
            .ok()
            .map(|i| self.0[i].value)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeOperation {
    pub path: RepoPath,
    pub expected: Option<EntryRef>,
    pub replacement: Option<EntryRef>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Changes(Vec<ChangeOperation>);
impl Changes {
    /// Original leaf-only constructor; existing callers retain the v1 contract.
    pub fn new(ops: Vec<ChangeOperation>) -> Result<Self> {
        Self::checked(ops, false)
    }
    /// Exact entry replacements, including whole directory subtrees. Parent and
    /// descendant operations remain forbidden; there is no implicit ordering.
    pub fn with_subtrees(ops: Vec<ChangeOperation>) -> Result<Self> {
        Self::checked(ops, true)
    }
    pub fn format_version(&self) -> u16 {
        if self.0.iter().any(|op| {
            [op.expected, op.replacement]
                .into_iter()
                .flatten()
                .any(|entry| entry.mode == EntryMode::Directory)
        }) {
            2
        } else {
            1
        }
    }
    fn checked(mut ops: Vec<ChangeOperation>, allow_directories: bool) -> Result<Self> {
        if ops.len() > MAX_ENTRIES {
            return Err(Error::TooLarge);
        }
        for op in &ops {
            if op.expected.is_none() && op.replacement.is_none() {
                return Err(Error::InvalidObject("empty operation"));
            }
            for value in [op.expected, op.replacement].into_iter().flatten() {
                if !allow_directories && value.mode == EntryMode::Directory {
                    return Err(Error::InvalidObject("operation must address a leaf"));
                }
            }
        }
        ops.sort_by(|a, b| a.path.cmp(&b.path));
        for pair in ops.windows(2) {
            if pair[0].path == pair[1].path {
                return Err(Error::OverlappingPaths);
            }
        }
        // An intervening name such as `a-1` can separate `a` from `a/b` in
        // byte ordering, so checking adjacent elements is insufficient.
        for op in &ops {
            for (i, b) in op.path.as_bytes().iter().enumerate() {
                if *b == b'/'
                    && ops
                        .binary_search_by(|p| p.path.as_bytes().cmp(&op.path.as_bytes()[..i]))
                        .is_ok()
                {
                    return Err(Error::OverlappingPaths);
                }
            }
        }
        Ok(Self(ops))
    }
    pub fn operations(&self) -> &[ChangeOperation] {
        &self.0
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitOrigin {
    /// Claimed Git identity, NOT an attestation of its association with this
    /// native commit. The interop boundary must verify content, tree and parents.
    pub oid: GitOid,
    /// Blob containing the complete original Git commit payload, byte for byte.
    pub raw: ObjectId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub git_origin: Option<Box<GitOrigin>>,
    pub parents: Vec<ObjectId>,
    pub snapshot: ObjectId,
    pub changes: ObjectId,
    pub author_name: String,
    pub author_email: String,
    pub timestamp_seconds: i64,
    pub timezone_minutes: i16,
    pub message: String,
}
impl Commit {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.parents.len() > MAX_PARENTS {
            return Err(Error::TooLarge);
        }
        if !(-1439..=1439).contains(&self.timezone_minutes) {
            return Err(Error::InvalidObject("timezone offset"));
        }
        if self.author_name.is_empty() || self.author_email.is_empty() {
            return Err(Error::InvalidObject("missing author"));
        }
        if self
            .author_name
            .bytes()
            .chain(self.author_email.bytes())
            .any(|b| matches!(b, 0 | b'\n' | b'\r' | b'<' | b'>'))
        {
            return Err(Error::InvalidObject("author delimiter"));
        }
        let mut unique = std::collections::BTreeSet::new();
        if self.parents.iter().any(|p| !unique.insert(p)) {
            return Err(Error::InvalidObject("duplicate parent"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Object {
    Blob(Vec<u8>),
    Directory(Directory),
    Snapshot { root: ObjectId },
    ChangeOperations(Changes),
    NativeCommit(Commit),
}
impl Object {
    pub fn kind(&self) -> u8 {
        match self {
            Self::Blob(_) => 1,
            Self::Directory(_) => 2,
            Self::Snapshot { .. } => 3,
            Self::ChangeOperations(_) => 4,
            Self::NativeCommit(_) => 5,
        }
    }
    pub fn id(&self) -> Result<ObjectId> {
        Ok(ObjectId::hash_frame(&self.encode()?))
    }
    pub fn references(&self) -> Vec<(ObjectId, u8)> {
        match self {
            Self::Blob(_) => vec![],
            Self::Directory(d) => d
                .entries()
                .iter()
                .map(|e| {
                    (
                        e.value.target,
                        if e.value.mode == EntryMode::Directory {
                            2
                        } else {
                            1
                        },
                    )
                })
                .collect(),
            Self::Snapshot { root } => vec![(*root, 2)],
            Self::ChangeOperations(ops) => ops
                .operations()
                .iter()
                .flat_map(|op| {
                    [op.expected, op.replacement]
                        .into_iter()
                        .flatten()
                        .map(|e| (e.target, if e.mode == EntryMode::Directory { 2 } else { 1 }))
                })
                .collect(),
            Self::NativeCommit(c) => c
                .parents
                .iter()
                .map(|id| (*id, 5))
                .chain([(c.snapshot, 3), (c.changes, 4)])
                .chain(c.git_origin.iter().map(|origin| (origin.raw, 1)))
                .collect(),
        }
    }
}
