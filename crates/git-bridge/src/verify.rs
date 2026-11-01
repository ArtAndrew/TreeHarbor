//! Verify exact native/Git identity without invoking Git or accepting a claimed
//! mapping. This proof is deliberately neither constructible nor deserializable.
use crate::{Error, HashAlgorithm, Limits, Result, check_source_limits, stream};
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
    check_source_limits(limits)?;
    let parent = crate::private_parent(&scratch.join("mapping"))?;
    let temp = tempfile::Builder::new()
        .prefix(".treeharbor-mapping-")
        .permissions(std::fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)?;
    let algorithm = match expected {
        GitOid::Sha1(_) => HashAlgorithm::Sha1,
        GitOid::Sha256(_) => HashAlgorithm::Sha256,
    };
    let graph = stream::write(
        store,
        &BTreeMap::from([("refs/heads/recovery".into(), native)]),
        &temp.path().join("projection.pack"),
        algorithm,
        limits,
        Instant::now() + limits.timeout,
    )?;
    let oid = graph.commits.get(&native).ok_or(Error::InvalidProjection)?;
    if format!("git:{}:{oid}", algorithm.name()) != RevisionRef::Git(expected).to_string() {
        return Err(Error::InvalidProjection);
    }
    Ok(VerifiedMapping {
        native,
        git: expected,
        source_objects: graph.objects,
        source_bytes: graph.bytes,
    })
}
