use crate::{Error, Limits, MaterializedTree, Result, Selection, Statistics};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::Arc,
};
use tempfile::TempDir;
use tokio::sync::OwnedSemaphorePermit;
use treeharbor_core::{EntryMode, EntryRef, Name, ObjectId, RepoPath};
use treeharbor_metadata::reads::ReadObject;

#[derive(Debug)]
pub(crate) struct Manifest {
    pub tenant: String,
    pub repository: String,
    pub snapshot: ObjectId,
    pub selection: Selection,
    pub entries: BTreeMap<RepoPath, EntryRef>,
}

pub(crate) struct Stage {
    temp: TempDir,
    _permit: OwnedSemaphorePermit,
    pub manifest: Manifest,
    previous: Option<Arc<Manifest>>,
    limits: Limits,
    stats: Statistics,
    planned_path_bytes: u64,
    actual_path_bytes: u64,
    actual: BTreeSet<Vec<u8>>,
    directories: BTreeSet<Vec<u8>>,
    links: BTreeMap<Vec<u8>, Vec<u8>>,
}
impl Stage {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        parent: PathBuf,
        tenant: String,
        repository: String,
        snapshot: ObjectId,
        selection: Selection,
        limits: Limits,
        permit: OwnedSemaphorePermit,
        previous: Option<MaterializedTree>,
    ) -> Result<Self> {
        private_directory(&parent)?;
        let temp = tempfile::Builder::new()
            .prefix("prepare-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&parent)?;
        private_directory(&temp.path().join("files"))?;
        let mut stage = Self {
            temp,
            _permit: permit,
            manifest: Manifest {
                tenant,
                repository,
                snapshot,
                selection,
                entries: BTreeMap::new(),
            },
            previous: previous.as_ref().map(|p| p.manifest.clone()),
            limits,
            stats: Statistics::default(),
            planned_path_bytes: 0,
            actual_path_bytes: 0,
            actual: BTreeSet::new(),
            directories: BTreeSet::from([vec![]]),
            links: BTreeMap::new(),
        };
        if let Some(previous) = previous {
            stage.copy_stopped(&previous.path())?;
        }
        Ok(stage)
    }
    fn files(&self) -> PathBuf {
        self.temp.path().join("files")
    }
    fn path(&self, relative: &[u8]) -> PathBuf {
        self.files().join(std::ffi::OsStr::from_bytes(relative))
    }
    pub fn read_budget(&self) -> Result<u64> {
        let remaining = self
            .limits
            .max_source_bytes
            .saturating_sub(self.stats.source_encoded_bytes);
        if remaining == 0 {
            return Err(Error::Limit);
        }
        Ok(remaining.min(treeharbor_metadata::reads::MAX_READ_BYTES))
    }
    pub fn account_source(&mut self, object: &ReadObject) -> Result<()> {
        self.stats.source_objects = self
            .stats
            .source_objects
            .checked_add(1)
            .ok_or(Error::Limit)?;
        self.stats.source_encoded_bytes = self
            .stats
            .source_encoded_bytes
            .checked_add(object.encoded_length())
            .ok_or(Error::Limit)?;
        if self.stats.source_encoded_bytes > self.limits.max_source_bytes {
            return Err(Error::Limit);
        }
        Ok(())
    }
    pub fn record(&mut self, path: RepoPath, value: EntryRef) -> Result<()> {
        if let Some(old) = self.manifest.entries.get(&path) {
            if *old != value {
                return Err(Error::WrongSnapshot);
            }
            return Ok(());
        }
        self.planned_path_bytes = self
            .planned_path_bytes
            .checked_add(path.as_bytes().len() as u64)
            .ok_or(Error::Limit)?;
        if self.manifest.entries.len() >= self.limits.max_entries
            || self.planned_path_bytes > self.limits.max_path_bytes
        {
            return Err(Error::Limit);
        }
        self.manifest.entries.insert(path, value);
        Ok(())
    }
    fn account_path(&mut self, path: &[u8], bytes: u64) -> Result<()> {
        if !self.actual.insert(path.to_vec()) {
            return Err(Error::LocalConflict(path.to_vec()));
        }
        self.actual_path_bytes = self
            .actual_path_bytes
            .checked_add(path.len() as u64)
            .ok_or(Error::Limit)?;
        self.stats.filesystem_entries += 1;
        self.stats.workspace_bytes = self
            .stats
            .workspace_bytes
            .checked_add(bytes)
            .ok_or(Error::Limit)?;
        if bytes > self.limits.max_file_bytes
            || self.stats.workspace_bytes > self.limits.max_workspace_bytes
            || self.stats.filesystem_entries > self.limits.max_entries
            || self.actual_path_bytes > self.limits.max_path_bytes
        {
            return Err(Error::Limit);
        }
        Ok(())
    }
    fn ensure_directory(&mut self, relative: &[u8]) -> Result<()> {
        let mut prefix = Vec::new();
        for component in relative.split(|c| *c == b'/').filter(|s| !s.is_empty()) {
            if !prefix.is_empty() {
                prefix.push(b'/');
            }
            prefix.extend_from_slice(component);
            if self.directories.contains(&prefix) {
                continue;
            }
            let path = self.path(&prefix);
            match fs::DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => (),
                // Existing entries here were not created as this exact directory
                // by this builder. Reject symlinks and filesystem case/Unicode aliases.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    return Err(Error::LocalConflict(prefix));
                }
                Err(e) => return Err(e.into()),
            }
            self.account_path(&prefix, 0)?;
            self.directories.insert(prefix.clone());
        }
        Ok(())
    }
    fn parents(&mut self, relative: &[u8]) -> Result<()> {
        if let Some(index) = relative.iter().rposition(|c| *c == b'/') {
            self.ensure_directory(&relative[..index])?;
        }
        Ok(())
    }
    pub fn source_directory(&mut self, relative: &[u8]) -> Result<()> {
        if relative.is_empty() {
            return Ok(());
        }
        let path = RepoPath::new(relative.to_vec())?;
        if self
            .previous
            .as_ref()
            .is_some_and(|p| p.entries.contains_key(&path))
        {
            return Ok(());
        }
        self.ensure_directory(relative)
    }
    pub fn preserve_leaf(&self, path: &RepoPath) -> Result<bool> {
        if self
            .previous
            .as_ref()
            .is_some_and(|p| p.entries.contains_key(path))
        {
            return Ok(true);
        }
        // The staging tree is owned only by this builder. Its exact-path ledger
        // avoids a stat through an untrusted copied symlink ancestor. Filesystem
        // aliases not in this ledger fail exclusive creation instead of overwriting.
        if self.actual.contains(path.as_bytes()) {
            if self.previous.is_some() {
                Ok(true)
            } else {
                Err(Error::LocalConflict(path.as_bytes().to_vec()))
            }
        } else {
            Ok(false)
        }
    }
    pub fn write_source(&mut self, relative: &[u8], mode: EntryMode, bytes: &[u8]) -> Result<()> {
        self.parents(relative)?;
        self.account_path(relative, bytes.len() as u64)?;
        match mode {
            EntryMode::File | EntryMode::Executable => {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(self.path(relative))?;
                file.write_all(bytes)?;
                file.set_permissions(fs::Permissions::from_mode(
                    if mode == EntryMode::Executable {
                        0o700
                    } else {
                        0o600
                    },
                ))?;
                file.sync_all()?;
            }
            EntryMode::Symlink => self.write_link(relative, bytes)?,
            EntryMode::Directory => return Err(Error::UnsafePath("directory written as a leaf")),
        }
        self.stats.source_bytes_written += bytes.len() as u64;
        Ok(())
    }
    fn write_link(&mut self, relative: &[u8], target: &[u8]) -> Result<()> {
        check_link_target(target)?;
        symlink(std::ffi::OsStr::from_bytes(target), self.path(relative))?;
        self.links.insert(relative.to_vec(), target.to_vec());
        Ok(())
    }
    fn copy_stopped(&mut self, source: &Path) -> Result<()> {
        let meta = fs::symlink_metadata(source)?;
        if !meta.is_dir() || meta.file_type().is_symlink() {
            return Err(Error::UnsafePath("workspace root"));
        }
        let mut pending = vec![vec![]];
        while let Some(relative) = pending.pop() {
            let mut children = Vec::new();
            let remaining = self
                .limits
                .max_entries
                .saturating_sub(self.stats.filesystem_entries);
            for entry in fs::read_dir(source.join(std::ffi::OsStr::from_bytes(&relative)))? {
                if children.len() >= remaining {
                    return Err(Error::Limit);
                }
                children.push(entry?);
            }
            children.sort_by_key(|e| e.file_name());
            for child in children {
                let name = Name::new(child.file_name().into_vec())?;
                let mut path = relative.clone();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(name.as_bytes());
                RepoPath::new(path.clone())?;
                let meta = fs::symlink_metadata(child.path())?;
                if meta.file_type().is_symlink() {
                    let target = fs::read_link(child.path())?.into_os_string().into_vec();
                    self.parents(&path)?;
                    self.account_path(&path, target.len() as u64)?;
                    self.write_link(&path, &target)?;
                } else if meta.is_dir() {
                    self.ensure_directory(&path)?;
                    pending.push(path);
                } else if meta.is_file() {
                    self.parents(&path)?;
                    self.account_path(&path, meta.len())?;
                    let input = File::open(child.path())?;
                    let mut output = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .mode(0o600)
                        .open(self.path(&path))?;
                    let copied = std::io::copy(
                        &mut input.take(meta.len().checked_add(1).ok_or(Error::Limit)?),
                        &mut output,
                    )?;
                    if copied != meta.len() {
                        return Err(Error::UnsafePath("writer changed stopped file length"));
                    }
                    output.set_permissions(fs::Permissions::from_mode(
                        if meta.permissions().mode() & 0o111 != 0 {
                            0o700
                        } else {
                            0o600
                        },
                    ))?;
                    output.sync_all()?;
                    self.stats.local_bytes_copied += copied;
                } else {
                    return Err(Error::UnsafePath("socket, device or other special file"));
                }
            }
        }
        Ok(())
    }
    pub fn finish(&mut self) -> Result<()> {
        if let Some(paths) = &self.manifest.selection.paths {
            for path in paths {
                if !self.manifest.entries.contains_key(path) {
                    return Err(Error::MissingPath(path.as_bytes().to_vec()));
                }
            }
        }
        validate_links(&self.links, |path| {
            let path = self.path(path);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    Ok(Some(fs::read_link(path)?.into_os_string().into_vec()))
                }
                Ok(_) => Ok(None),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e.into()),
            }
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(self.temp.path().join("manifest.json"))?;
        let paths = self
            .manifest
            .selection
            .paths
            .as_ref()
            .map(|v| v.iter().map(|p| hex(p.as_bytes())).collect::<Vec<_>>());
        let header = serde_json::json!({"version":1,"materializer":"copy-unix-v1","tenant":self.manifest.tenant,"repository":self.manifest.repository,"snapshot":self.manifest.snapshot.hex(),"selection":paths,"preserved_local_state":self.previous.is_some()});
        // Stream entries so the manifest encoding does not allocate a second full tree.
        file.write_all(b"{\"header\":")?;
        serde_json::to_writer(&mut file, &header)?;
        file.write_all(b",\"entries\":[")?;
        for (index, (path, entry)) in self.manifest.entries.iter().enumerate() {
            if index != 0 {
                file.write_all(b",")?;
            }
            serde_json::to_writer(
                &mut file,
                &serde_json::json!([hex(path.as_bytes()), entry.mode as u32, entry.target.hex()]),
            )?;
        }
        file.write_all(b"]}\n")?;
        file.sync_all()?;
        for directory in self.directories.iter().rev() {
            File::open(self.path(directory))?.sync_all()?;
        }
        File::open(self.temp.path())?.sync_all()?;
        File::open(self.temp.path().parent().ok_or(Error::Configuration)?)?.sync_all()?;
        Ok(())
    }
    pub fn publish(self) -> Result<MaterializedTree> {
        // No node registry or runtime can discover this directory through a
        // returned receipt before completion. Parent fsync precedes that receipt.
        let root = self.temp.keep();
        Ok(MaterializedTree {
            root,
            manifest: Arc::new(self.manifest),
            stats: self.stats,
        })
    }
}

pub(crate) fn private_directory(path: &Path) -> Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let meta = fs::symlink_metadata(path)?;
            if !meta.is_dir()
                || meta.file_type().is_symlink()
                || meta.permissions().mode() & 0o077 != 0
            {
                return Err(Error::UnsafePath(
                    "service directory must be private and not a symlink",
                ));
            }
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn check_link_target(target: &[u8]) -> Result<()> {
    if target.is_empty()
        || target.len() > 4096
        || target.starts_with(b"/")
        || target.iter().any(|b| matches!(b, 0 | b'\\'))
    {
        return Err(Error::UnsafePath("symlink target"));
    }
    Ok(())
}
fn validate_links(
    links: &BTreeMap<Vec<u8>, Vec<u8>>,
    mut lookup: impl FnMut(&[u8]) -> Result<Option<Vec<u8>>>,
) -> Result<()> {
    for (link, target) in links {
        let mut queue: VecDeque<Vec<u8>> = link.split(|b| *b == b'/').map(<[u8]>::to_vec).collect();
        queue.pop_back();
        queue.extend(target.split(|b| *b == b'/').map(<[u8]>::to_vec));
        let mut resolved: Vec<Vec<u8>> = Vec::new();
        let mut followed = 0;
        while let Some(component) = queue.pop_front() {
            match component.as_slice() {
                b"" | b"." => continue,
                b".." => {
                    if resolved.pop().is_none() {
                        return Err(Error::UnsafePath("symlink escapes workspace"));
                    }
                }
                _ => {
                    resolved.push(component);
                    let path = resolved.join(&b'/');
                    // Use the target filesystem's lookup semantics. A byte-only
                    // map would miss case/Unicode aliases on macOS and could
                    // incorrectly approve a chain containing parent traversals.
                    if let Some(target) = lookup(&path)? {
                        check_link_target(&target)?;
                        followed += 1;
                        if followed > 40 {
                            return Err(Error::UnsafePath("symlink cycle or excessive traversal"));
                        }
                        resolved.pop();
                        for part in target.split(|b| *b == b'/').rev() {
                            queue.push_front(part.to_vec());
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn check(links: BTreeMap<Vec<u8>, Vec<u8>>) -> Result<()> {
        validate_links(&links, |path| Ok(links.get(path).cloned()))
    }
    #[test]
    fn symlink_resolution_checks_expansion_before_parent_components() {
        let links = BTreeMap::from([
            (b"dir/link".to_vec(), b"..".to_vec()),
            (b"escape".to_vec(), b"dir/link/../../secret".to_vec()),
        ]);
        assert!(check(links).is_err());
        assert!(
            check(BTreeMap::from([
                (b"a".to_vec(), b"b".to_vec()),
                (b"b".to_vec(), b"a".to_vec())
            ]))
            .is_err()
        );
        assert!(
            check(BTreeMap::from([(
                b"dir/up".to_vec(),
                b"../missing".to_vec()
            )]))
            .is_ok()
        );
    }

    #[tokio::test]
    async fn manifest_v1_has_stable_explicit_byte_names_and_baseline_identity() {
        let parent = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let permit = Arc::new(tokio::sync::Semaphore::new(1))
            .acquire_owned()
            .await
            .unwrap();
        let mut stage = Stage::new(
            parent.path().to_owned(),
            "tenant".into(),
            "00000000-0000-0000-0000-000000000001".into(),
            ObjectId::from_bytes([3; 32]),
            Selection::full(),
            Limits::default(),
            permit,
            None,
        )
        .unwrap();
        stage
            .record(
                RepoPath::new(b"a".to_vec()).unwrap(),
                EntryRef {
                    mode: EntryMode::File,
                    target: ObjectId::from_bytes([1; 32]),
                },
            )
            .unwrap();
        stage.write_source(b"a", EntryMode::File, b"x").unwrap();
        stage.finish().unwrap();
        let expected = concat!(
            "{\"header\":{\"materializer\":\"copy-unix-v1\",\"preserved_local_state\":false,\"repository\":\"00000000-0000-0000-0000-000000000001\",\"selection\":null,\"snapshot\":\"",
            "0303030303030303030303030303030303030303030303030303030303030303",
            "\",\"tenant\":\"tenant\",\"version\":1},\"entries\":[[\"61\",33188,\"",
            "0101010101010101010101010101010101010101010101010101010101010101",
            "\"]]}\n"
        );
        assert_eq!(
            fs::read_to_string(stage.temp.path().join("manifest.json")).unwrap(),
            expected
        );
    }
}
