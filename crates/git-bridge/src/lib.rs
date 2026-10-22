//! Optional Git interop. Does not own native refs, validation or publication rights.
//! Callers must retain every source object for the entire export (a live server
//! needs a read lease/pin; a local immutable pack has a separate lifetime).
#![cfg(unix)]
mod process;
mod stream;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use treeharbor_core::{ObjectId, ObjectStore};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Git export filesystem operation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid source object: {0}")]
    Source(#[from] treeharbor_core::Error),
    #[error("export resource limit or deadline exceeded")]
    Limit,
    #[error("unsupported native-to-Git projection: {0}")]
    Unsupported(&'static str),
    #[error("invalid export configuration or unsafe output directory")]
    Configuration,
    #[error("Git subprocess failed during {0}; no completed export was published")]
    Git(&'static str),
    #[error("Git returned an invalid projection")]
    InvalidProjection,
    #[error("invalid export manifest: {0}")]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    Sha1,
    Sha256,
}
impl HashAlgorithm {
    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
        }
    }
    fn hex_len(self) -> usize {
        match self {
            Self::Sha1 => 40,
            Self::Sha256 => 64,
        }
    }
}
#[derive(Clone, Copy)]
pub struct Limits {
    pub objects: usize,
    pub edges: usize,
    pub source_bytes: u64,
    pub stream_bytes: u64,
    /// Counts expanded paths across all snapshots, including reused trees.
    pub path_visits: usize,
    pub timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            objects: 100_000,
            edges: 2_000_000,
            source_bytes: 4 << 30,
            stream_bytes: 8 << 30,
            path_visits: 2_000_000,
            timeout: Duration::from_secs(300),
        }
    }
}
pub struct Export<'a> {
    /// Trusted operator-selected installed executable, never supplied by an agent.
    pub git: &'a Path,
    /// New directory under an existing owner-only parent; never overwritten.
    pub output: &'a Path,
    /// Nonempty, bounded mapping of full refs/heads/... names to native commits.
    pub refs: BTreeMap<String, ObjectId>,
    pub algorithm: HashAlgorithm,
    pub limits: Limits,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProjectedRef {
    pub native: String,
    pub git: String,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u8,
    pub projection: String,
    pub git_object_format: HashAlgorithm,
    pub bundle_sha256: String,
    pub bundle_bytes: u64,
    pub refs: BTreeMap<String, ProjectedRef>,
    pub commits: BTreeMap<String, String>,
    pub source_objects: usize,
    pub source_bytes: u64,
}
fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        Err(Error::Limit)
    } else {
        Ok(())
    }
}
fn private_parent(path: &Path) -> Result<PathBuf> {
    let parent = path.parent().ok_or(Error::Configuration)?;
    let meta = fs::symlink_metadata(parent)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Configuration);
    }
    let name = path.file_name().ok_or(Error::Configuration)?;
    if name == "." || name == ".." {
        return Err(Error::Configuration);
    }
    Ok(parent.canonicalize()?)
}
fn valid_ref(name: &str) -> bool {
    name.starts_with("refs/heads/")
        && name.len() <= 255
        && !name.contains("..")
        && !name.contains("@{")
        && !name.ends_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/-_.".contains(&b))
        && name
            .split('/')
            .all(|p| !p.is_empty() && !p.starts_with('.') && !p.ends_with(".lock"))
}

/// Full, self-contained projection of every reachable parent. Native source
/// bytes are never checked out, filtered, interpreted as commands, or mutated.
/// Successful return means bundle + mapping have been synced and published
/// together. An IO error after rename can mean publication is uncertain: inspect
/// the output, never overwrite it. CPU/RAM/disk isolation is an external duty.
pub fn export(store: &impl ObjectStore, request: Export<'_>) -> Result<Manifest> {
    let limits = request.limits;
    if request.refs.is_empty()
        || request.refs.len() > 128
        || request.refs.keys().any(|r| !valid_ref(r))
        || limits.objects == 0
        || limits.objects > 1_000_000
        || limits.edges == 0
        || limits.edges > 4_000_000
        || limits.source_bytes == 0
        || limits.source_bytes > 64 << 30
        || limits.stream_bytes == 0
        || limits.stream_bytes > 128 << 30
        || limits.path_visits == 0
        || limits.path_visits > 10_000_000
        || limits.timeout.is_zero()
        || limits.timeout > Duration::from_secs(3600)
        || !request.git.is_absolute()
        || !request.git.is_file()
    {
        return Err(Error::Configuration);
    }
    let deadline = Instant::now() + limits.timeout;
    let parent = private_parent(request.output)?;
    let name = request.output.file_name().ok_or(Error::Configuration)?;
    let output = parent.join(name);
    // This is only an early check; final rename is also atomic no-replace.
    if output.symlink_metadata().is_ok() {
        return Err(Error::Configuration);
    }
    let scratch = tempfile::Builder::new()
        .prefix(".treeharbor-export-work-")
        .tempdir_in(&parent)?;
    let stage = tempfile::Builder::new()
        .prefix(".treeharbor-export-ready-")
        .tempdir_in(&parent)?;
    fs::set_permissions(scratch.path(), fs::Permissions::from_mode(0o700))?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let stream_path = scratch.path().join("import.stream");
    let graph = stream::write(store, &request.refs, &stream_path, limits, deadline)?;
    let git = process::Git::new(request.git.canonicalize()?, scratch.path(), deadline);
    let repo = scratch.path().join("projection.git");
    let template = scratch.path().join("empty-template");
    fs::create_dir(&template)?;
    fs::create_dir(&repo)?;
    let mut template_arg = std::ffi::OsString::from("--template=");
    template_arg.push(&template);
    let init_args = [
        "init".into(),
        "--bare".into(),
        "--quiet".into(),
        format!("--object-format={}", request.algorithm.name()).into(),
        template_arg,
        repo.as_os_str().to_owned(),
    ];
    git.run(Some(&repo), "init", &init_args, None, 4096)?;
    git.run(
        Some(&repo),
        "fast-import",
        &[
            "fast-import".into(),
            "--quiet".into(),
            "--done".into(),
            "--date-format=raw".into(),
            "--export-marks=marks".into(),
            "--depth=0".into(),
            "--big-file-threshold=1m".into(),
        ],
        Some(&stream_path),
        4096,
    )?;
    let marks_path = repo.join("marks");
    if fs::metadata(&marks_path)?.len() > limits.objects as u64 * 100 {
        return Err(Error::Limit);
    }
    let marks = fs::read_to_string(&marks_path)?;
    let mut parsed = BTreeMap::new();
    for line in marks.lines() {
        let (number, oid) = line.split_once(' ').ok_or(Error::InvalidProjection)?;
        let number: usize = number
            .strip_prefix(':')
            .ok_or(Error::InvalidProjection)?
            .parse()
            .map_err(|_| Error::InvalidProjection)?;
        if oid.len() != request.algorithm.hex_len()
            || !oid
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || parsed.insert(number, oid.to_string()).is_some()
        {
            return Err(Error::InvalidProjection);
        }
    }
    if parsed.len() != graph.marks {
        return Err(Error::InvalidProjection);
    }
    let commits: BTreeMap<_, _> = graph
        .commits
        .iter()
        .map(|(id, mark)| {
            parsed
                .get(mark)
                .cloned()
                .map(|oid| (id.to_string(), oid))
                .ok_or(Error::InvalidProjection)
        })
        .collect::<Result<_>>()?;
    let refs: BTreeMap<_, _> = request
        .refs
        .iter()
        .map(|(name, id)| {
            Ok((
                name.clone(),
                ProjectedRef {
                    native: id.to_string(),
                    git: commits
                        .get(&id.to_string())
                        .cloned()
                        .ok_or(Error::InvalidProjection)?,
                },
            ))
        })
        .collect::<Result<_>>()?;
    git.run(
        Some(&repo),
        "fsck",
        &[
            "fsck".into(),
            "--strict".into(),
            "--no-reflogs".into(),
            "--no-dangling".into(),
        ],
        None,
        1 << 20,
    )?;
    let bundle = stage.path().join("history.bundle");
    let mut args = vec![
        "bundle".into(),
        "create".into(),
        "--quiet".into(),
        bundle.as_os_str().to_owned(),
    ];
    args.extend(request.refs.keys().map(Into::into));
    git.run(Some(&repo), "bundle", &args, None, 4096)?;
    // Verify prerequisites in a NEW empty repository, not the exporting repo.
    let empty = scratch.path().join("verify.git");
    fs::create_dir(&empty)?;
    let mut init = init_args;
    *init.last_mut().ok_or(Error::Configuration)? = empty.as_os_str().to_owned();
    git.run(Some(&empty), "init verifier", &init, None, 4096)?;
    git.run(
        Some(&empty),
        "bundle verify",
        &[
            "bundle".into(),
            "verify".into(),
            "--quiet".into(),
            bundle.as_os_str().to_owned(),
        ],
        None,
        1 << 20,
    )?;
    git.run(
        Some(&empty),
        "bundle unbundle",
        &[
            "bundle".into(),
            "unbundle".into(),
            bundle.as_os_str().to_owned(),
        ],
        None,
        128 * 400,
    )?;
    let mut verify = vec![
        "fsck".into(),
        "--strict".into(),
        "--no-reflogs".into(),
        "--no-dangling".into(),
    ];
    verify.extend(refs.values().map(|r| r.git.clone().into()));
    git.run(Some(&empty), "restored fsck", &verify, None, 1 << 20)?;
    let mut file = File::open(&bundle)?;
    if file.metadata()?.len() > limits.stream_bytes {
        return Err(Error::Limit);
    }
    fs::set_permissions(&bundle, fs::Permissions::from_mode(0o600))?;
    let mut hasher = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65536];
    loop {
        check_deadline(deadline)?;
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > limits.stream_bytes {
            return Err(Error::Limit);
        }
        hasher.update(&buffer[..n]);
    }
    file.sync_all()?;
    let manifest = Manifest {
        version: 1,
        projection: "native-v1".into(),
        git_object_format: request.algorithm,
        bundle_sha256: ObjectId::from_bytes(hasher.finalize().into()).hex(),
        bundle_bytes: bytes,
        refs,
        commits,
        source_objects: graph.objects,
        source_bytes: graph.bytes,
    };
    let mut json = File::create(stage.path().join("manifest.json"))?;
    fs::set_permissions(
        stage.path().join("manifest.json"),
        fs::Permissions::from_mode(0o600),
    )?;
    serde_json::to_writer_pretty(&mut json, &manifest)?;
    json.write_all(b"\n")?;
    json.sync_all()?;
    File::open(stage.path())?.sync_all()?;
    check_deadline(deadline)?;
    let parent_fd = File::open(&parent)?;
    rustix::fs::renameat_with(
        &parent_fd,
        stage.path().file_name().ok_or(Error::Configuration)?,
        &parent_fd,
        name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    parent_fd.sync_all()?;
    Ok(manifest)
}
