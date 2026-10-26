use crate::{Error, Result};
use rustix::fs::OFlags;
use serde::{Deserialize, Serialize};
#[cfg(any(target_os = "linux", test))]
use std::os::unix::ffi::OsStringExt;
#[cfg(target_os = "linux")]
use std::{fs, io::Read};
use std::{
    fs::{File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VolumeIdentity {
    pub mount: PathBuf,
    pub files: PathBuf,
    pub device: u64,
    pub mount_inode: u64,
    pub files_inode: u64,
    pub mount_id: u64,
    pub capacity_bytes: u64,
    pub limit_bytes: u64,
}
/// An operator-provisioned, private, independent ext4 filesystem, not an arbitrary
/// bind directory. Held descriptors prevent ordinary unmount; hostile privileged
/// mount changes and unverified network filesystems are outside this profile.
pub struct Volume {
    identity: VolumeIdentity,
    mount: File,
    files: File,
}
impl Volume {
    pub fn open(mount: &Path, files: &Path, limit_bytes: u64) -> Result<Self> {
        if !cfg!(target_os = "linux") {
            return Err(Error::Unsupported);
        }
        let mount = mount.canonicalize()?;
        let files = files.canonicalize()?;
        if files == mount || !files.starts_with(&mount) {
            return Err(Error::Invalid);
        }
        let mount_fd = directory(&mount)?;
        let files_fd = files_directory(&files)?;
        rustix::fs::flock(
            &mount_fd,
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|_| Error::State)?;
        let identity = inspect(&mount, &files, &mount_fd, &files_fd, limit_bytes)?;
        Ok(Self {
            identity,
            mount: mount_fd,
            files: files_fd,
        })
    }
    pub fn identity(&self) -> &VolumeIdentity {
        &self.identity
    }
    pub fn files(&self) -> &Path {
        &self.identity.files
    }
    /// Duplicate the same held mount lock and physical placement identity.
    pub fn try_clone(&self) -> Result<Self> {
        self.verify()?;
        Ok(Self {
            identity: self.identity.clone(),
            mount: self.mount.try_clone()?,
            files: self.files.try_clone()?,
        })
    }
    /// Keep the same exclusive mount ownership while selecting a descendant
    /// directory to expose to a task. Duplicated descriptors share the lock.
    pub fn for_files(&self, files: &Path) -> Result<Self> {
        self.verify()?;
        let files = files.canonicalize()?;
        if files == self.identity.files || !files.starts_with(&self.identity.files) {
            return Err(Error::Invalid);
        }
        let mount = self.mount.try_clone()?;
        let files_fd = files_directory(&files)?;
        let identity = inspect(
            &self.identity.mount,
            &files,
            &mount,
            &files_fd,
            self.identity.limit_bytes,
        )?;
        Ok(Self {
            identity,
            mount,
            files: files_fd,
        })
    }
    pub fn verify(&self) -> Result<()> {
        let current_mount = directory(&self.identity.mount)?;
        let current_files = files_directory(&self.identity.files)?;
        let current = inspect(
            &self.identity.mount,
            &self.identity.files,
            &current_mount,
            &current_files,
            self.identity.limit_bytes,
        )?;
        if current != self.identity
            || self.mount.metadata()?.ino() != current.mount_inode
            || self.files.metadata()?.ino() != current.files_inode
        {
            return Err(Error::Mismatch);
        }
        Ok(())
    }
}
fn directory(path: &Path) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Mismatch);
    }
    Ok(file)
}
// The task owns the bound directory and can chmod it, including mode 000.
// Its private mount ancestor remains operator-owned and is checked separately.
// Linux O_PATH verifies type/owner/inode without requiring read/search permission
// on that final directory; task permissions must not disable exact-ID teardown.
fn files_directory(path: &Path) -> Result<File> {
    let flags = OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    #[cfg(target_os = "linux")]
    let flags = flags | OFlags::PATH;
    // Keep the typed platform flags at the syscall boundary. Passing their raw
    // bits through std/libc OpenOptions lost O_PATH in the static Linux build.
    let file: File = rustix::fs::open(path, flags | OFlags::CLOEXEC, rustix::fs::Mode::empty())
        .map_err(std::io::Error::from)?
        .into();
    let meta = file.metadata()?;
    if !meta.is_dir() || meta.uid() != rustix::process::geteuid().as_raw() {
        return Err(Error::Mismatch);
    }
    Ok(file)
}
#[cfg(target_os = "linux")]
fn inspect(
    mount: &Path,
    files: &Path,
    mount_fd: &File,
    files_fd: &File,
    limit: u64,
) -> Result<VolumeIdentity> {
    let a = mount_fd.metadata()?;
    let b = files_fd.metadata()?;
    if a.dev() != b.dev() || a.dev() == fs::metadata(mount.parent().ok_or(Error::Invalid)?)?.dev() {
        return Err(Error::Mismatch);
    }
    let stat = rustix::fs::fstatfs(mount_fd).map_err(std::io::Error::from)?;
    if stat.f_type != 0xef53 {
        return Err(Error::Unsupported);
    }
    let capacity = stat
        .f_blocks
        .checked_mul(stat.f_bsize.try_into().map_err(|_| Error::Invalid)?)
        .ok_or(Error::Invalid)?;
    if capacity == 0 || capacity > limit {
        return Err(Error::Mismatch);
    }
    let mut bytes = Vec::new();
    File::open("/proc/self/mountinfo")?
        .take(4 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 4 * 1024 * 1024 {
        return Err(Error::Limit);
    }
    let text = std::str::from_utf8(&bytes).map_err(|_| Error::Unsupported)?;
    let id = mount_record(text, mount, a.dev())?;
    Ok(VolumeIdentity {
        mount: mount.to_owned(),
        files: files.to_owned(),
        device: a.dev(),
        mount_inode: a.ino(),
        files_inode: b.ino(),
        mount_id: id,
        capacity_bytes: capacity,
        limit_bytes: limit,
    })
}
#[cfg(not(target_os = "linux"))]
fn inspect(_: &Path, _: &Path, _: &File, _: &File, _: u64) -> Result<VolumeIdentity> {
    Err(Error::Unsupported)
}

#[cfg(any(target_os = "linux", test))]
fn mount_record(text: &str, expected: &Path, device: rustix::fs::Dev) -> Result<u64> {
    let mut found = None;
    for line in text.lines() {
        let fields: Vec<_> = line.split(' ').collect();
        if fields.len() < 10 {
            return Err(Error::Invalid);
        }
        let point = unescape(fields[4])?;
        if point != expected {
            // Any nested mount could expose or charge a different filesystem.
            if point.starts_with(expected) {
                return Err(Error::Mismatch);
            }
            continue;
        }
        if found.is_some() {
            return Err(Error::Mismatch);
        }
        let split = fields
            .iter()
            .position(|s| *s == "-")
            .ok_or(Error::Invalid)?;
        if split < 6 || fields.len() != split + 4 || fields[3] != "/" || fields[split + 1] != "ext4"
        {
            return Err(Error::Unsupported);
        }
        let options: Vec<_> = fields[5].split(',').collect();
        if !["rw", "nodev", "nosuid"]
            .iter()
            .all(|s| options.contains(s))
            || !fields[split + 3].split(',').any(|s| s == "rw")
            || fields[6..split].iter().any(|s| {
                s.starts_with("shared:")
                    || s.starts_with("master:")
                    || s.starts_with("propagate_from:")
            })
        {
            return Err(Error::Mismatch);
        }
        let (major, minor) = fields[2].split_once(':').ok_or(Error::Invalid)?;
        if major.parse::<u32>().map_err(|_| Error::Invalid)? != rustix::fs::major(device)
            || minor.parse::<u32>().map_err(|_| Error::Invalid)? != rustix::fs::minor(device)
        {
            return Err(Error::Mismatch);
        }
        found = Some(fields[0].parse::<u64>().map_err(|_| Error::Invalid)?);
    }
    found.ok_or(Error::Mismatch)
}
#[cfg(any(target_os = "linux", test))]
fn unescape(value: &str) -> Result<PathBuf> {
    let mut out = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            let escape = bytes.get(i + 1..i + 4).ok_or(Error::Invalid)?;
            out.push(match escape {
                b"040" => b' ',
                b"011" => b'\t',
                b"012" => b'\n',
                b"134" => b'\\',
                _ => return Err(Error::Invalid),
            });
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(std::ffi::OsString::from_vec(out).into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    #[test]
    fn task_directory_mode_zero_keeps_identity_readable_without_changing_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("files");
        fs::create_dir(&path).unwrap();
        let inode = fs::metadata(&path).unwrap().ino();
        fs::set_permissions(&path, fs::Permissions::from_mode(0)).unwrap();
        let opened = files_directory(&path);
        let mode = fs::metadata(&path).unwrap().mode() & 0o777;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(mode, 0);
        let opened = opened.unwrap();
        assert_eq!(opened.metadata().unwrap().ino(), inode);
        assert!(
            rustix::fs::fcntl_getfl(&opened)
                .unwrap()
                .contains(OFlags::PATH)
        );
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn task_directory_open_never_follows_a_link_or_accepts_a_regular_file() {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("directory");
        fs::create_dir(&directory).unwrap();
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&directory, &link).unwrap();
        assert!(files_directory(&link).is_err());
        let file = root.path().join("file");
        fs::write(&file, b"keep").unwrap();
        assert!(files_directory(&file).is_err());
        assert_eq!(fs::read(file).unwrap(), b"keep");
        if rustix::process::geteuid().as_raw() != 0 {
            assert!(files_directory(Path::new("/")).is_err());
        }
    }
    #[test]
    fn mount_proof_rejects_aliases_shared_and_nested_mounts() {
        let device = rustix::fs::makedev(7, 3);
        let valid = "42 2 7:3 / /private/slot rw,nodev,nosuid - ext4 /dev/loop3 rw\n";
        assert_eq!(
            mount_record(valid, Path::new("/private/slot"), device).unwrap(),
            42
        );
        for invalid in [
            valid.replace("rw,nodev,nosuid", "rw"),
            valid.replace(" - ", " shared:1 - "),
            valid.replace(" / /private", " /subdir /private"),
            valid.repeat(2),
            format!("{valid}43 42 1:1 / /private/slot/nested rw - tmpfs tmpfs rw\n"),
        ] {
            assert!(mount_record(&invalid, Path::new("/private/slot"), device).is_err());
        }
        assert_eq!(
            unescape("/slot\\040one\\134two").unwrap(),
            Path::new("/slot one\\two")
        );
    }
}
