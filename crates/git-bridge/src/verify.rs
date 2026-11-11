//! Verify exact native/Git identity without invoking Git or accepting a claimed
//! mapping. This proof is deliberately neither constructible nor deserializable.
use crate::{Error, HashAlgorithm, Limits, Result, check_branch_refs, check_source_limits, stream};
use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path, time::Instant};
use treeharbor_core::{GitOid, ObjectId, ObjectStore, RevisionRef};

#[derive(Debug)]
pub struct VerifiedMapping {
    native: ObjectId,
    git: GitOid,
    source_objects: usize,
    source_bytes: u64,
}
impl VerifiedMapping {
    pub fn native(&self) -> ObjectId {
        self.native
    }
    pub fn git(&self) -> GitOid {
        self.git
    }
    pub fn source_objects(&self) -> usize {
        self.source_objects
    }
    pub fn source_bytes(&self) -> u64 {
        self.source_bytes
    }
}

/// One proof of every named tip and of the complete shared native closure.
/// Like `VerifiedMapping`, this value is produced only by the projector.
#[derive(Debug)]
pub struct VerifiedBranchMappings {
    tips: BTreeMap<String, (ObjectId, GitOid)>,
    source_objects: usize,
    source_bytes: u64,
}
impl VerifiedBranchMappings {
    pub fn tips(&self) -> &BTreeMap<String, (ObjectId, GitOid)> {
        &self.tips
    }
    pub fn source_objects(&self) -> usize {
        self.source_objects
    }
    pub fn source_bytes(&self) -> u64 {
        self.source_bytes
    }
}

fn algorithm(oid: GitOid) -> HashAlgorithm {
    match oid {
        GitOid::Sha1(_) => HashAlgorithm::Sha1,
        GitOid::Sha256(_) => HashAlgorithm::Sha256,
    }
}

/// The caller must retain the complete source graph throughout this call. Scratch
/// must be a private bounded-volume directory; no output artifacts are retained.
/// All typed edges and parent history are checked using the existing projector.
/// This proves content identity, not current host ownership or signature validity.
pub fn verify_mapping(
    store: &impl ObjectStore,
    native: ObjectId,
    expected: GitOid,
    scratch: &Path,
    limits: Limits,
) -> Result<VerifiedMapping> {
    let verified = verify_branch_mappings(
        store,
        BTreeMap::from([("refs/heads/recovery".into(), (native, expected))]),
        scratch,
        limits,
    )?;
    Ok(VerifiedMapping {
        native,
        git: expected,
        source_objects: verified.source_objects(),
        source_bytes: verified.source_bytes(),
    })
}

/// Reconstruct every expected Git tip in one bounded Rust projection. Distinct
/// branch names may share a tip. All expected revisions must use the same Git
/// object format. The caller must retain the complete native graph.
pub fn verify_branch_mappings(
    store: &impl ObjectStore,
    tips: BTreeMap<String, (ObjectId, GitOid)>,
    scratch: &Path,
    limits: Limits,
) -> Result<VerifiedBranchMappings> {
    check_branch_refs(&tips)?;
    check_source_limits(limits)?;
    let format = algorithm(tips.values().next().ok_or(Error::Configuration)?.1);
    if tips.values().any(|(_, git)| algorithm(*git) != format) {
        return Err(Error::Configuration);
    }
    let parent = crate::private_parent(&scratch.join("mapping"))?;
    let temp = tempfile::Builder::new()
        .prefix(".treeharbor-mapping-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    let graph = stream::write(
        store,
        &native_refs(&tips),
        &temp.path().join("projection.pack"),
        format,
        limits,
        Instant::now() + limits.timeout,
    )?;
    verified(tips, format, graph)
}

/// Reconstruct the archived pull commit IDs without a Git process or a pack
/// file. Labels are internal branch-shaped names, never published refs.
pub fn verify_archive_commit_mappings(
    store: &impl ObjectStore,
    tips: BTreeMap<String, (ObjectId, GitOid)>,
    limits: Limits,
) -> Result<VerifiedBranchMappings> {
    check_source_limits(limits)?;
    if tips.is_empty()
        || tips.len() > crate::archive::MAX_REFS
        || tips.iter().any(|(name, (_, oid))| {
            !crate::valid_ref(name) || !matches!(oid, GitOid::Sha1(bytes) if *bytes != [0; 20])
        })
    {
        return Err(Error::Configuration);
    }
    let format = HashAlgorithm::Sha1;
    let graph = stream::project(
        store,
        &native_refs(&tips),
        format,
        limits,
        Instant::now() + limits.timeout,
        |kind, raw| {
            if kind == "blob" {
                crate::ingest::check_import_blob(raw)?;
            }
            crate::origin::hash(kind, raw, format)
        },
    )?;
    verified(tips, format, graph)
}

fn native_refs(tips: &BTreeMap<String, (ObjectId, GitOid)>) -> BTreeMap<String, ObjectId> {
    tips.iter()
        .map(|(name, (native, _))| (name.clone(), *native))
        .collect()
}

fn verified(
    tips: BTreeMap<String, (ObjectId, GitOid)>,
    format: HashAlgorithm,
    graph: stream::Graph,
) -> Result<VerifiedBranchMappings> {
    for (native, expected) in tips.values() {
        let oid = graph.commits.get(native).ok_or(Error::InvalidProjection)?;
        if format!("git:{}:{oid}", format.name()) != RevisionRef::Git(*expected).to_string() {
            return Err(Error::InvalidProjection);
        }
    }
    Ok(VerifiedBranchMappings {
        tips,
        source_objects: graph.objects,
        source_bytes: graph.bytes,
    })
}
