use super::*;
use rustix::fs::{self as unix, AtFlags, Dir, FileType, Mode, OFlags, Stat};
use std::{
    ffi::OsStr,
    io::{self, Read},
    os::unix::ffi::OsStrExt,
};
use treeharbor_core::{Directory, Entry, Name};
fn call<T>(r: rustix::io::Result<T>) -> Result<T> {
    Ok(r.map_err(io::Error::from)?)
}
fn flags(directory: bool) -> OFlags {
    let f = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    if directory { f | OFlags::DIRECTORY } else { f }
}
fn stat(parent: &File, name: &[u8]) -> Result<Stat> {
    call(unix::statat(
        parent,
        OsStr::from_bytes(name),
        AtFlags::SYMLINK_NOFOLLOW,
    ))
}
fn stable(a: &Stat, b: &Stat) -> Result<()> {
    if a.st_dev != b.st_dev
        || a.st_ino != b.st_ino
        || a.st_mode != b.st_mode
        || a.st_nlink != b.st_nlink
        || a.st_size != b.st_size
        || a.st_mtime != b.st_mtime
        || a.st_mtime_nsec != b.st_mtime_nsec
        || a.st_ctime != b.st_ctime
        || a.st_ctime_nsec != b.st_ctime_nsec
    {
        return Err(Error::CaptureConflict);
    }
    Ok(())
}
fn parent(root: &File, path: &RepoPath, device: u64) -> Result<Option<File>> {
    let parts: Vec<_> = path.components().collect();
    let mut current = root.try_clone()?;
    for part in &parts[..parts.len() - 1] {
        let before = match stat(&current, part) {
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            other => other?,
        };
        if before.st_dev as u64 != device
            || FileType::from_raw_mode(before.st_mode) != FileType::Directory
        {
            return Err(Error::UnsafePath("capture ancestor"));
        }
        let next = File::from(call(unix::openat(
            &current,
            OsStr::from_bytes(part),
            flags(true),
            Mode::empty(),
        ))?);
        stable(&before, &call(unix::fstat(&next))?)?;
        current = next;
    }
    Ok(Some(current))
}
pub(super) struct Root {
    pub file: File,
    path: PathBuf,
    before: Stat,
}
impl Root {
    pub fn open(path: &Path) -> Result<Self> {
        let before = call(unix::statat(unix::CWD, path, AtFlags::SYMLINK_NOFOLLOW))?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return Err(Error::UnsafePath("capture root"));
        }
        let file = File::from(call(unix::open(path, flags(true), Mode::empty()))?);
        stable(&before, &call(unix::fstat(&file))?)?;
        Ok(Self {
            file,
            path: path.into(),
            before,
        })
    }
    pub fn device(&self) -> u64 {
        self.before.st_dev as u64
    }
    pub fn confirm(&self) -> Result<()> {
        stable(&self.before, &call(unix::fstat(&self.file))?)?;
        stable(
            &self.before,
            &call(unix::statat(
                unix::CWD,
                &self.path,
                AtFlags::SYMLINK_NOFOLLOW,
            ))?,
        )
    }
    pub fn confirm_absent(&self, path: &RepoPath) -> Result<()> {
        let Some(dir) = parent(&self.file, path, self.device())? else {
            return Ok(());
        };
        match stat(&dir, path.components().last().unwrap()) {
            Err(Error::Io(e)) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(Error::CaptureConflict),
            Err(e) => Err(e),
        }
    }
}
impl Builder<'_> {
    pub(super) fn capture_path(&mut self, root: &File, path: &RepoPath) -> Result<EntryRef> {
        let parent = parent(root, path, self.device)?.ok_or(Error::CaptureConflict)?;
        self.entry(&parent, path)
    }
    fn entry(&mut self, parent: &File, path: &RepoPath) -> Result<EntryRef> {
        self.visit(path)?;
        let name = path.components().last().unwrap();
        let before = stat(parent, name)?;
        if before.st_dev as u64 != self.device || before.st_mode & 0o7000 != 0 {
            return Err(Error::UnsafePath("capture device or special mode"));
        }
        let kind = FileType::from_raw_mode(before.st_mode);
        let entry = if kind == FileType::Directory {
            let dir = File::from(call(unix::openat(
                parent,
                OsStr::from_bytes(name),
                flags(true),
                Mode::empty(),
            ))?);
            stable(&before, &call(unix::fstat(&dir))?)?;
            let object = self.directory(&dir, path)?;
            stable(&before, &call(unix::fstat(&dir))?)?;
            EntryRef {
                mode: EntryMode::Directory,
                target: object,
            }
        } else {
            if self
                .selection
                .exclude()
                .iter()
                .any(|p| path.is_ancestor_of(p) && self.baseline(p).is_some())
            {
                return Err(Error::CaptureConflict);
            }
            if !matches!(kind, FileType::RegularFile | FileType::Symlink) {
                return Err(Error::UnsafePath("capture special file"));
            }
            let size = u64::try_from(before.st_size).map_err(|_| Error::CaptureConflict)?;
            if size > self.limits.max_file_bytes
                || size > treeharbor_core::MAX_PAYLOAD as u64
                || size > self.limits.max_workspace_bytes.saturating_sub(self.bytes)
            {
                return Err(Error::Limit);
            }
            let (data, mode) = if kind == FileType::RegularFile {
                if before.st_nlink != 1 {
                    return Err(Error::UnsafePath("capture hard link"));
                }
                let file = File::from(call(unix::openat(
                    parent,
                    OsStr::from_bytes(name),
                    flags(false),
                    Mode::empty(),
                ))?);
                stable(&before, &call(unix::fstat(&file))?)?;
                let mut data = Vec::new();
                (&file).take(size + 1).read_to_end(&mut data)?;
                stable(&before, &call(unix::fstat(&file))?)?;
                (
                    data,
                    if before.st_mode & 0o111 != 0 {
                        EntryMode::Executable
                    } else {
                        EntryMode::File
                    },
                )
            } else {
                if size > 4096 {
                    return Err(Error::Limit);
                }
                let mut bytes = vec![0; size as usize + 1];
                let n = call(unix::readlinkat_raw(
                    parent,
                    OsStr::from_bytes(name),
                    &mut bytes,
                ))?;
                bytes.truncate(n);
                link(path, &bytes)?;
                (bytes, EntryMode::Symlink)
            };
            if data.len() as u64 != size {
                return Err(Error::CaptureConflict);
            }
            self.bytes += size;
            EntryRef {
                mode,
                target: self.put(Object::Blob(data))?,
            }
        };
        stable(&before, &stat(parent, name)?)?;
        Ok(entry)
    }
    fn directory(&mut self, dir: &File, path: &RepoPath) -> Result<ObjectId> {
        let mut names = BTreeSet::new();
        for entry in call(Dir::read_from(dir))? {
            let entry = call(entry)?;
            let name = entry.file_name().to_bytes();
            if name == b"."
                || name == b".."
                || name.eq_ignore_ascii_case(b".git")
                || name.eq_ignore_ascii_case(b".treeharbor")
            {
                continue;
            }
            Name::new(name.to_vec())?;
            if names.len() >= treeharbor_core::MAX_ENTRIES || names.len() >= self.limits.max_entries
            {
                return Err(Error::Limit);
            }
            if !names.insert(name.to_vec()) {
                return Err(Error::CaptureConflict);
            }
        }
        // A missing excluded entry still preserves the original base. If an
        // ancestor vanished, reject rather than lose the excluded descendant.
        for excluded in self.selection.exclude() {
            if path.is_ancestor_of(excluded) && self.baseline(excluded).is_some() {
                let suffix = &excluded.as_bytes()[path.as_bytes().len() + 1..];
                names.insert(suffix.split(|b| *b == b'/').next().unwrap().to_vec());
            }
        }
        let mut entries = Vec::new();
        for name in names {
            let mut bytes = path.as_bytes().to_vec();
            bytes.push(b'/');
            bytes.extend_from_slice(&name);
            let child = RepoPath::new(bytes)?;
            let value = if self.selection.excluded(&child) {
                self.visit(&child)?;
                self.dependency(self.baseline(&child))?
            } else {
                Some(self.entry(dir, &child)?)
            };
            if let Some(value) = value {
                entries.push(Entry {
                    name: Name::new(name)?,
                    value,
                });
            }
        }
        self.put(Object::Directory(Directory::new(entries)?))
    }
}
fn link(path: &RepoPath, target: &[u8]) -> Result<()> {
    if target.is_empty()
        || target.starts_with(b"/")
        || target.contains(&b'\\')
        || target.contains(&0)
    {
        return Err(Error::UnsafePath("capture symlink target"));
    }
    let mut depth = path.components().count() - 1;
    for component in target.split(|b| *b == b'/') {
        if component == b".." {
            depth = depth
                .checked_sub(1)
                .ok_or(Error::UnsafePath("capture symlink escape"))?;
        } else if component != b"." && !component.is_empty() {
            Name::new(component.to_vec())?;
            depth += 1;
        }
    }
    Ok(())
}
