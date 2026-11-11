//! Store the archive files as a bounded directory of ordinary immutable objects.
use super::{ArchiveObservation, ArchiveObservationReceipt, Upstream};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    io::{Read, Write},
    time::Instant,
};
use treeharbor_core::{
    Directory, Entry, EntryMode, EntryRef, FRAME_HEADER, Name, Object, ObjectId, ObjectStore,
};
use treeharbor_git_bridge::ArchiveBundleInfo;
use treeharbor_github_contract::archive_inventory::ArchivedRepository;

pub const ARCHIVE_ARTIFACT_FORMAT: &str = "treeharbor-github-archive";
const CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_BUNDLE_BYTES: u64 = 8 << 30;
const MAX_MANIFEST_BYTES: usize = treeharbor_core::MAX_PAYLOAD;
const MAX_CHUNKS: usize = (MAX_BUNDLE_BYTES / CHUNK_BYTES as u64) as usize;
const MANIFEST_NAME: &str = "manifest.json";

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveArtifactManifest<B> {
    pub format: String,
    pub version: u8,
    pub receipt: ArchiveObservationReceipt,
    pub inventory: ArchivedRepository<B>,
    pub bundle: ArchiveBundleInfo,
}

impl<B: Serialize> ArchiveArtifactManifest<B> {
    pub fn validate(&self) -> Result<()> {
        let request = &self.receipt.request;
        let bundle = &self.bundle;
        if self.format != ARCHIVE_ARTIFACT_FORMAT
            || self.version != 1
            || self.receipt.version != 1
            || self.receipt.state != "observed"
            || request.version != 1
            || request.binding.is_nil()
            || request.repository.is_nil()
            || request.epoch < 1
            || !canonical_hex(&bundle.sha256)
            || !(1..=MAX_BUNDLE_BYTES).contains(&bundle.bytes)
            || !(1..=100_000).contains(&bundle.objects)
            || bundle.object_bytes > (4 << 30)
            || self.receipt.observation
                != ArchiveObservation::from_inventory(
                    request.binding,
                    &request.key,
                    &self.inventory,
                )?
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveArtifactSpec {
    pub version: u8,
    pub observation_key: String,
    pub root: String,
    pub manifest_sha256: String,
    pub object_count: i64,
    pub encoded_bytes: i64,
}

impl ArchiveArtifactSpec {
    pub(super) fn validate(&self) -> Result<ObjectId> {
        let root = self
            .root
            .parse::<ObjectId>()
            .map_err(|_| Error::Invalid("archive root"))?;
        let fingerprint = self
            .manifest_sha256
            .parse::<ObjectId>()
            .map_err(|_| Error::Invalid("archive fingerprint"))?;
        let max_bytes = MAX_BUNDLE_BYTES + MAX_MANIFEST_BYTES as u64 + (1 << 20);
        let Some(key) = self.observation_key.strip_prefix("archive-observed-") else {
            return Err(Error::Invalid("archive observation key"));
        };
        if self.version != 1
            || !canonical_hex(key)
            || root.to_string() != self.root
            || fingerprint.to_string() != self.manifest_sha256
            || !(3..=(MAX_CHUNKS + 2) as i64).contains(&self.object_count)
            || !(1..=max_bytes as i64).contains(&self.encoded_bytes)
        {
            return Err(Error::Invalid("archive artifact"));
        }
        Ok(root)
    }
}

pub struct ArchiveArtifact {
    pub specification: ArchiveArtifactSpec,
    /// Distinct objects in child-before-parent order.
    pub objects: Vec<ObjectId>,
}

pub fn archive_manifest_fingerprint(bytes: &[u8]) -> ObjectId {
    let mut digest = Sha256::new();
    digest.update(b"treeharbor-github-archive\0v1\0");
    digest.update(bytes);
    ObjectId::from_bytes(digest.finalize().into())
}

fn canonical_hex(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(Error::UploadLimit);
    }
    Ok(())
}

fn manifest(bytes: &[u8]) -> Result<ArchiveArtifactManifest<Upstream>> {
    if bytes.len() > MAX_MANIFEST_BYTES {
        return Err(Error::UploadLimit);
    }
    let manifest: ArchiveArtifactManifest<Upstream> =
        serde_json::from_slice(bytes).map_err(|_| Error::ArchiveArtifactMismatch)?;
    manifest.validate()?;
    Ok(manifest)
}

fn chunk_name(index: usize) -> String {
    format!("bundle-{index:04}")
}

fn chunk_sizes(bytes: u64) -> impl Iterator<Item = usize> {
    (0..bytes.div_ceil(CHUNK_BYTES as u64))
        .map(move |index| (bytes - index * CHUNK_BYTES as u64).min(CHUNK_BYTES as u64) as usize)
}

fn archive_io(error: std::io::Error) -> Error {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        Error::ArchiveArtifactMismatch
    } else {
        Error::Objects(treeharbor_core::Error::Storage(error.to_string()))
    }
}

fn blob_entry(name: &str, id: ObjectId) -> Result<Entry> {
    Ok(Entry {
        name: Name::new(name.as_bytes())?,
        value: EntryRef {
            mode: EntryMode::File,
            target: id,
        },
    })
}

pub fn pack_archive_artifact(
    store: &impl ObjectStore,
    bytes: Vec<u8>,
    mut bundle: impl Read,
    deadline: Instant,
) -> Result<ArchiveArtifact> {
    let manifest = manifest(&bytes)?;
    let manifest_sha256 = archive_manifest_fingerprint(&bytes).to_string();
    let mut entries = Vec::new();
    let mut objects = Vec::new();
    let mut seen = BTreeSet::new();
    let mut encoded_bytes = 0i64;
    let mut add_blob = |name: &str, bytes: Vec<u8>| -> Result<()> {
        let length = bytes.len() + FRAME_HEADER;
        let id = store.put(&Object::Blob(bytes))?;
        if seen.insert(id) {
            objects.push(id);
            encoded_bytes += length as i64;
        }
        entries.push(blob_entry(name, id)?);
        Ok(())
    };
    add_blob(MANIFEST_NAME, bytes)?;
    let mut hash = Sha256::new();
    for (index, length) in chunk_sizes(manifest.bundle.bytes).enumerate() {
        check_deadline(deadline)?;
        let mut bytes = vec![0; length];
        bundle.read_exact(&mut bytes).map_err(archive_io)?;
        hash.update(&bytes);
        add_blob(&chunk_name(index), bytes)?;
    }
    if bundle.read(&mut [0]).map_err(archive_io)? != 0
        || ObjectId::from_bytes(hash.finalize().into()).hex() != manifest.bundle.sha256
    {
        return Err(Error::ArchiveArtifactMismatch);
    }
    check_deadline(deadline)?;
    let root = Object::Directory(Directory::new(entries)?);
    encoded_bytes += root.encode()?.len() as i64;
    let root = store.put(&root)?;
    objects.push(root);
    let specification = ArchiveArtifactSpec {
        version: 1,
        observation_key: manifest.receipt.request.observation_key,
        root: root.to_string(),
        manifest_sha256,
        object_count: objects.len() as i64,
        encoded_bytes,
    };
    specification.validate()?;
    Ok(ArchiveArtifact {
        specification,
        objects,
    })
}

/// `store` resolves only the exact generations for this scoped upload. A
/// caller must publish any output files only after this function succeeds.
pub(super) fn check_archive_artifact(
    store: &impl ObjectStore,
    spec: &ArchiveArtifactSpec,
    expected: &ArchiveObservationReceipt,
    deadline: Instant,
    mut bundle_output: impl Write,
) -> Result<Vec<u8>> {
    check_deadline(deadline)?;
    let root = store.get(spec.validate()?)?;
    let mut encoded_bytes = root.encode()?.len() as i64;
    let Object::Directory(directory) = root else {
        return Err(Error::ArchiveArtifactMismatch);
    };
    let Some((manifest_entry, chunks)) = directory.entries().split_last() else {
        return Err(Error::ArchiveArtifactMismatch);
    };
    let mut seen = BTreeSet::new();
    let mut read_blob = |entry: &Entry, name: &str| -> Result<Vec<u8>> {
        check_deadline(deadline)?;
        if entry.name.as_bytes() != name.as_bytes() || entry.value.mode != EntryMode::File {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let Object::Blob(bytes) = store.get(entry.value.target)? else {
            return Err(Error::ArchiveArtifactMismatch);
        };
        if seen.insert(entry.value.target) {
            encoded_bytes += (FRAME_HEADER + bytes.len()) as i64;
        }
        Ok(bytes)
    };
    let manifest_bytes = read_blob(manifest_entry, MANIFEST_NAME)?;
    let manifest = manifest(&manifest_bytes)?;
    if &manifest.receipt != expected
        || spec.observation_key != expected.request.observation_key
        || archive_manifest_fingerprint(&manifest_bytes).to_string() != spec.manifest_sha256
        || chunks.len() as u64 != manifest.bundle.bytes.div_ceil(CHUNK_BYTES as u64)
    {
        return Err(Error::ArchiveArtifactMismatch);
    }
    let mut hash = Sha256::new();
    for (index, (entry, length)) in chunks
        .iter()
        .zip(chunk_sizes(manifest.bundle.bytes))
        .enumerate()
    {
        let bytes = read_blob(entry, &chunk_name(index))?;
        if bytes.len() != length {
            return Err(Error::ArchiveArtifactMismatch);
        }
        hash.update(&bytes);
        bundle_output.write_all(&bytes).map_err(archive_io)?;
    }
    if ObjectId::from_bytes(hash.finalize().into()).hex() != manifest.bundle.sha256
        || (seen.len() + 1) as i64 != spec.object_count
        || encoded_bytes != spec.encoded_bytes
    {
        return Err(Error::ArchiveArtifactMismatch);
    }
    check_deadline(deadline)?;
    Ok(manifest_bytes)
}
