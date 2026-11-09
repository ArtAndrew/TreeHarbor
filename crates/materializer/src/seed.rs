//! Bounded, disposable source copies that never become an agent's writable tree.
use crate::{
    BaselineExpectation, Error, Limits, Result, Selection, Stage, filesystem::Manifest,
    publication, recovery,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{DirBuilderExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Instant,
};
use treeharbor_core::{EntryMode, Object, ObjectId, SourceProfile};
use treeharbor_metadata::Scope;

const MAX_FILES_BYTES: u64 = 16 << 20;
const MAX_MANIFEST_BYTES: u64 = 4 << 20;
const MAX_ENTRIES: usize = 8192;
const MAX_RECEIPT: u64 = 2048;
const CONTENTS: &str = "contents-v1";

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
        let ready = publication::directory(&self.root.join("ready"))?;
        let receipt = publication::read_file(&ready, "receipt.json")?;
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
        if publication::read_file(&ready, "manifest.json")?
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
        let manifest = recovery::read_manifest(
            &ready,
            BaselineExpectation {
                scope,
                snapshot,
                profile: &self.profile,
                manifest_digest: receipt.manifest,
            },
            limits,
        )?;
        let mut contents = BufReader::new(publication::read_file(&ready, CONTENTS)?);
        let leaves = read_leaves(&mut contents, &manifest, limits)?;
        Ok(Some(SeedInputs { manifest, leaves }))
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
            // This source is the node's fresh, verified staging tree, before
            // adding Git metadata or exposing any path to an agent.
            let contents = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(pending.join(CONTENTS))?;
            let mut contents = BufWriter::new(contents);
            write_leaves(&mut contents, &files, &stage.manifest)?;
            contents.flush()?;
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
    manifest: Manifest,
    leaves: Vec<Vec<u8>>,
}
impl SeedInputs {
    pub fn replay(self, mut stage: Stage) -> Result<Stage> {
        let started = Instant::now();
        let mut leaves = self.leaves.into_iter();
        for (path, entry) in self.manifest.entries {
            if entry.mode == EntryMode::Directory {
                stage.source_directory(path.as_bytes())?;
            } else {
                let data = leaves.next().ok_or(Error::InvalidBaseline)?;
                stage.write_source(path.as_bytes(), entry.mode, &data)?;
            }
            stage.record(path, entry)?;
        }
        stage.stats.local_bytes_copied = stage.stats.source_bytes_written;
        stage.stats.source_bytes_written = 0;
        stage.stats.build_batches += started.elapsed();
        stage.finish()?;
        Ok(stage)
    }
}

// The private cache uses one payload per manifest leaf, in manifest order.
// Paths, modes, and expected object IDs come from the verified manifest.
fn read_leaves(input: &mut impl Read, manifest: &Manifest, limits: Limits) -> Result<Vec<Vec<u8>>> {
    if manifest.preserved_local_state {
        return Err(Error::InvalidBaseline);
    }
    let mut remaining = limits.max_workspace_bytes;
    let mut leaves = Vec::new();
    for entry in manifest
        .entries
        .values()
        .filter(|entry| entry.mode != EntryMode::Directory)
    {
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u64::from(u32::from_be_bytes(length));
        remaining = remaining.checked_sub(length).ok_or(Error::Limit)?;
        if length > limits.max_file_bytes || length > treeharbor_core::MAX_PAYLOAD as u64 {
            return Err(Error::Limit);
        }
        let mut data = vec![0; length as usize];
        input.read_exact(&mut data)?;
        let blob = Object::Blob(data);
        if blob.id()? != entry.target {
            return Err(Error::InvalidBaseline);
        }
        let Object::Blob(data) = blob else {
            unreachable!()
        };
        leaves.push(data);
    }
    if input.read(&mut [0])? != 0 {
        return Err(Error::InvalidBaseline);
    }
    Ok(leaves)
}

fn write_leaves(output: &mut impl Write, files: &Path, manifest: &Manifest) -> Result<()> {
    for (path, entry) in &manifest.entries {
        let source = files.join(std::ffi::OsStr::from_bytes(path.as_bytes()));
        let data = match entry.mode {
            EntryMode::Directory => continue,
            EntryMode::Symlink => fs::read_link(source)?.into_os_string().into_vec(),
            EntryMode::File | EntryMode::Executable => fs::read(source)?,
        };
        let length = u32::try_from(data.len()).map_err(|_| Error::Limit)?;
        output.write_all(&length.to_be_bytes())?;
        output.write_all(&data)?;
    }
    Ok(())
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
    use std::os::unix::fs::symlink;
    use treeharbor_core::{EntryRef, RepoPath};

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

    #[test]
    fn contents_v1_round_trips_exact_leaves_and_rejects_bad_frames() {
        let files = tempfile::tempdir().unwrap();
        fs::create_dir(files.path().join("d")).unwrap();
        fs::write(files.path().join("d/f"), b"hi").unwrap();
        symlink("d/f", files.path().join("link")).unwrap();
        let manifest = Manifest {
            tenant: "fixture".into(),
            repository: "fixture".into(),
            snapshot: ObjectId::from_bytes([1; 32]),
            selection: Selection::from_profile(&SourceProfile::full()),
            preserved_local_state: false,
            entries: [
                ("d", EntryMode::Directory, &b""[..]),
                ("d/f", EntryMode::Executable, &b"hi"[..]),
                ("link", EntryMode::Symlink, &b"d/f"[..]),
            ]
            .into_iter()
            .map(|(path, mode, data)| {
                (
                    RepoPath::new(path.as_bytes().to_vec()).unwrap(),
                    EntryRef {
                        mode,
                        target: Object::Blob(data.to_vec()).id().unwrap(),
                    },
                )
            })
            .collect(),
        };
        let mut raw = Vec::new();
        write_leaves(&mut raw, files.path(), &manifest).unwrap();
        assert_eq!(raw, b"\0\0\0\x02hi\0\0\0\x03d/f");
        let limits = Limits::default();
        assert_eq!(
            read_leaves(&mut raw.as_slice(), &manifest, limits).unwrap(),
            [b"hi".to_vec(), b"d/f".to_vec()],
        );
        assert!(read_leaves(&mut &raw[..raw.len() - 1], &manifest, limits).is_err());
        assert!(
            read_leaves(
                &mut raw.as_slice(),
                &manifest,
                Limits {
                    max_workspace_bytes: 4,
                    ..limits
                }
            )
            .is_err()
        );
        raw.push(0);
        assert!(read_leaves(&mut raw.as_slice(), &manifest, limits).is_err());
    }
}
