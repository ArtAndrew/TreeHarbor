//! Fresh-check inputs only. Never use this to infer that a stopped agent's edits
//! may be discarded. Caller holds lifecycle exclusion and prevents other writers.
use crate::{Error, Limits, MaterializedTree, Result};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, openat, statat};
use std::{
    collections::BTreeSet,
    ffi::OsStr,
    fs::File,
    io::{self, Read},
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
};
use treeharbor_core::{EntryMode, Object, RepoPath};
fn unix<T>(r: std::result::Result<T, rustix::io::Errno>) -> Result<T> {
    Ok(r.map_err(io::Error::from)?)
}
fn open_dir(parent: &File, path: &[u8]) -> Result<File> {
    let mut dir = parent.try_clone()?;
    for part in path.split(|b| *b == b'/').filter(|p| !p.is_empty()) {
        dir = File::from(unix(openat(
            &dir,
            OsStr::from_bytes(part),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ))?);
    }
    Ok(dir)
}
impl MaterializedTree {
    /// Verify a fresh full preparation byte-for-byte, including the exact entry
    /// set, executable bits and symlink payloads. No path traversal follows links.
    /// Limits bound IO/memory; source bytes are read once per file, not cached.
    pub fn verify_pristine(&self, limits: Limits) -> Result<()> {
        let limits = limits.validate()?;
        if self.manifest.preserved_local_state || self.manifest.selection.paths.is_some() {
            return Err(Error::InvalidBaseline);
        }
        let parent = File::from(unix(rustix::fs::open(
            &self.root,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        ))?);
        let root = open_dir(&parent, b"files")?;
        let mut pending = vec![Vec::<u8>::new()];
        let mut seen = BTreeSet::new();
        let mut paths = 0u64;
        let mut bytes = 0u64;
        while let Some(path) = pending.pop() {
            let dir = open_dir(&root, &path)?;
            for entry in unix(Dir::read_from(&dir))? {
                let entry = unix(entry)?;
                let name = entry.file_name().to_bytes();
                if name == b"." || name == b".." {
                    continue;
                }
                let mut relative = path.clone();
                if !relative.is_empty() {
                    relative.push(b'/');
                }
                relative.extend_from_slice(name);
                paths = paths
                    .checked_add(relative.len() as u64)
                    .ok_or(Error::Limit)?;
                if seen.len() >= limits.max_entries || paths > limits.max_path_bytes {
                    return Err(Error::Limit);
                }
                let key = RepoPath::new(relative.clone())?;
                let expected = self
                    .manifest
                    .entries
                    .get(&key)
                    .ok_or(Error::InvalidBaseline)?;
                if !seen.insert(key) {
                    return Err(Error::InvalidBaseline);
                }
                let name = OsStr::from_bytes(name);
                let before = unix(statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW))?;
                let kind = FileType::from_raw_mode(before.st_mode);
                if expected.mode == EntryMode::Directory {
                    if kind != FileType::Directory {
                        return Err(Error::InvalidBaseline);
                    }
                    pending.push(relative);
                    continue;
                }
                let size = u64::try_from(before.st_size).map_err(|_| Error::InvalidBaseline)?;
                bytes = bytes.checked_add(size).ok_or(Error::Limit)?;
                if size > limits.max_file_bytes
                    || size > treeharbor_core::MAX_PAYLOAD as u64
                    || bytes > limits.max_workspace_bytes
                {
                    return Err(Error::Limit);
                }
                let data = match expected.mode {
                    EntryMode::File | EntryMode::Executable => {
                        if kind != FileType::RegularFile
                            || before.st_nlink != 1
                            || (before.st_mode & 0o111 != 0)
                                != (expected.mode == EntryMode::Executable)
                            || before.st_mode & 0o7000 != 0
                        {
                            return Err(Error::InvalidBaseline);
                        }
                        let file = File::from(unix(openat(
                            &dir,
                            name,
                            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                            Mode::empty(),
                        ))?);
                        let meta = file.metadata()?;
                        if !meta.is_file()
                            || meta.dev() != before.st_dev as u64
                            || meta.ino() != before.st_ino as u64
                        {
                            return Err(Error::InvalidBaseline);
                        }
                        let mut data = Vec::new();
                        (&file).take(size + 1).read_to_end(&mut data)?;
                        let after = file.metadata()?;
                        if data.len() as u64 != size
                            || after.len() != size
                            || after.mtime() != meta.mtime()
                            || after.mtime_nsec() != meta.mtime_nsec()
                            || after.ctime() != meta.ctime()
                            || after.ctime_nsec() != meta.ctime_nsec()
                        {
                            return Err(Error::InvalidBaseline);
                        }
                        data
                    }
                    EntryMode::Symlink => {
                        if kind != FileType::Symlink {
                            return Err(Error::InvalidBaseline);
                        }
                        let mut data =
                            vec![0u8; usize::try_from(size).map_err(|_| Error::Limit)? + 1];
                        let n = unix(rustix::fs::readlinkat_raw(&dir, name, &mut data))?;
                        if n as u64 != size {
                            return Err(Error::InvalidBaseline);
                        }
                        data.truncate(n);
                        data
                    }
                    EntryMode::Directory => unreachable!(),
                };
                if Object::Blob(data).id()? != expected.target {
                    return Err(Error::InvalidBaseline);
                }
            }
        }
        if seen.len() != self.manifest.entries.len() {
            return Err(Error::InvalidBaseline);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Selection, filesystem::Manifest};
    use std::{
        collections::BTreeMap,
        fs,
        os::unix::fs::{PermissionsExt, symlink},
        sync::Arc,
    };
    use treeharbor_core::{EntryRef, ObjectId, SourceProfile};
    fn fixture() -> (tempfile::TempDir, MaterializedTree) {
        let dir = tempfile::tempdir().unwrap();
        let files = dir.path().join("files");
        fs::create_dir(&files).unwrap();
        fs::create_dir(files.join("dir")).unwrap();
        fs::write(files.join("dir/file"), b"original\n").unwrap();
        fs::set_permissions(files.join("dir/file"), fs::Permissions::from_mode(0o600)).unwrap();
        symlink("dir/file", files.join("link")).unwrap();
        let entries = BTreeMap::from([
            (
                RepoPath::new(b"dir".to_vec()).unwrap(),
                EntryRef {
                    mode: EntryMode::Directory,
                    target: ObjectId::from_bytes([1; 32]),
                },
            ),
            (
                RepoPath::new(b"dir/file".to_vec()).unwrap(),
                EntryRef {
                    mode: EntryMode::File,
                    target: Object::Blob(b"original\n".to_vec()).id().unwrap(),
                },
            ),
            (
                RepoPath::new(b"link".to_vec()).unwrap(),
                EntryRef {
                    mode: EntryMode::Symlink,
                    target: Object::Blob(b"dir/file".to_vec()).id().unwrap(),
                },
            ),
        ]);
        let tree = MaterializedTree {
            root: dir.path().to_owned(),
            manifest: Arc::new(Manifest {
                tenant: "test".into(),
                repository: "test".into(),
                snapshot: ObjectId::from_bytes([2; 32]),
                selection: Selection::from_profile(&SourceProfile::full()),
                entries,
                preserved_local_state: false,
            }),
            stats: None,
        };
        (dir, tree)
    }
    #[test]
    fn fresh_check_detects_changed_bytes_modes_entries_links_and_aliases() {
        let (_dir, tree) = fixture();
        tree.verify_pristine(Limits::default()).unwrap();
        for attack in 0..8 {
            let (_dir, tree) = fixture();
            let files = tree.path();
            match attack {
                0 => fs::write(files.join("dir/file"), b"changed!\n").unwrap(),
                1 => fs::remove_file(files.join("dir/file")).unwrap(),
                2 => fs::write(files.join("untracked"), b"").unwrap(),
                3 => fs::create_dir(files.join("untracked-dir")).unwrap(),
                4 => fs::set_permissions(files.join("dir/file"), fs::Permissions::from_mode(0o700))
                    .unwrap(),
                5 => {
                    fs::remove_file(files.join("link")).unwrap();
                    symlink("missing", files.join("link")).unwrap();
                }
                6 => {
                    fs::rename(files.join("dir"), files.join("moved")).unwrap();
                    symlink("moved", files.join("dir")).unwrap();
                }
                7 => {
                    fs::hard_link(files.join("dir/file"), _dir.path().join("alias")).unwrap();
                }
                _ => unreachable!(),
            }
            assert!(
                tree.verify_pristine(Limits::default()).is_err(),
                "attack {attack}"
            );
        }
    }
    #[test]
    fn fresh_check_obeys_bounds_and_rejects_preserved_or_sparse_inputs() {
        let (_dir, mut tree) = fixture();
        for limits in [
            Limits {
                max_entries: 1,
                ..Limits::default()
            },
            Limits {
                max_path_bytes: 1,
                ..Limits::default()
            },
            Limits {
                max_file_bytes: 1,
                ..Limits::default()
            },
            Limits {
                max_workspace_bytes: 1,
                ..Limits::default()
            },
        ] {
            assert!(tree.verify_pristine(limits).is_err());
        }
        Arc::get_mut(&mut tree.manifest)
            .unwrap()
            .preserved_local_state = true;
        assert!(tree.verify_pristine(Limits::default()).is_err());
    }
}
