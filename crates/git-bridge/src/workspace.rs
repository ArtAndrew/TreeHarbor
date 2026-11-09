//! Build private Git metadata for an exact, already materialized working tree.
use crate::{Error, HashAlgorithm, Limits, Result, check_deadline, check_limits, private_parent};
use crate::{process::Git, stream};
use std::{
    collections::BTreeMap,
    fs::{self, File, Permissions},
    os::unix::fs::PermissionsExt,
    path::Path,
    time::{Duration, Instant},
};
use treeharbor_core::{GitOid, ObjectId, ObjectStore, RevisionRef};

pub struct WorkspaceSeed<'a> {
    /// Trusted operator-selected executable.
    pub git: &'a Path,
    /// Private, daemon-owned directory with the full pinned baseline, still
    /// inaccessible to agents. The caller holds exclusive placement ownership.
    pub files: &'a Path,
    pub native: ObjectId,
    pub expected: GitOid,
    pub limits: Limits,
}

/// Adds a private index and object database to a trusted prepared tree. The
/// caller retains the full native graph and verifies the file baseline before
/// use. Git only processes the private staging repository and never checks out
/// or filters working files. Existing metadata is never replaced, including
/// when a prior call failed after rename. The caller must reconcile or discard
/// the whole unexposed placement in that case. Never invoke this on agent output.
pub fn seed_workspace(store: &impl ObjectStore, request: WorkspaceSeed<'_>) -> Result<()> {
    check_limits(request.limits, request.git)?;
    let deadline = Instant::now() + request.limits.timeout;
    let parent = private_parent(&request.files.join(".git"))?;
    if parent.join(".git").symlink_metadata().is_ok() {
        return Err(Error::Configuration);
    }
    let staging_parent = private_parent(&parent)?;
    let scratch = tempfile::Builder::new()
        .prefix(".treeharbor-git-seed-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(staging_parent)?;
    let metadata = scratch.path().join("metadata");
    fs::create_dir(&metadata)?;
    fs::set_permissions(&metadata, Permissions::from_mode(0o700))?;
    let pack = scratch.path().join("objects.pack");
    let algorithm = match request.expected {
        GitOid::Sha1(_) => HashAlgorithm::Sha1,
        GitOid::Sha256(_) => HashAlgorithm::Sha256,
    };
    let graph = stream::write(
        store,
        &BTreeMap::from([("refs/heads/workspace".into(), request.native)]),
        &pack,
        algorithm,
        request.limits,
        deadline,
    )?;
    let oid = graph
        .commits
        .get(&request.native)
        .ok_or(Error::InvalidProjection)?;
    if format!("git:{}:{oid}", algorithm.name()) != RevisionRef::Git(request.expected).to_string() {
        return Err(Error::InvalidProjection);
    }
    let git = Git::new(request.git.canonicalize()?, scratch.path(), deadline);
    git.run(
        Some(&metadata),
        "workspace init",
        &[
            "init".into(),
            "--bare".into(),
            "--quiet".into(),
            "--template=".into(),
            format!("--object-format={}", algorithm.name()).into(),
            metadata.as_os_str().into(),
        ],
        None,
        4096,
    )?;
    git.run(
        Some(&metadata),
        "workspace index-pack",
        &["index-pack".into(), "--stdin".into(), "--strict".into()],
        Some(&pack),
        4096,
    )?;
    git.run(
        Some(&metadata),
        "workspace HEAD",
        &[
            "update-ref".into(),
            "--no-deref".into(),
            "HEAD".into(),
            oid.into(),
        ],
        None,
        4096,
    )?;
    git.run(
        Some(&metadata),
        "workspace index",
        &["read-tree".into(), oid.into()],
        None,
        4096,
    )?;
    git.run(
        Some(&metadata),
        "workspace fsck",
        &[
            "fsck".into(),
            "--strict".into(),
            "--no-reflogs".into(),
            "--no-dangling".into(),
        ],
        None,
        1 << 20,
    )?;
    for (key, value) in [
        ("core.bare", "false"),
        ("core.fileMode", "true"),
        ("core.symlinks", "true"),
        ("core.hooksPath", "/dev/null"),
    ] {
        git.run(
            Some(&metadata),
            "workspace config",
            &["config".into(), "--local".into(), key.into(), value.into()],
            None,
            4096,
        )?;
    }
    install_seed(scratch.path(), &parent, deadline)
}

/// Install a trusted, independently copied seed from `staging/metadata` into
/// the unpublished working tree. The caller verifies the cached bytes and
/// retains exclusive ownership of both paths. Existing .git is never replaced.
pub fn install_workspace_seed(staging: &Path, files: &Path, timeout: Duration) -> Result<()> {
    let parent = private_parent(&files.join(".git"))?;
    let staging = private_parent(&staging.join("metadata"))?;
    install_seed(&staging, &parent, Instant::now() + timeout)
}

fn install_seed(staging: &Path, parent: &Path, deadline: Instant) -> Result<()> {
    sync_metadata(&staging.join("metadata"), deadline)?;
    check_deadline(deadline)?;
    let source = File::open(staging)?;
    let target = File::open(parent)?;
    rustix::fs::renameat_with(
        &source,
        "metadata",
        &target,
        ".git",
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    source.sync_all()?;
    target.sync_all()?;
    Ok(())
}

fn sync_metadata(path: &Path, deadline: Instant) -> Result<()> {
    check_deadline(deadline)?;
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        check_deadline(deadline)?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            sync_metadata(&entry.path(), deadline)?;
        } else if kind.is_file() {
            File::open(entry.path())?.sync_all()?;
        } else {
            return Err(Error::Configuration);
        }
    }
    File::open(path)?.sync_all()?;
    Ok(())
}
