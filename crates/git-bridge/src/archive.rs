//! Verify and retain the original Git bytes for an archived repository.
use crate::{
    BundleInput, Error, HashAlgorithm, ImportedBranches, Limits, Result, bundle, check_deadline,
    check_limits, ingest, origin, process::Git, valid_ref,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    time::Instant,
};
use treeharbor_core::{GitOid, ObjectStore, RevisionRef};

pub(super) const MAX_REFS: usize = crate::MAX_EXPORT_REFS + 10_000;
const MAX_REF_BYTES: usize = 1034;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitObjectKind {
    Commit,
    Tag,
    Tree,
    Blob,
}

impl GitObjectKind {
    fn name(self) -> &'static [u8] {
        match self {
            Self::Commit => b"commit",
            Self::Tag => b"tag",
            Self::Tree => b"tree",
            Self::Blob => b"blob",
        }
    }
}

#[derive(Clone, Copy)]
pub struct ArchiveRef {
    pub oid: GitOid,
    pub kind: GitObjectKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveBundleInfo {
    pub sha256: String,
    pub bytes: u64,
    pub objects: usize,
    pub object_bytes: u64,
}

pub struct ArchiveBundle {
    scratch: tempfile::TempDir,
    git: PathBuf,
    limits: Limits,
    deadline: Instant,
    pub info: ArchiveBundleInfo,
}

impl ArchiveBundle {
    /// The synced, verified private copy. It can be moved out before drop.
    pub fn path(&self) -> PathBuf {
        self.scratch.path().join("archive.bundle")
    }

    /// Import the selected exact commits from this verified private archive.
    /// Names are labels in the result and never installed as Git or native refs.
    pub fn import_commits(
        &mut self,
        store: &impl ObjectStore,
        commits: &BTreeMap<String, GitOid>,
    ) -> Result<ImportedBranches> {
        if commits.is_empty()
            || commits.len() > MAX_REFS
            || commits.iter().any(|(name, id)| {
                !valid_ref(name) || !matches!(id, GitOid::Sha1(bytes) if *bytes != [0; 20])
            })
        {
            return Err(Error::Configuration);
        }
        let scratch = self.scratch.path();
        let git = Git::new(self.git.clone(), scratch, self.deadline);
        ingest::import_verified_commits(
            store,
            &git,
            &scratch.join("source.git"),
            scratch,
            commits,
            self.limits,
            self.deadline,
        )
    }
}

fn oid(bytes: &[u8]) -> Result<GitOid> {
    let bytes = origin::parse_oid(bytes, HashAlgorithm::Sha1)?;
    Ok(GitOid::Sha1(
        bytes.try_into().map_err(|_| Error::InvalidProjection)?,
    ))
}

fn valid_name(name: &str, kind: GitObjectKind) -> bool {
    if name.len() > MAX_REF_BYTES || name.bytes().any(|b| b <= b' ' || b == 0x7f) {
        return false;
    }
    // update-ref independently enforces Git's complete ref syntax.
    if name.starts_with("refs/tags/") {
        return true;
    }
    kind == GitObjectKind::Commit
        && (name.starts_with("refs/heads/")
            || name
                .strip_prefix("refs/pull/")
                .and_then(|name| name.strip_suffix("/head"))
                .and_then(|number| number.parse::<u32>().ok().map(|n| (number, n)))
                .is_some_and(|(number, n)| n > 0 && number == n.to_string()))
}

fn require_complete_bundle(file: &File) -> Result<()> {
    let mut reader = BufReader::new(file);
    match ingest::line(&mut reader)?.as_slice() {
        b"# v2 git bundle" => Ok(()),
        b"# v3 git bundle" => {
            let mut format_seen = false;
            while reader.fill_buf()?.starts_with(b"@") {
                if format_seen || ingest::line(&mut reader)? != b"@object-format=sha1" {
                    return Err(Error::Unsupported("filtered or unsupported archive bundle"));
                }
                format_seen = true;
            }
            Ok(())
        }
        _ => Err(Error::Unsupported("archive bundle version")),
    }
}

fn check_listing(
    listing: File,
    refs: &BTreeMap<String, ArchiveRef>,
    default: GitOid,
) -> Result<()> {
    let mut found = std::collections::BTreeSet::new();
    let mut head_seen = false;
    for line in BufReader::new(listing).lines() {
        let line = line?;
        let (name_oid, name) = line.split_once(' ').ok_or(Error::InvalidProjection)?;
        let advertised = oid(name_oid.as_bytes())?;
        if name == "HEAD" {
            if head_seen || advertised != default {
                return Err(Error::ArchiveInventoryMismatch);
            }
            head_seen = true;
        } else if refs.get(name).map(|r| r.oid) != Some(advertised)
            || !found.insert(name.to_owned())
        {
            return Err(Error::ArchiveInventoryMismatch);
        }
    }
    if found.len() != refs.len() {
        return Err(Error::ArchiveInventoryMismatch);
    }
    Ok(())
}

fn check_objects(
    listing: File,
    mut required: HashMap<GitOid, GitObjectKind>,
    request: &BundleInput<'_>,
    deadline: Instant,
) -> Result<(usize, u64)> {
    let mut reader = BufReader::new(listing);
    let mut objects = 0;
    let mut bytes = 0u64;
    loop {
        check_deadline(deadline)?;
        let line = ingest::line(&mut reader)?;
        if line.is_empty() {
            break;
        }
        let mut fields = line.split(|b| *b == b' ');
        let object = oid(fields.next().ok_or(Error::InvalidProjection)?)?;
        let kind = fields.next().ok_or(Error::InvalidProjection)?;
        let size = std::str::from_utf8(fields.next().ok_or(Error::InvalidProjection)?)
            .map_err(|_| Error::InvalidProjection)?
            .parse::<u64>()
            .map_err(|_| Error::InvalidProjection)?;
        if fields.next().is_some() {
            return Err(Error::InvalidProjection);
        }
        if let Some(expected) = required.remove(&object)
            && kind != expected.name()
        {
            return Err(Error::ArchiveInventoryMismatch);
        }
        objects += 1;
        bytes = bytes.checked_add(size).ok_or(Error::Limit)?;
        if objects > request.limits.objects || bytes > request.limits.source_bytes {
            return Err(Error::Limit);
        }
    }
    if !required.is_empty() {
        return Err(Error::ArchiveInventoryMismatch);
    }
    Ok((objects, bytes))
}

/// Check a complete SHA-1 bundle against the archived host inventory. Install
/// only the expected refs in a disposable bare repository and verify all
/// reachable objects there. Never read a checkout, remote or operator Git config.
pub fn verify_archive_bundle(
    request: BundleInput<'_>,
    refs: &BTreeMap<String, ArchiveRef>,
    default_ref: &str,
    pull_bases: impl IntoIterator<Item = GitOid>,
) -> Result<ArchiveBundle> {
    check_limits(request.limits, request.git)?;
    if refs.len() > MAX_REFS || !default_ref.starts_with("refs/heads/") {
        return Err(Error::Configuration);
    }
    let default = refs
        .get(default_ref)
        .ok_or(Error::ArchiveInventoryMismatch)?;
    let mut required = HashMap::new();
    for (name, tip) in refs {
        if !valid_name(name, tip.kind)
            || !matches!(tip.oid, GitOid::Sha1(id) if id != [0; 20])
            || required
                .insert(tip.oid, tip.kind)
                .is_some_and(|k| k != tip.kind)
        {
            return Err(Error::Configuration);
        }
    }
    for base in pull_bases {
        if !matches!(base, GitOid::Sha1(id) if id != [0; 20])
            || required
                .insert(base, GitObjectKind::Commit)
                .is_some_and(|k| k != GitObjectKind::Commit)
        {
            return Err(Error::ArchiveInventoryMismatch);
        }
    }

    let deadline = Instant::now() + request.limits.timeout;
    let scratch = bundle::scratch(request.scratch, ".treeharbor-verify-archive-")?;
    let saved = scratch.path().join("archive.bundle");
    bundle::copy(
        request.bundle,
        &saved,
        request.limits.stream_bytes,
        deadline,
    )?;
    let file = File::open(&saved)?;
    require_complete_bundle(&file)?;
    let executable = request.git.canonicalize()?;
    let git = Git::new(executable.clone(), scratch.path(), deadline);
    let max_listing = (refs.len() as u64 + 1) * (MAX_REF_BYTES as u64 + 42);
    let (repo, listing) = bundle::unpack(
        &git,
        scratch.path(),
        &saved,
        HashAlgorithm::Sha1,
        max_listing,
    )?;
    check_listing(listing, refs, default.oid)?;
    let listing = git.capture(
        Some(&repo),
        "list archived objects",
        &[
            "cat-file".into(),
            "--batch-check".into(),
            "--batch-all-objects".into(),
        ],
        None,
        (request.limits.objects as u64 + 1) * 80,
    )?;
    let (objects, object_bytes) = check_objects(listing, required, &request, deadline)?;

    let updates = scratch.path().join("archive.refs");
    let mut updates_file = File::create(&updates)?;
    for (name, tip) in refs {
        check_deadline(deadline)?;
        let oid = RevisionRef::Git(tip.oid).to_string();
        let oid = oid.strip_prefix("git:sha1:").ok_or(Error::Configuration)?;
        write!(updates_file, "create {name}\0{oid}\0")?;
    }
    drop(updates_file);
    git.run(
        Some(&repo),
        "restore archived refs",
        &["update-ref".into(), "--stdin".into(), "-z".into()],
        Some(&updates),
        4096,
    )?;
    git.run(
        Some(&repo),
        "restore default branch",
        &["symbolic-ref".into(), "HEAD".into(), default_ref.into()],
        None,
        4096,
    )?;
    let unreachable = git.capture(
        Some(&repo),
        "verify archived objects",
        &[
            "fsck".into(),
            "--strict".into(),
            "--no-reflogs".into(),
            "--no-dangling".into(),
            "--unreachable".into(),
        ],
        None,
        1 << 20,
    )?;
    if unreachable.metadata()?.len() != 0 {
        return Err(Error::ArchiveInventoryMismatch);
    }
    let (sha256, bytes) =
        bundle::digest(File::open(&saved)?, request.limits.stream_bytes, deadline)?;
    file.sync_all()?;
    check_deadline(deadline)?;
    Ok(ArchiveBundle {
        scratch,
        git: executable,
        limits: request.limits,
        deadline,
        info: ArchiveBundleInfo {
            sha256,
            bytes,
            objects,
            object_bytes,
        },
    })
}
