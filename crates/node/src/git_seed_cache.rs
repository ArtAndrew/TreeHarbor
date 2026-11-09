//! Disposable, node-private copies of already verified Git metadata.
use crate::{Error, Result, private, storage::Operation};
use rustix::fs::OFlags;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, DirBuilder, OpenOptions, Permissions},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
};
use tempfile::TempDir;
use treeharbor_core::RevisionRef;
use treeharbor_git_bridge::Limits;
use treeharbor_metadata::preparations::Preparation;

const DIRECTORY: &str = "git-seed-cache-v1";
const MAX_BYTES: u64 = 32 << 20;
const MAX_ENTRIES: usize = 256;
const MAX_DEPTH: usize = 8;
pub(crate) type Key = [u8; 32];

pub(crate) fn key(plan: &Preparation, git: &Path, limits: Limits) -> io::Result<Key> {
    let path = git.canonicalize()?;
    let executable = path.metadata()?;
    let revision = plan.revision().to_string();
    let origin =
        RevisionRef::Git(plan.git_revision().ok_or(io::ErrorKind::InvalidInput)?).to_string();
    let mut hash = Sha256::new();
    for field in [
        path.as_os_str().as_encoded_bytes(),
        plan.scope().tenant().as_bytes(),
        plan.scope().repository().as_bytes(),
        revision.as_bytes(),
        plan.snapshot().as_bytes(),
        origin.as_bytes(),
    ] {
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field);
    }
    for field in [
        executable.dev(),
        executable.ino(),
        executable.len(),
        u64::from(executable.mode()),
        executable.mtime() as u64,
        executable.mtime_nsec() as u64,
        executable.ctime() as u64,
        executable.ctime_nsec() as u64,
        plan.epoch() as u64,
        limits.objects as u64,
        limits.edges as u64,
        limits.source_bytes,
        limits.stream_bytes,
        limits.path_visits as u64,
    ] {
        hash.update(field.to_be_bytes());
    }
    hash.update(limits.timeout.as_nanos().to_be_bytes());
    Ok(hash.finalize().into())
}

pub(crate) fn load(operation: &Operation, parent: &Path, key: Key) -> Result<Option<TempDir>> {
    operation.verify()?;
    let _lock = operation
        .storage
        .git_seed_cache
        .lock()
        .map_err(|_| Error::Busy)?;
    let root = private::directory(&operation.storage.root.join(DIRECTORY))?;
    restore(&root, parent, key)
}

pub(crate) fn save(operation: &Operation, source: &Path, key: Key) -> Result<()> {
    operation.verify()?;
    let _lock = operation
        .storage
        .git_seed_cache
        .lock()
        .map_err(|_| Error::Busy)?;
    let root = private::make_directory(&operation.storage.root.join(DIRECTORY))?;
    remember(&root, source, key)
}

fn restore(root: &Path, parent: &Path, key: Key) -> Result<Option<TempDir>> {
    let ready = root.join("ready");
    let receipt = private::read(&ready.join("receipt"), 64)?;
    if receipt.get(..32) != Some(key.as_slice()) {
        return Ok(None);
    }
    let staging = tempfile::Builder::new()
        .prefix(".treeharbor-git-cache-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(private::directory(parent)?)?;
    let digest = copy_metadata(&ready.join("metadata"), &staging.path().join("metadata"))?;
    if receipt.get(32..) != Some(digest.as_slice()) {
        return Ok(None);
    }
    Ok(Some(staging))
}

fn remember(root: &Path, source: &Path, key: Key) -> Result<()> {
    let ready = root.join("ready");
    let pending = root.join("pending");
    remove(&pending)?;
    let result = (|| {
        DirBuilder::new().mode(0o700).create(&pending)?;
        let digest = copy_metadata(source, &pending.join("metadata"))?;
        let mut receipt = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(pending.join("receipt"))?;
        receipt.write_all(&key)?;
        receipt.write_all(&digest)?;
        remove(&ready)?;
        fs::rename(&pending, &ready)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = remove(&pending);
    }
    result
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::remove_dir_all(path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

// The receipt covers the sorted paths, entry types and full file contents.
// Neither copying nor removing a seed operates on any exposed workspace.
fn copy_metadata(source: &Path, target: &Path) -> Result<[u8; 32]> {
    let mut copy = Copy {
        hash: Sha256::new(),
        remaining_bytes: MAX_BYTES,
        remaining_entries: MAX_ENTRIES,
        buffer: [0; 64 * 1024],
    };
    copy.directory(source, target, 0)?;
    Ok(copy.hash.finalize().into())
}

struct Copy {
    hash: Sha256,
    remaining_bytes: u64,
    remaining_entries: usize,
    buffer: [u8; 64 * 1024],
}

impl Copy {
    fn directory(&mut self, source: &Path, target: &Path, depth: usize) -> Result<()> {
        let meta = source.symlink_metadata()?;
        if !meta.is_dir()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o022 != 0
            || depth > MAX_DEPTH
        {
            return Err(Error::Private);
        }
        DirBuilder::new().mode(0o700).create(target)?;
        let mut entries = fs::read_dir(source)?
            .take(self.remaining_entries + 1)
            .collect::<io::Result<Vec<_>>>()?;
        self.remaining_entries = self
            .remaining_entries
            .checked_sub(entries.len())
            .ok_or(Error::Configuration)?;
        entries.sort_by_key(|entry| entry.file_name());
        self.hash.update((entries.len() as u64).to_be_bytes());
        for entry in entries {
            let name = entry.file_name();
            let name_bytes = name.as_encoded_bytes();
            self.hash.update((name_bytes.len() as u64).to_be_bytes());
            self.hash.update(name_bytes);
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.hash.update(b"d");
                self.directory(&entry.path(), &target.join(name), depth + 1)?;
            } else if kind.is_file() {
                self.hash.update(b"f");
                self.file(&entry.path(), &target.join(name))?;
            } else {
                return Err(Error::Private);
            }
        }
        Ok(())
    }

    fn file(&mut self, source: &Path, target: &Path) -> Result<()> {
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(source)?;
        let meta = input.metadata()?;
        if !meta.is_file()
            || meta.uid() != rustix::process::geteuid().as_raw()
            || meta.mode() & 0o022 != 0
            || meta.nlink() != 1
        {
            return Err(Error::Private);
        }
        self.remaining_bytes = self
            .remaining_bytes
            .checked_sub(meta.len())
            .ok_or(Error::Configuration)?;
        self.hash.update(meta.len().to_be_bytes());
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(target)?;
        let mut remaining = meta.len();
        while remaining != 0 {
            let length = remaining.min(self.buffer.len() as u64) as usize;
            let chunk = &mut self.buffer[..length];
            input.read_exact(chunk)?;
            self.hash.update(&*chunk);
            output.write_all(chunk)?;
            remaining -= length as u64;
        }
        if input.read(&mut [0])? != 0 {
            return Err(Error::Identity);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use treeharbor_core::ObjectId;

    #[test]
    fn copied_metadata_is_private_independent_and_checked() {
        let root = tempfile::Builder::new()
            .permissions(Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let cache = private::make_directory(&root.path().join("cache")).unwrap();
        let source = private::make_directory(&root.path().join("source")).unwrap();
        let key = [7; 32];
        let head = source.join("HEAD");
        fs::write(&head, b"verified").unwrap();
        remember(&cache, &source, key).unwrap();
        let receipt = fs::read(cache.join("ready/receipt")).unwrap();
        let digest = ObjectId::from_bytes(receipt[32..].try_into().unwrap());
        // Independent hashlib over the v1 directory count, name, type and bytes.
        assert_eq!(
            digest.hex(),
            "3fc0f2d2ccfe35a3e2bea2cc005c4f19bde62e1dabb3f5b0755e4703bea1c557"
        );
        assert!(restore(&cache, root.path(), [8; 32]).unwrap().is_none());

        let first = restore(&cache, root.path(), key).unwrap().unwrap();
        let second = restore(&cache, root.path(), key).unwrap().unwrap();
        let first_head = first.path().join("metadata/HEAD");
        let second_head = second.path().join("metadata/HEAD");
        assert_eq!(second_head.metadata().unwrap().mode() & 0o777, 0o600);
        assert_ne!(
            first_head.metadata().unwrap().ino(),
            second_head.metadata().unwrap().ino()
        );
        fs::write(first_head, b"agent write").unwrap();
        assert_eq!(fs::read(second_head).unwrap(), b"verified");
        assert_eq!(fs::read(&head).unwrap(), b"verified");

        fs::write(cache.join("ready/metadata/HEAD"), b"tampered").unwrap();
        assert!(restore(&cache, root.path(), key).unwrap().is_none());
        fs::remove_file(&head).unwrap();
        std::os::unix::fs::symlink(cache.join("ready/receipt"), head).unwrap();
        assert!(remember(&cache, &source, key).is_err());
        assert!(!cache.join("pending").exists());
    }
}
