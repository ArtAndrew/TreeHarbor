//! Bounded, disposable source copies that never become an agent's writable tree.
use crate::{
    BaselineExpectation, Error, Limits, MaterializedTree, Result, Selection, Stage, publication,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, DirBuilder, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Instant,
};
use treeharbor_core::{EntryMode, ObjectId, RepoPath, SourceProfile};
use treeharbor_metadata::Scope;

const MAX_FILES_BYTES: u64 = 16 << 20;
const MAX_MANIFEST_BYTES: u64 = 4 << 20;
const MAX_ENTRIES: usize = 8192;
const MAX_RECEIPT: u64 = 2048;

/// A local optimization controlled by the trusted node, which owns the private
/// parent and excludes other processes. All its slots share the same lock.
#[derive(Clone)]
pub struct SourceSeed {
    root: PathBuf,
    binding: [u8; 32],
    profile: SourceProfile,
    lock: Arc<RwLock<()>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    binding: [u8; 32],
    root: String,
    manifest: [u8; 32],
    source_bytes: u64,
}

impl SourceSeed {
    pub fn new(
        root: PathBuf,
        binding: [u8; 32],
        profile: SourceProfile,
        lock: Arc<RwLock<()>>,
    ) -> Self {
        Self {
            root,
            binding,
            profile,
            lock,
        }
    }

    pub(crate) fn matches(&self, selection: &Selection) -> bool {
        Selection::from_profile(&self.profile) == *selection
    }

    pub(crate) fn load(
        &self,
        scope: &Scope,
        root: ObjectId,
        snapshot: ObjectId,
        limits: Limits,
    ) -> Result<Option<SeedInputs>> {
        let _lock = self.lock.read().map_err(|_| Error::Configuration)?;
        publication::directory(&self.root)?;
        let ready = self.root.join("ready");
        let receipt = publication::read_file(&publication::directory(&ready)?, "receipt.json")?;
        let mut bytes = Vec::new();
        receipt.take(MAX_RECEIPT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_RECEIPT {
            return Err(Error::Limit);
        }
        let receipt: Receipt = serde_json::from_slice(&bytes)?;
        if receipt.binding != self.binding
            || receipt.root != root.to_string()
            || receipt.source_bytes > limits.max_source_bytes
        {
            return Ok(None);
        }
        let ready_dir = publication::directory(&ready)?;
        if publication::read_file(&ready_dir, "manifest.json")?
            .metadata()?
            .len()
            > MAX_MANIFEST_BYTES
        {
            return Err(Error::Limit);
        }
        let limits = Limits {
            max_entries: limits.max_entries.min(MAX_ENTRIES),
            max_workspace_bytes: limits.max_workspace_bytes.min(MAX_FILES_BYTES),
            ..limits
        };
        let tree = MaterializedTree::load_baseline(
            &ready,
            BaselineExpectation {
                scope,
                snapshot,
                profile: &self.profile,
                manifest_digest: receipt.manifest,
            },
            limits,
        )?;
        let mut leaves = BTreeMap::new();
        tree.read_pristine_files(limits, |path, data| {
            leaves.insert(path.clone(), data);
        })?;
        Ok(Some(SeedInputs { tree, leaves }))
    }

    pub(crate) fn remember(&self, stage: &Stage, root: ObjectId) -> Result<()> {
        if stage.manifest.entries.len() > MAX_ENTRIES
            || stage.stats.workspace_bytes > MAX_FILES_BYTES
        {
            return Ok(());
        }
        let files = stage.files();
        let manifest = files
            .parent()
            .ok_or(Error::Configuration)?
            .join("manifest.json");
        let manifest = read_bounded(&manifest, MAX_MANIFEST_BYTES)?;
        let _lock = self.lock.write().map_err(|_| Error::Configuration)?;
        match DirBuilder::new().mode(0o700).create(&self.root) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => (),
            Err(e) => return Err(e.into()),
        }
        publication::directory(&self.root)?;
        let ready = self.root.join("ready");
        let pending = self.root.join("pending");
        remove(&pending)?;
        let result = (|| {
            DirBuilder::new().mode(0o700).create(&pending)?;
            let new_files = pending.join("files");
            DirBuilder::new().mode(0o700).create(&new_files)?;
            // This source is the node's fresh, verified staging tree, before
            // adding Git metadata or exposing any path to an agent.
            for (path, entry) in &stage.manifest.entries {
                let path = std::ffi::OsStr::from_bytes(path.as_bytes());
                let source = files.join(path);
                let target = new_files.join(path);
                match entry.mode {
                    EntryMode::Directory => DirBuilder::new().mode(0o700).create(target)?,
                    EntryMode::Symlink => symlink(fs::read_link(source)?, target)?,
                    EntryMode::File | EntryMode::Executable => {
                        fs::copy(source, &target)?;
                        let mode = if entry.mode == EntryMode::Executable {
                            0o500
                        } else {
                            0o400
                        };
                        fs::set_permissions(target, fs::Permissions::from_mode(mode))?;
                    }
                }
            }
            write_new(&pending.join("manifest.json"), &manifest)?;
            let receipt = Receipt {
                binding: self.binding,
                root: root.to_string(),
                manifest: Sha256::digest(&manifest).into(),
                source_bytes: stage.stats.source_encoded_bytes,
            };
            write_new(
                &pending.join("receipt.json"),
                &serde_json::to_vec(&receipt)?,
            )?;
            remove(&ready)?;
            fs::rename(&pending, &ready)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = remove(&pending);
        }
        result
    }
}

pub(crate) struct SeedInputs {
    tree: MaterializedTree,
    leaves: BTreeMap<RepoPath, Vec<u8>>,
}
impl SeedInputs {
    pub fn replay(self, mut stage: Stage) -> Result<Stage> {
        let started = Instant::now();
        for (path, entry) in &self.tree.manifest.entries {
            stage.record(path.clone(), *entry)?;
            if entry.mode == EntryMode::Directory {
                stage.source_directory(path.as_bytes())?;
            } else {
                let data = self.leaves.get(path).ok_or(Error::InvalidBaseline)?;
                stage.write_source(path.as_bytes(), entry.mode, data)?;
            }
        }
        stage.stats.local_bytes_copied = stage.stats.source_bytes_written;
        stage.stats.source_bytes_written = 0;
        stage.stats.build_batches += started.elapsed();
        stage.finish()?;
        Ok(stage)
    }
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let parent = publication::directory(path.parent().ok_or(Error::Configuration)?)?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or(Error::Configuration)?;
    let mut bytes = Vec::new();
    publication::read_file(&parent, name)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Limit);
    }
    Ok(bytes)
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(bytes)
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_v1_matches_independent_digest() {
        let receipt = Receipt {
            binding: [1; 32],
            root: ObjectId::from_bytes([2; 32]).to_string(),
            manifest: [3; 32],
            source_bytes: 4096,
        };
        let bytes = serde_json::to_vec(&receipt).unwrap();
        assert_eq!(bytes.len(), 255);
        assert_eq!(
            ObjectId::hash_frame(&bytes).hex(),
            "5903844db9e583b990aaa0716db8a80176d31338eeeb483bcae520a135ec49cf"
        );
    }
}
