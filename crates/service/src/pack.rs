//! A durable client spool of explicitly selected native source files. This is
//! neither a Git import nor an atomic snapshot of a tree with concurrent writers.
use crate::files;
use rustix::fs::{self as unixfs, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{self, File},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
};
use treeharbor_core::{
    Changes, Commit, Directory, Entry, EntryMode, EntryRef, MAX_PAYLOAD, Name, Object, ObjectId,
    ObjectStore, RepoPath, SourceProfile,
};
use treeharbor_storage::{LocalStore, TenantId};

pub const MAX_MANIFEST: usize = 128 * 1024 * 1024;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("source or spool filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("source object is invalid: {0}")]
    Object(#[from] treeharbor_core::Error),
    #[error("source/spool resource limit exceeded")]
    Limit,
    #[error("source changed during capture; stop writers and create a new pack")]
    Changed,
    #[error(
        "unsupported source entry or unsafe link; select regular files/directories and relative links"
    )]
    Unsupported,
    #[error("invalid or incomplete source pack")]
    Invalid,
    #[error("pack output must be a new path outside the source, in a private directory")]
    Output,
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Copy)]
pub struct Limits {
    pub objects: u64,
    pub bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            objects: 100_000,
            bytes: 4 * 1024 * 1024 * 1024,
        }
    }
}
impl Limits {
    fn check(self) -> Result<Self> {
        if !(1..=1_000_000).contains(&self.objects)
            || !(22..=64 * 1024 * 1024 * 1024).contains(&self.bytes)
        {
            return Err(Error::Limit);
        }
        Ok(self)
    }
}
pub struct Capture<'a> {
    pub directory: &'a Path,
    pub output: &'a Path,
    pub paths: Vec<RepoPath>,
    pub author_name: String,
    pub author_email: String,
    pub message: String,
    pub timestamp_seconds: i64,
    pub limits: Limits,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    version: u8,
    commit: String,
    snapshot: String,
    selection_hex: Vec<String>,
    objects: Vec<String>,
    encoded_bytes: u64,
    filesystem_entries: u64,
}
/// Validated complete graph. Source inputs are no longer consulted by upload.
pub struct Pack {
    pub(crate) directory: PathBuf,
    pub(crate) commit: ObjectId,
    pub(crate) snapshot: ObjectId,
    pub(crate) ids: Vec<ObjectId>,
    pub(crate) bytes: u64,
    pub(crate) fingerprint: ObjectId,
    pub(crate) entries: u64,
    store: LocalStore,
}
impl Pack {
    pub(crate) fn store(&self) -> &LocalStore {
        &self.store
    }
    pub fn commit(&self) -> ObjectId {
        self.commit
    }
    pub fn snapshot(&self) -> ObjectId {
        self.snapshot
    }
    pub fn object_count(&self) -> usize {
        self.ids.len()
    }
    pub fn encoded_bytes(&self) -> u64 {
        self.bytes
    }
    pub fn filesystem_entries(&self) -> u64 {
        self.entries
    }
    pub fn frame(&self, id: ObjectId) -> Result<Vec<u8>> {
        Ok(self.store.get(id)?.encode()?)
    }
    /// Validate every frame and exact reachable membership before transmitting
    /// source bytes. An edited list cannot smuggle unrelated local objects.
    pub fn open(directory: &Path) -> Result<Self> {
        let directory = files::private_directory(directory).map_err(|_| Error::Output)?;
        let raw = files::read_private(&directory.join("manifest.json"), MAX_MANIFEST)
            .map_err(|_| Error::Invalid)?;
        let manifest: Manifest = serde_json::from_slice(&raw).map_err(|_| Error::Invalid)?;
        if manifest.version != 1
            || manifest.objects.is_empty()
            || manifest.objects.len() > 1_000_000
            || manifest.filesystem_entries > 1_000_000
        {
            return Err(Error::Invalid);
        }
        let paths = manifest
            .selection_hex
            .iter()
            .map(|s| decode_path(s))
            .collect::<Result<Vec<_>>>()?;
        let profile = SourceProfile::sparse(paths)?;
        if profile
            .paths()
            .unwrap_or_default()
            .iter()
            .map(|p| hex(p.as_bytes()))
            .collect::<Vec<_>>()
            != manifest.selection_hex
        {
            return Err(Error::Invalid);
        }
        let commit: ObjectId = manifest.commit.parse()?;
        let snapshot: ObjectId = manifest.snapshot.parse()?;
        let store = LocalStore::open(&directory, &TenantId::new("native-pack")?)?;
        let mut descriptors = BTreeMap::new();
        let mut ids = Vec::new();
        let mut bytes = 0u64;
        let mut edge_count = 0usize;
        for value in manifest.objects {
            let id: ObjectId = value.parse()?;
            if descriptors.contains_key(&id) {
                return Err(Error::Invalid);
            }
            let object = store.get(id)?;
            let references = object.references();
            edge_count = edge_count
                .checked_add(references.len())
                .ok_or(Error::Limit)?;
            if edge_count > 2_000_000 {
                return Err(Error::Limit);
            }
            for (child, kind) in &references {
                if descriptors.get(child).map(|(k, _)| k) != Some(kind) {
                    return Err(Error::Invalid);
                }
            }
            bytes = bytes
                .checked_add(object.encode()?.len() as u64)
                .ok_or(Error::Limit)?;
            if bytes > 64 * 1024 * 1024 * 1024 {
                return Err(Error::Limit);
            }
            descriptors.insert(id, (object.kind(), references));
            ids.push(id);
        }
        if ids.last() != Some(&commit) || bytes != manifest.encoded_bytes {
            return Err(Error::Invalid);
        }
        let Object::NativeCommit(c) = store.get(commit)? else {
            return Err(Error::Invalid);
        };
        if c.snapshot != snapshot {
            return Err(Error::Invalid);
        }
        let mut reachable = BTreeSet::new();
        let mut pending = vec![commit];
        while let Some(id) = pending.pop() {
            if reachable.insert(id) {
                pending.extend(
                    descriptors
                        .get(&id)
                        .ok_or(Error::Invalid)?
                        .1
                        .iter()
                        .map(|(child, _)| *child),
                );
            }
        }
        if reachable.len() != ids.len() {
            return Err(Error::Invalid);
        }
        // Fingerprint is a client-spool identity, not a native source-object ID.
        let mut frame = b"treeharbor-source-pack\0v1\0".to_vec();
        frame.extend_from_slice(&raw);
        Ok(Self {
            directory,
            commit,
            snapshot,
            ids,
            bytes,
            fingerprint: ObjectId::hash_frame(&frame),
            entries: manifest.filesystem_entries,
            store,
        })
    }
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn decode_path(value: &str) -> Result<RepoPath> {
    if value.len() > 8192
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(Error::Invalid);
    }
    let bytes = (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| Error::Invalid))
        .collect::<Result<Vec<_>>>()?;
    Ok(RepoPath::new(bytes)?)
}
fn unix<T>(result: rustix::io::Result<T>) -> Result<T> {
    result.map_err(|e| Error::Io(e.into()))
}
fn stable(a: &Stat, b: &Stat) -> bool {
    a.st_dev == b.st_dev
        && a.st_ino == b.st_ino
        && a.st_mode == b.st_mode
        && a.st_nlink == b.st_nlink
        && a.st_size == b.st_size
        && a.st_mtime == b.st_mtime
        && a.st_mtime_nsec == b.st_mtime_nsec
        && a.st_ctime == b.st_ctime
        && a.st_ctime_nsec == b.st_ctime_nsec
}
fn unchanged(a: &Stat, b: &Stat) -> Result<()> {
    if !stable(a, b) {
        Err(Error::Changed)
    } else {
        Ok(())
    }
}
fn confirm_root(fd: &File, path: &Path, before: &Stat) -> Result<()> {
    unchanged(before, &unix(unixfs::fstat(fd))?)?;
    unchanged(
        before,
        &unix(unixfs::statat(unixfs::CWD, path, AtFlags::SYMLINK_NOFOLLOW))?,
    )
}
fn flags(directory: bool) -> OFlags {
    let mut f = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    if directory {
        f |= OFlags::DIRECTORY;
    }
    f
}
#[derive(Default)]
struct Selection {
    whole: bool,
    children: BTreeMap<Vec<u8>, Selection>,
}
impl Selection {
    fn add(&mut self, path: &RepoPath) {
        let mut node = self;
        for name in path.components() {
            node = node.children.entry(name.to_vec()).or_default();
        }
        node.whole = true;
    }
}
struct Builder {
    store: LocalStore,
    limits: Limits,
    ids: Vec<ObjectId>,
    seen: BTreeSet<ObjectId>,
    encoded: u64,
    source: u64,
    entries: u64,
    path_bytes: u64,
}
impl Builder {
    fn put(&mut self, object: Object) -> Result<ObjectId> {
        let frame = object.encode()?;
        let id = ObjectId::hash_frame(&frame);
        if self.seen.contains(&id) {
            return Ok(id);
        }
        if self.ids.len() as u64 >= self.limits.objects
            || frame.len() as u64 > self.limits.bytes - self.encoded
        {
            return Err(Error::Limit);
        }
        self.store.put(&object)?;
        self.encoded += frame.len() as u64;
        self.seen.insert(id);
        self.ids.push(id);
        Ok(id)
    }
    fn directory(&mut self, fd: &File, path: &[u8], selection: &Selection) -> Result<ObjectId> {
        let before = unix(unixfs::fstat(fd))?;
        let names = if selection.whole {
            let mut names = BTreeSet::new();
            for entry in unix(Dir::read_from(fd))? {
                let entry = unix(entry)?;
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                Name::new(name.to_vec())?;
                if names.len() >= treeharbor_core::MAX_ENTRIES {
                    return Err(Error::Limit);
                }
                if !names.insert(name.to_vec()) {
                    return Err(Error::Changed);
                }
            }
            names
        } else {
            selection.children.keys().cloned().collect()
        };
        let mut entries = Vec::new();
        for name in names {
            let mut child_path = path.to_vec();
            if !child_path.is_empty() {
                child_path.push(b'/');
            }
            child_path.extend_from_slice(&name);
            let checked = RepoPath::new(child_path)?;
            self.entries += 1;
            self.path_bytes += checked.as_bytes().len() as u64;
            if self.entries > 1_000_000 || self.path_bytes > 64 * 1024 * 1024 {
                return Err(Error::Limit);
            }
            let whole = Selection {
                whole: true,
                children: BTreeMap::new(),
            };
            let child_selection = if selection.whole {
                &whole
            } else {
                selection.children.get(&name).ok_or(Error::Invalid)?
            };
            let name_os = OsStr::from_bytes(&name);
            let stat = unix(unixfs::statat(fd, name_os, AtFlags::SYMLINK_NOFOLLOW))?;
            let kind = FileType::from_raw_mode(stat.st_mode);
            let value = if kind == FileType::Directory {
                let child = File::from(unix(unixfs::openat(
                    fd,
                    name_os,
                    flags(true),
                    Mode::empty(),
                ))?);
                unchanged(&stat, &unix(unixfs::fstat(&child))?)?;
                let target = self.directory(&child, checked.as_bytes(), child_selection)?;
                unchanged(
                    &stat,
                    &unix(unixfs::statat(fd, name_os, AtFlags::SYMLINK_NOFOLLOW))?,
                )?;
                EntryRef {
                    mode: EntryMode::Directory,
                    target,
                }
            } else if !child_selection.whole {
                return Err(Error::Unsupported);
            } else if kind == FileType::RegularFile {
                if stat.st_size < 0
                    || stat.st_size as u64 > MAX_PAYLOAD as u64
                    || stat.st_size as u64 > self.limits.bytes - self.source
                {
                    return Err(Error::Limit);
                }
                let child = File::from(unix(unixfs::openat(
                    fd,
                    name_os,
                    flags(false),
                    Mode::empty(),
                ))?);
                unchanged(&stat, &unix(unixfs::fstat(&child))?)?;
                let mut bytes = Vec::new();
                (&child)
                    .take(MAX_PAYLOAD as u64 + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_PAYLOAD {
                    return Err(Error::Limit);
                }
                unchanged(&stat, &unix(unixfs::fstat(&child))?)?;
                unchanged(
                    &stat,
                    &unix(unixfs::statat(fd, name_os, AtFlags::SYMLINK_NOFOLLOW))?,
                )?;
                if bytes.len() as u64 != stat.st_size as u64 {
                    return Err(Error::Changed);
                }
                self.source += bytes.len() as u64;
                // A native capture must not silently claim an unresolved LFS
                // pointer is the real large-file content.
                if bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\n")
                    || bytes.starts_with(b"version https://git-lfs.github.com/spec/v1\r\n")
                {
                    return Err(Error::Unsupported);
                }
                EntryRef {
                    mode: if stat.st_mode & 0o111 != 0 {
                        EntryMode::Executable
                    } else {
                        EntryMode::File
                    },
                    target: self.put(Object::Blob(bytes))?,
                }
            } else if kind == FileType::Symlink {
                let mut buffer = [0u8; 4097];
                let size = unix(unixfs::readlinkat_raw(fd, name_os, &mut buffer[..]))?;
                if size == buffer.len() {
                    return Err(Error::Limit);
                }
                let target = &buffer[..size];
                validate_link(checked.as_bytes(), target)?;
                unchanged(
                    &stat,
                    &unix(unixfs::statat(fd, name_os, AtFlags::SYMLINK_NOFOLLOW))?,
                )?;
                if size as u64 > self.limits.bytes - self.source {
                    return Err(Error::Limit);
                }
                self.source += size as u64;
                EntryRef {
                    mode: EntryMode::Symlink,
                    target: self.put(Object::Blob(target.to_vec()))?,
                }
            } else {
                return Err(Error::Unsupported);
            };
            entries.push(Entry {
                name: Name::new(name)?,
                value,
            });
        }
        unchanged(&before, &unix(unixfs::fstat(fd))?)?;
        self.put(Object::Directory(Directory::new(entries)?))
    }
}
fn validate_link(path: &[u8], target: &[u8]) -> Result<()> {
    if target.is_empty()
        || target.starts_with(b"/")
        || target.contains(&b'\\')
        || target.contains(&0)
    {
        return Err(Error::Unsupported);
    }
    let mut depth = path.split(|b| *b == b'/').count() - 1;
    for component in target.split(|b| *b == b'/') {
        if component == b".." {
            if depth == 0 {
                return Err(Error::Unsupported);
            }
            depth -= 1;
        } else if component != b"." && !component.is_empty() {
            Name::new(component.to_vec())?;
            depth += 1;
        }
    }
    Ok(())
}
pub fn capture(request: Capture<'_>) -> Result<Pack> {
    let limits = request.limits.check()?;
    let profile = SourceProfile::sparse(request.paths)?;
    let source_before = unix(unixfs::statat(
        unixfs::CWD,
        request.directory,
        AtFlags::SYMLINK_NOFOLLOW,
    ))?;
    if FileType::from_raw_mode(source_before.st_mode) != FileType::Directory {
        return Err(Error::Unsupported);
    }
    // Open the caller's path, not its canonicalized replacement. Otherwise a
    // concurrent rename to a symlink could redirect the entire capture.
    let fd = File::from(unix(unixfs::open(
        request.directory,
        flags(true),
        Mode::empty(),
    ))?);
    confirm_root(&fd, request.directory, &source_before)?;
    let source = request.directory.canonicalize()?;
    unchanged(
        &source_before,
        &unix(unixfs::statat(
            unixfs::CWD,
            &source,
            AtFlags::SYMLINK_NOFOLLOW,
        ))?,
    )?;
    let parent = files::private_directory(request.output.parent().ok_or(Error::Output)?)
        .map_err(|_| Error::Output)?;
    if parent.starts_with(&source) {
        return Err(Error::Output);
    }
    let name = request.output.file_name().ok_or(Error::Output)?;
    let output = parent.join(name);
    fs::DirBuilder::new().mode(0o700).create(&output)?;
    File::open(&parent)?.sync_all()?;
    let store = LocalStore::open(&output, &TenantId::new("native-pack")?)?;
    let mut builder = Builder {
        store,
        limits,
        ids: Vec::new(),
        seen: BTreeSet::new(),
        encoded: 0,
        source: 0,
        entries: 0,
        path_bytes: 0,
    };
    let mut selection = Selection::default();
    for path in profile.paths().unwrap_or_default() {
        selection.add(path);
    }
    let root = builder.directory(&fd, b"", &selection)?;
    let snapshot = builder.put(Object::Snapshot { root })?;
    let changes = builder.put(Object::ChangeOperations(Changes::new(vec![])?))?;
    let commit = builder.put(Object::NativeCommit(Commit {
        git_origin: None,
        parents: vec![],
        snapshot,
        changes,
        author_name: request.author_name,
        author_email: request.author_email,
        timestamp_seconds: request.timestamp_seconds,
        timezone_minutes: 0,
        message: request.message,
    }))?;
    let manifest = Manifest {
        version: 1,
        commit: commit.to_string(),
        snapshot: snapshot.to_string(),
        selection_hex: profile
            .paths()
            .unwrap_or_default()
            .iter()
            .map(|p| hex(p.as_bytes()))
            .collect(),
        objects: builder.ids.iter().map(ToString::to_string).collect(),
        encoded_bytes: builder.encoded,
        filesystem_entries: builder.entries,
    };
    let bytes = serde_json::to_vec(&manifest).map_err(|_| Error::Invalid)?;
    if bytes.len() > MAX_MANIFEST {
        return Err(Error::Limit);
    }
    confirm_root(&fd, request.directory, &source_before)?;
    files::write_private(&output.join("manifest.json"), &bytes).map_err(|_| Error::Output)?;
    Pack::open(&output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opened_root_must_still_match_its_name_before_manifest_publication() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source");
        fs::create_dir(&path).unwrap();
        let fd = File::open(&path).unwrap();
        let before = unixfs::fstat(&fd).unwrap();
        confirm_root(&fd, &path, &before).unwrap();
        fs::rename(&path, dir.path().join("moved")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(matches!(
            confirm_root(&fd, &path, &before),
            Err(Error::Changed)
        ));
        fs::remove_dir(&path).unwrap();
        std::os::unix::fs::symlink("moved", &path).unwrap();
        assert!(matches!(
            confirm_root(&fd, &path, &before),
            Err(Error::Changed)
        ));
    }
}
