//! Descriptor-relative removal inside an already quiescent owned placement.
//! This module does not establish runtime quiescence or metadata authorization.
use crate::{Error, Result};
use rustix::fs::{self as unix, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use serde::{Deserialize, Serialize};
use std::{
    ffi::OsStr,
    fs::File,
    io,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 4_000_000;
const MAX_DEPTH: usize = 256;
const MAX_NAME_BYTES: usize = 64 * 1024 * 1024;
struct Budget {
    entries: usize,
    bytes: usize,
    visited: usize,
}
impl Budget {
    fn new() -> Self {
        Self {
            entries: MAX_ENTRIES,
            bytes: MAX_NAME_BYTES,
            visited: 0,
        }
    }
    fn reserve(&mut self, bytes: usize) -> Result<()> {
        if self.entries == 0 || bytes > self.bytes {
            return Err(Error::CleanupLimit);
        }
        self.entries -= 1;
        self.bytes -= bytes;
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Identity {
    pub path: PathBuf,
    pub device: u64,
    pub inode: u64,
}
pub(crate) struct Root {
    file: File,
    identity: Identity,
}
fn call<T>(r: rustix::io::Result<T>) -> Result<T> {
    Ok(r.map_err(io::Error::from)?)
}
fn flags() -> OFlags {
    OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::DIRECTORY
}
fn same(a: &Stat, b: &Stat) -> Result<()> {
    if a.st_dev != b.st_dev
        || a.st_ino != b.st_ino
        || FileType::from_raw_mode(a.st_mode) != FileType::from_raw_mode(b.st_mode)
    {
        return Err(Error::Identity);
    }
    Ok(())
}
// dev_t differs between supported Linux and macOS targets.
#[allow(clippy::unnecessary_cast)]
fn device(s: &Stat) -> u64 {
    s.st_dev as u64
}
impl Root {
    pub fn open(path: &Path) -> Result<Self> {
        let canonical = crate::private::directory(path)?;
        if canonical != path {
            return Err(Error::Identity);
        }
        let file = File::from(call(unix::open(path, flags(), Mode::empty()))?);
        let meta = file.metadata()?;
        let result = Self {
            file,
            identity: Identity {
                path: path.into(),
                device: meta.dev(),
                inode: meta.ino(),
            },
        };
        result.verify()?;
        Ok(result)
    }
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    pub fn verify(&self) -> Result<()> {
        let current = call(unix::statat(
            unix::CWD,
            &self.identity.path,
            AtFlags::SYMLINK_NOFOLLOW,
        ))?;
        let held = call(unix::fstat(&self.file))?;
        same(&current, &held)?;
        if device(&held) != self.identity.device
            || held.st_ino != self.identity.inode
            || FileType::from_raw_mode(held.st_mode) != FileType::Directory
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
    pub fn names(&self) -> Result<Vec<Vec<u8>>> {
        names(&self.file, &mut Budget::new())
    }
    pub fn empty(&self) -> Result<bool> {
        for entry in call(Dir::read_from(&self.file))? {
            let entry = call(entry)?;
            if !matches!(entry.file_name().to_bytes(), b"." | b"..") {
                return Ok(false);
            }
        }
        Ok(true)
    }
    /// The caller owns every selected child. This never removes the root inode.
    /// Partial failure is resumable, but must not produce a completion receipt.
    pub fn remove(&self, selected: &[Vec<u8>], check: impl Fn() -> Result<()>) -> Result<()> {
        self.verify()?;
        check()?;
        let mut budget = Budget::new();
        for name in selected {
            budget.reserve(name.len())?;
        }
        for name in selected {
            remove(
                &self.file,
                name,
                self.identity.device,
                0,
                &mut budget,
                &check,
            )?;
        }
        self.file.sync_all()?;
        self.verify()?;
        check()?;
        if !self.empty()? {
            return Err(Error::Incomplete);
        }
        Ok(())
    }
    pub fn sync(&self) -> Result<()> {
        Ok(self.file.sync_all()?)
    }
}
fn names(dir: &File, budget: &mut Budget) -> Result<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    for entry in call(Dir::read_from(dir))? {
        let entry = call(entry)?;
        let name = entry.file_name().to_bytes();
        if matches!(name, b"." | b"..") {
            continue;
        }
        budget.reserve(name.len())?;
        out.push(name.to_vec());
    }
    out.sort();
    Ok(out)
}

#[cfg(target_os = "linux")]
fn repair_directory(parent: &File, name: &OsStr, expected: &Stat) -> Result<()> {
    use std::os::fd::AsRawFd;
    // O_PATH + NOFOLLOW holds this exact directory even at mode 000. The only
    // followed link below is the kernel-owned /proc/self/fd entry for OUR held
    // descriptor; no repository path is passed to chmod. rustix 1.1.5 does not
    // implement Linux fchmodat2/AT_EMPTY_PATH or no-follow chmodat.
    let held = File::from(call(unix::openat(
        parent,
        name,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    ))?);
    same(expected, &call(unix::fstat(&held))?)?;
    let proc = File::from(call(unix::open(
        "/proc/self/fd",
        flags() & !OFlags::NOFOLLOW,
        Mode::empty(),
    ))?);
    if call(unix::fstatfs(&proc))?.f_type != 0x9fa0 {
        return Err(Error::Identity);
    }
    call(unix::chmodat(
        &proc,
        held.as_raw_fd().to_string(),
        Mode::RWXU,
        AtFlags::empty(),
    ))?;
    same(expected, &call(unix::fstat(&held))?)?;
    Ok(())
}
#[cfg(not(target_os = "linux"))]
fn repair_directory(parent: &File, name: &OsStr, _: &Stat) -> Result<()> {
    // Supported macOS local-file tests; managed execution remains Linux only.
    call(unix::chmodat(
        parent,
        name,
        Mode::RWXU,
        AtFlags::SYMLINK_NOFOLLOW,
    ))
}
fn remove(
    parent: &File,
    name: &[u8],
    dev: u64,
    depth: usize,
    budget: &mut Budget,
    check: &impl Fn() -> Result<()>,
) -> Result<()> {
    if name.is_empty() || matches!(name, b"." | b"..") || name.contains(&b'/') || name.contains(&0)
    {
        return Err(Error::Identity);
    }
    if depth >= MAX_DEPTH {
        return Err(Error::CleanupLimit);
    }
    budget.visited += 1;
    if budget.visited.is_multiple_of(256) {
        check()?;
    }
    let name = OsStr::from_bytes(name);
    let before = match unix::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => return Ok(()),
        other => call(other)?,
    };
    if device(&before) != dev {
        return Err(Error::Identity);
    }
    if FileType::from_raw_mode(before.st_mode) == FileType::Directory {
        if before.st_uid != rustix::process::geteuid().as_raw() {
            return Err(Error::Private);
        }
        // Only directory mode is repaired. Regular files (including hard links)
        // and link targets are never opened, written or chmod'ed.
        if before.st_mode & 0o700 != 0o700 {
            repair_directory(parent, name, &before)?;
        }
        let child = File::from(call(unix::openat(parent, name, flags(), Mode::empty()))?);
        same(&before, &call(unix::fstat(&child))?)?;
        for entry in names(&child, budget)? {
            remove(&child, &entry, dev, depth + 1, budget, check)?;
        }
        child.sync_all()?;
        same(
            &before,
            &call(unix::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW))?,
        )?;
        call(unix::unlinkat(parent, name, AtFlags::REMOVEDIR))?;
    } else {
        // Unlink the directory entry itself, including FIFO/socket/device/link.
        call(unix::unlinkat(parent, name, AtFlags::empty()))?;
    }
    // Each directory is synced after its children, and the root after all
    // removals. A partial failure has no completion receipt and is retried.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    fn root() -> (tempfile::TempDir, Root) {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let path = temp.path().canonicalize().unwrap();
        (temp, Root::open(&path).unwrap())
    }
    #[test]
    fn deletion_keeps_root_and_never_mutates_symlink_or_hardlink_targets() {
        let (_temp, root) = root();
        let outside = tempfile::tempdir().unwrap();
        let sentinel = outside.path().join("retain");
        fs::write(&sentinel, b"retained").unwrap();
        fs::set_permissions(&sentinel, fs::Permissions::from_mode(0o400)).unwrap();
        let tree = root.identity.path.join("tree");
        fs::create_dir(&tree).unwrap();
        symlink(outside.path(), tree.join("outside-dir")).unwrap();
        symlink(&sentinel, tree.join("outside-file")).unwrap();
        fs::hard_link(&sentinel, tree.join("linked-file")).unwrap();
        fs::create_dir(tree.join("locked")).unwrap();
        fs::write(tree.join("locked/file"), b"delete").unwrap();
        let socket = std::os::unix::net::UnixListener::bind(tree.join("socket")).unwrap();
        #[cfg(target_os = "linux")]
        call(unix::mkfifoat(&root.file, "pipe", Mode::RUSR | Mode::WUSR)).unwrap();
        fs::set_permissions(tree.join("locked"), fs::Permissions::from_mode(0o0)).unwrap();
        root.remove(&root.names().unwrap(), || Ok(())).unwrap();
        assert!(root.empty().unwrap());
        root.verify().unwrap();
        assert_eq!(fs::read(&sentinel).unwrap(), b"retained");
        assert_eq!(
            fs::metadata(&sentinel).unwrap().permissions().mode() & 0o777,
            0o400
        );
        drop(socket);
    }
    #[test]
    fn interrupted_cleanup_can_resume_without_following_replaced_root() {
        let (_temp, root) = root();
        for i in 0..600 {
            fs::write(root.identity.path.join(format!("item-{i:04}")), b"x").unwrap();
        }
        let count = std::cell::Cell::new(0);
        assert!(
            root.remove(&root.names().unwrap(), || {
                count.set(count.get() + 1);
                if count.get() > 1 {
                    Err(Error::Incomplete)
                } else {
                    Ok(())
                }
            })
            .is_err()
        );
        let remaining = root.names().unwrap().len();
        assert!(remaining > 0 && remaining < 600);
        let restarted = Root::open(&root.identity.path).unwrap();
        assert_eq!(root.identity(), restarted.identity());
        restarted
            .remove(&restarted.names().unwrap(), || Ok(()))
            .unwrap();
        assert!(restarted.empty().unwrap());
        let path = root.identity.path.join("sub");
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let sub = Root::open(&path).unwrap();
        fs::rename(&path, root.identity.path.join("old")).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(sub.verify().is_err());
        assert!(root.remove(&[b"../escape".to_vec()], || Ok(())).is_err());
    }
}
