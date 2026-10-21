//! Private local configuration/credential files; no source-workspace file access.
use crate::{Error, Result};
use rustix::fs::OFlags;
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub fn private_directory(path: &Path) -> Result<PathBuf> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir()
        || meta.permissions().readonly()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::UnsafeFile);
    }
    Ok(path.canonicalize()?)
}
pub fn private_file(file: &File) -> Result<()> {
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(Error::UnsafeFile);
    }
    Ok(())
}
pub fn read_private(path: &Path, max: usize) -> Result<Vec<u8>> {
    private_directory(path.parent().ok_or(Error::UnsafeFile)?)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    private_file(&file)?;
    if file.metadata()?.len() > max as u64 {
        return Err(Error::Size);
    }
    let mut bytes = Vec::new();
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::Size);
    }
    Ok(bytes)
}
/// Atomic no-clobber publication. A successful return means file and containing
/// directory were synced. The caller handles any uncertain post-publication IO error.
pub fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = private_directory(path.parent().ok_or(Error::UnsafeFile)?)?;
    let mut file = tempfile::NamedTempFile::new_in(&parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    file.persist_noclobber(path)
        .map_err(|error| Error::Io(error.error))?;
    File::open(parent)?.sync_all()?;
    Ok(())
}
