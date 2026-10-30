//! Private offline copies. Directory handles anchor traversal; symlinks and
//! special files are never followed. The owner must keep every input stopped.
use super::{Error, Result};
use rustix::fs::{self as unix, AtFlags, Dir, FileType, Mode, OFlags};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::Path,
    time::Instant,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub mode: u32,
    // None denotes a directory. Empty files have Some(hash of empty bytes).
    pub sha256: Option<String>,
    pub bytes: u64,
}
pub(super) type Inventory = BTreeMap<String, Entry>;

#[derive(Clone)]
pub(super) struct Budget {
    pub remaining: u64,
    pub entries: usize,
    pub deadline: Instant,
}
impl Budget {
    pub fn tick(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(Error::Limit);
        }
        Ok(())
    }
    fn entry(&mut self, name: &str) -> Result<()> {
        self.tick()?;
        if self.entries == 0 || name.len() > 4096 {
            return Err(Error::Limit);
        }
        self.entries -= 1;
        Ok(())
    }
}

fn flags(directory: bool) -> OFlags {
    OFlags::RDONLY
        | OFlags::NOFOLLOW
        | OFlags::CLOEXEC
        | OFlags::NONBLOCK
        | if directory {
            OFlags::DIRECTORY
        } else {
            OFlags::empty()
        }
}
pub(super) fn open(path: &Path, directory: bool) -> Result<File> {
    Ok(File::from(unix::open(
        path,
        flags(directory),
        Mode::empty(),
    )?))
}
pub(super) fn mkdir(path: &Path) -> Result<()> {
    fs::DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}
fn stable(a: &Metadata, b: &Metadata) -> Result<()> {
    if (
        a.dev(),
        a.ino(),
        a.len(),
        a.mode(),
        a.nlink(),
        a.uid(),
        a.mtime(),
        a.mtime_nsec(),
        a.ctime(),
        a.ctime_nsec(),
    ) != (
        b.dev(),
        b.ino(),
        b.len(),
        b.mode(),
        b.nlink(),
        b.uid(),
        b.mtime(),
        b.mtime_nsec(),
        b.ctime(),
        b.ctime_nsec(),
    ) {
        return Err(Error::Changed);
    }
    Ok(())
}
fn private(meta: &Metadata) -> Result<()> {
    if meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o7077 != 0
        || (!meta.is_dir() && (!meta.is_file() || meta.nlink() != 1))
    {
        return Err(Error::Unsafe);
    }
    Ok(())
}

/// With `destination=None`, read and hash the same layout without creating files.
pub(super) fn walk(
    source: &File,
    destination: Option<&Path>,
    relative: &str,
    inventory: &mut Inventory,
    budget: &mut Budget,
    depth: usize,
) -> Result<()> {
    budget.entry(relative)?;
    if depth > 64 {
        return Err(Error::Limit);
    }
    let before = source.metadata()?;
    private(&before)?;
    let entry = if before.is_dir() {
        if let Some(path) = destination {
            mkdir(path)?;
        }
        for item in Dir::read_from(source)? {
            let item = item?;
            let bytes = item.file_name().to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            let name = std::str::from_utf8(bytes).map_err(|_| Error::Unsafe)?;
            if name.contains('/') || name.contains('\\') {
                return Err(Error::Unsafe);
            }
            let stat = unix::statat(source, name, AtFlags::SYMLINK_NOFOLLOW)?;
            let kind = FileType::from_raw_mode(stat.st_mode);
            if kind != FileType::Directory && kind != FileType::RegularFile {
                return Err(Error::Unsafe);
            }
            let child = File::from(unix::openat(
                source,
                name,
                flags(kind == FileType::Directory),
                Mode::empty(),
            )?);
            let meta = child.metadata()?;
            if meta.dev() != stat.st_dev as u64 || meta.ino() != stat.st_ino as u64 {
                return Err(Error::Changed);
            }
            let child_relative = if relative.is_empty() {
                name.to_owned()
            } else {
                format!("{relative}/{name}")
            };
            let target = destination.map(|path| path.join(name));
            walk(
                &child,
                target.as_deref(),
                &child_relative,
                inventory,
                budget,
                depth + 1,
            )?;
            // A rename/replacement of this entry is not a stopped source.
            let after = unix::statat(source, name, AtFlags::SYMLINK_NOFOLLOW)?;
            if stat.st_dev != after.st_dev || stat.st_ino != after.st_ino {
                return Err(Error::Changed);
            }
        }
        if let Some(path) = destination {
            fs::set_permissions(path, fs::Permissions::from_mode(before.mode() & 0o700))?;
            File::open(path)?.sync_all()?;
        }
        Entry {
            mode: before.mode() & 0o700,
            sha256: None,
            bytes: 0,
        }
    } else {
        if before.len() > budget.remaining {
            return Err(Error::Limit);
        }
        let mut target = destination
            .map(|path| {
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(path)
            })
            .transpose()?;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        let mut read = source;
        let mut bytes = 0u64;
        loop {
            budget.tick()?;
            let n = read.read(&mut buffer)?;
            if n == 0 {
                break;
            }
            if n as u64 > budget.remaining {
                return Err(Error::Limit);
            }
            budget.remaining -= n as u64;
            bytes += n as u64;
            hash.update(&buffer[..n]);
            if let Some(target) = &mut target {
                target.write_all(&buffer[..n])?;
            }
        }
        if bytes != before.len() {
            return Err(Error::Changed);
        }
        if let Some(target) = target {
            target.set_permissions(fs::Permissions::from_mode(before.mode() & 0o700))?;
            target.sync_all()?;
        }
        Entry {
            mode: before.mode() & 0o700,
            sha256: Some(treeharbor_core::ObjectId::from_bytes(hash.finalize().into()).hex()),
            bytes,
        }
    };
    stable(&before, &source.metadata()?)?;
    if inventory.insert(relative.into(), entry).is_some() {
        return Err(Error::Changed);
    }
    Ok(())
}

pub(super) fn scan(path: &Path, budget: &mut Budget) -> Result<Inventory> {
    let mut inventory = Inventory::new();
    walk(&open(path, true)?, None, "", &mut inventory, budget, 0)?;
    Ok(inventory)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::symlink, time::Duration};

    fn budget(bytes: u64) -> Budget {
        Budget {
            remaining: bytes,
            entries: 100,
            deadline: Instant::now() + Duration::from_secs(5),
        }
    }
    #[test]
    fn backup_refuses_links_special_files_and_over_budget_copies() {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in("/tmp")
            .unwrap();
        let source = root.path().join("source");
        mkdir(&source).unwrap();
        let secret = root.path().join("private-outside-input");
        crate::files::write_private(&secret, b"must not traverse").unwrap();
        symlink(&secret, source.join("link")).unwrap();
        assert!(matches!(
            scan(&source, &mut budget(100)),
            Err(Error::Unsafe)
        ));
        fs::remove_file(source.join("link")).unwrap();
        fs::hard_link(&secret, source.join("hardlink")).unwrap();
        assert!(matches!(
            scan(&source, &mut budget(100)),
            Err(Error::Unsafe)
        ));
        fs::remove_file(source.join("hardlink")).unwrap();
        let socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();
        assert!(matches!(
            scan(&source, &mut budget(100)),
            Err(Error::Unsafe)
        ));
        drop(socket);
        fs::remove_file(source.join("socket")).unwrap();
        crate::files::write_private(&source.join("data"), b"0123456789").unwrap();
        let target = root.path().join("copy");
        assert!(matches!(
            walk(
                &open(&source, true).unwrap(),
                Some(&target),
                "objects",
                &mut Inventory::new(),
                &mut budget(9),
                0
            ),
            Err(Error::Limit)
        ));
        assert_eq!(fs::read(source.join("data")).unwrap(), b"0123456789");
        assert!(!target.join("data").exists());
        fs::set_permissions(source.join("data"), fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(
            scan(&source, &mut budget(100)),
            Err(Error::Unsafe)
        ));
    }
}
