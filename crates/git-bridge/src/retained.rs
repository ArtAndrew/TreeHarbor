//! Disposable Git metadata input produced by a trusted local import.
use crate::{
    Error, HashAlgorithm, Limits, Result, check_deadline, check_source_limits, origin,
    private_parent, process::Git,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File, OpenOptions, Permissions},
    io::{BufRead, BufReader, Read, Seek, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Instant,
};
use treeharbor_core::{GitOid, ObjectId, RevisionRef};
use uuid::Uuid;

const DIRECTORY: &str = "workspace-git-v1";
const MAX_PACK: u64 = 32 << 20;
const MAX_RECEIPT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspacePackScope {
    pub tenant: String,
    pub repository: Uuid,
    pub epoch: i64,
}

/// Supplied from the verified import or a live authorized preparation, never
/// inferred from a cache entry.
pub struct WorkspacePackIdentity<'a> {
    pub scope: &'a WorkspacePackScope,
    pub native: ObjectId,
    pub snapshot: ObjectId,
    pub origin: GitOid,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Bounds {
    pub objects: usize,
    pub source_bytes: u64,
    pub edges: usize,
    pub path_visits: usize,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    scope: WorkspacePackScope,
    native: String,
    snapshot: String,
    origin: String,
    bounds: Bounds,
    bytes: u64,
    sha256: String,
}

/// A bounded, digest-checked copy in caller-owned private scratch.
pub struct WorkspacePack {
    file: tempfile::NamedTempFile,
    pub(crate) native: ObjectId,
    pub(crate) origin: GitOid,
}

impl WorkspacePack {
    pub(crate) fn path(&self) -> &Path {
        self.file.path()
    }

    /// The caller must already hold a live source authorization for identity.
    /// Files must be in a trusted importer directory owned by the same user.
    pub fn load(
        directory: &Path,
        scratch: &Path,
        identity: WorkspacePackIdentity<'_>,
        limits: Limits,
    ) -> Result<Self> {
        check_source_limits(limits)?;
        if !directory.is_absolute() {
            return Err(Error::Configuration);
        }
        let directory = private_parent(&directory.join(DIRECTORY))?.join(DIRECTORY);
        let directory = private_parent(&directory.join("receipt.json"))?;
        let scratch = private_parent(&scratch.join("retained.pack"))?;
        let mut bytes = Vec::new();
        open_file(&directory.join("receipt.json"), true)?
            .take(MAX_RECEIPT as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_RECEIPT {
            return Err(Error::Limit);
        }
        let receipt: Receipt = serde_json::from_slice(&bytes)?;
        if receipt.version != 1
            || &receipt.scope != identity.scope
            || receipt.native != identity.native.to_string()
            || receipt.snapshot != identity.snapshot.to_string()
            || receipt.origin != RevisionRef::Git(identity.origin).to_string()
            || receipt.bounds.objects == 0
            || receipt.bounds.source_bytes == 0
            || receipt.bounds.objects > limits.objects
            || receipt.bounds.source_bytes > limits.source_bytes
            || receipt.bounds.edges > limits.edges
            || receipt.bounds.path_visits > limits.path_visits
            || receipt.bytes > MAX_PACK.min(limits.stream_bytes)
        {
            return Err(Error::InvalidProjection);
        }
        let input = open_file(&directory.join("objects.pack"), true)?;
        if input.metadata()?.len() != receipt.bytes {
            return Err(Error::InvalidProjection);
        }
        let mut file = tempfile::NamedTempFile::new_in(scratch)?;
        let (hash, bytes) = copy(
            input,
            &mut file,
            receipt.bytes,
            Instant::now() + limits.timeout,
        )?;
        if hash != receipt.sha256 || bytes != receipt.bytes {
            return Err(Error::InvalidProjection);
        }
        Ok(Self {
            file,
            native: identity.native,
            origin: identity.origin,
        })
    }
}

pub(crate) fn exact_pack(
    git: &Git<'_>,
    repo: &Path,
    algorithm: HashAlgorithm,
    expected: &BTreeSet<Vec<u8>>,
    deadline: Instant,
) -> Result<File> {
    let directory = repo.join("objects/pack");
    let entries: Vec<PathBuf> = fs::read_dir(directory)?
        .take(4)
        .map(|entry| Ok(entry?.path()))
        .collect::<std::io::Result<_>>()?;
    let Some(idx) = entries
        .iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "idx"))
    else {
        return Err(Error::InvalidProjection);
    };
    let pack = idx.with_extension("pack");
    let reverse_index = idx.with_extension("rev");
    if !entries.contains(&pack)
        || entries
            .iter()
            .any(|path| path != idx && path != &pack && path != &reverse_index)
    {
        return Err(Error::InvalidProjection);
    }
    let mut file = open_file(&pack, false)?;
    let length = file.metadata()?.len();
    if !(12..=MAX_PACK).contains(&length) {
        return Err(Error::Limit);
    }
    let mut header = [0; 12];
    file.read_exact(&mut header)?;
    let count = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    if &header[..4] != b"PACK" || count as usize != expected.len() {
        return Err(Error::InvalidProjection);
    }
    file.rewind()?;
    // fsck already validated the private pack/index. Check that no valid but
    // unrelated object from an input bundle would accompany the selected tip.
    let listed = git.capture(
        Some(repo),
        "list retained pack objects",
        &[
            "show-index".into(),
            format!("--object-format={}", algorithm.name()).into(),
        ],
        Some(idx),
        expected.len() as u64 * 128,
    )?;
    let mut seen = BTreeSet::new();
    let mut reader = BufReader::new(listed);
    for _ in 0..expected.len() {
        check_deadline(deadline)?;
        let line = crate::ingest::line(&mut reader)?;
        let oid = line
            .split(|byte| *byte == b' ')
            .nth(1)
            .ok_or(Error::InvalidProjection)?;
        let oid = origin::parse_oid(oid, algorithm)?;
        if !expected.contains(&oid) || !seen.insert(oid) {
            return Err(Error::InvalidProjection);
        }
    }
    if !reader.fill_buf()?.is_empty() {
        return Err(Error::InvalidProjection);
    }
    Ok(file)
}

pub(crate) fn save(
    directory: &Path,
    pack: File,
    identity: WorkspacePackIdentity<'_>,
    bounds: Bounds,
    deadline: Instant,
) -> Result<()> {
    let directory = private_parent(&directory.join(DIRECTORY))?;
    let staging = tempfile::Builder::new()
        .prefix(".workspace-git-")
        .permissions(Permissions::from_mode(0o700))
        .tempdir_in(&directory)?;
    let mut output = create_file(&staging.path().join("objects.pack"))?;
    let (sha256, bytes) = copy(pack, &mut output, MAX_PACK, deadline)?;
    output.sync_all()?;
    let receipt = Receipt {
        version: 1,
        scope: identity.scope.clone(),
        native: identity.native.to_string(),
        snapshot: identity.snapshot.to_string(),
        origin: RevisionRef::Git(identity.origin).to_string(),
        bounds,
        bytes,
        sha256,
    };
    let mut manifest = create_file(&staging.path().join("receipt.json"))?;
    manifest.write_all(&serde_json::to_vec(&receipt)?)?;
    manifest.sync_all()?;
    File::open(staging.path())?.sync_all()?;
    let parent = File::open(&directory)?;
    rustix::fs::renameat_with(
        &parent,
        staging.path().file_name().ok_or(Error::Configuration)?,
        &parent,
        DIRECTORY,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    parent.sync_all()?;
    Ok(())
}

fn open_file(path: &Path, owner_only: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32)
        .open(path)?;
    let meta = file.metadata()?;
    let forbidden = if owner_only { 0o077 } else { 0o022 };
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.nlink() != 1
        || meta.mode() & forbidden != 0
    {
        return Err(Error::Configuration);
    }
    Ok(file)
}

fn create_file(path: &Path) -> Result<File> {
    Ok(OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?)
}

fn copy(
    mut input: File,
    output: &mut impl Write,
    limit: u64,
    deadline: Instant,
) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        check_deadline(deadline)?;
        let count = input.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or(Error::Limit)?;
        if size > limit {
            return Err(Error::Limit);
        }
        hash.update(&buffer[..count]);
        output.write_all(&buffer[..count])?;
    }
    Ok((origin::hex(&hash.finalize()), size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_v1_golden_vector() {
        let golden = include_str!("../tests/fixtures/workspace-git-v1.json").trim();
        let parsed: Receipt = serde_json::from_str(golden).unwrap();
        assert_eq!(parsed.version, 1);
        assert_eq!(parsed.scope.epoch, 7);
        assert_eq!(parsed.bounds.objects, 12);
        assert_eq!(serde_json::to_string(&parsed).unwrap(), golden);
    }
}
