//! Isolated loading of a local Git bundle into an empty, temporary repository.
use crate::{Error, HashAlgorithm, Result, check_deadline, process::Git};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Instant,
};
use treeharbor_core::ObjectId;

pub(super) fn scratch(parent: &Path, prefix: &str) -> Result<tempfile::TempDir> {
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Configuration);
    }
    let dir = tempfile::Builder::new().prefix(prefix).tempdir_in(parent)?;
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))?;
    Ok(dir)
}

pub(super) fn digest(mut file: File, limit: u64, deadline: Instant) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 65536];
    loop {
        check_deadline(deadline)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > limit {
            return Err(Error::Limit);
        }
        hash.update(&buffer[..n]);
    }
    Ok((ObjectId::from_bytes(hash.finalize().into()).hex(), bytes))
}

pub(super) fn copy(source: &Path, dest: &Path, limit: u64, deadline: Instant) -> Result<()> {
    let input = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(source)?;
    if !input.metadata()?.is_file() || input.metadata()?.len() > limit {
        return Err(Error::Configuration);
    }
    let mut output = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(dest)?;
    let mut input = input.take(limit + 1);
    let mut buffer = [0u8; 65536];
    let mut bytes = 0u64;
    loop {
        check_deadline(deadline)?;
        let n = input.read(&mut buffer)?;
        if n == 0 {
            return Ok(());
        }
        bytes += n as u64;
        if bytes > limit {
            return Err(Error::Limit);
        }
        output.write_all(&buffer[..n])?;
    }
}

pub(super) fn unpack(
    git: &Git<'_>,
    scratch: &Path,
    bundle: &Path,
    algorithm: HashAlgorithm,
    max_listing: u64,
) -> Result<(PathBuf, File)> {
    let repo = scratch.join("source.git");
    let template = scratch.join("empty-template");
    fs::create_dir(&repo)?;
    fs::create_dir(&template)?;
    let mut template_arg = std::ffi::OsString::from("--template=");
    template_arg.push(&template);
    git.run(
        Some(&repo),
        "init importer",
        &[
            "init".into(),
            "--bare".into(),
            "--quiet".into(),
            format!("--object-format={}", algorithm.name()).into(),
            template_arg,
            repo.as_os_str().to_owned(),
        ],
        None,
        4096,
    )?;
    git.run(
        Some(&repo),
        "verify source bundle",
        &[
            "bundle".into(),
            "verify".into(),
            "--quiet".into(),
            bundle.as_os_str().to_owned(),
        ],
        None,
        1 << 20,
    )?;
    let unbundled = git.capture(
        Some(&repo),
        "unbundle source",
        &[
            "bundle".into(),
            "unbundle".into(),
            bundle.as_os_str().to_owned(),
        ],
        None,
        max_listing,
    )?;
    Ok((repo, unbundled))
}
