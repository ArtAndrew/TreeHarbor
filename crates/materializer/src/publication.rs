//! Node-owned publication, separate from the source baseline format.
use crate::{Error, Result};
use rustix::fs::{Mode, OFlags, RenameFlags, openat, renameat_with};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};
use treeharbor_core::ObjectId;

pub struct Publication {
    destination: PathBuf,
    binding: [u8; 32],
    _guard: Arc<dyn Send + Sync>,
}
impl Publication {
    /// Paths and guard come only from a trusted local controller. The guard must
    /// own BOTH process exclusion and in-process operation exclusion. It is a
    /// lifetime mechanism, not an authentication token or runtime sandbox.
    pub fn new(destination: PathBuf, binding: [u8; 32], guard: Arc<dyn Send + Sync>) -> Self {
        Self {
            destination,
            binding,
            _guard: guard,
        }
    }
    pub(crate) fn publish(&self, staging: &Path) -> Result<PathBuf> {
        let parent = self.destination.parent().ok_or(Error::Configuration)?;
        let name = self.destination.file_name().ok_or(Error::Configuration)?;
        let target_parent = directory(parent)?;
        let source_parent = directory(staging.parent().ok_or(Error::Configuration)?)?;
        let source = directory(staging)?;
        let mut manifest = read_file(&source, "manifest.json")?;
        if manifest.metadata()?.len() > 256 * 1024 * 1024 {
            return Err(Error::Limit);
        }
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        let mut total = 0u64;
        loop {
            let read = manifest.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total += read as u64;
            if total > 256 * 1024 * 1024 {
                return Err(Error::Limit);
            }
            hash.update(&buffer[..read]);
        }
        let receipt = PublicationReceipt {
            version: 1,
            binding: ObjectId::from_bytes(self.binding).to_string(),
            manifest: ObjectId::from_bytes(hash.finalize().into()).to_string(),
        };
        let mut output = File::from(
            openat(
                &source,
                "publication.json",
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_raw_mode(0o600),
            )
            .map_err(io::Error::from)?,
        );
        serde_json::to_writer(&mut output, &receipt)?;
        output.write_all(b"\n")?;
        output.sync_all()?;
        source.sync_all()?;
        renameat_with(
            &source_parent,
            staging.file_name().ok_or(Error::Configuration)?,
            &target_parent,
            name,
            RenameFlags::NOREPLACE,
        )
        .map_err(io::Error::from)?;
        // A post-rename failure is uncertain publication, never permission to
        // delete/overwrite the destination. Recovery inspects the durable seal.
        source_parent.sync_all()?;
        target_parent.sync_all()?;
        Ok(self.destination.clone())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceipt {
    version: u8,
    binding: String,
    manifest: String,
}
impl PublicationReceipt {
    /// This is a private controller record, not agent-supplied JSON. A receipt
    /// proves no DB completion, current content, disk quota or sandbox execution.
    pub fn load(directory_path: &Path, expected_binding: [u8; 32]) -> Result<Self> {
        let root = directory(directory_path)?;
        let input = read_file(&root, "publication.json")?;
        if input.metadata()?.len() > 1024 {
            return Err(Error::Limit);
        }
        let mut bytes = Vec::new();
        input.take(1025).read_to_end(&mut bytes)?;
        if bytes.len() > 1024 {
            return Err(Error::Limit);
        }
        let receipt: Self = serde_json::from_slice(&bytes)?;
        if receipt.version != 1
            || receipt.binding != ObjectId::from_bytes(expected_binding).to_string()
        {
            return Err(Error::InvalidBaseline);
        }
        receipt.manifest_digest()?;
        // Retry the durability barrier after a previous post-rename fsync error.
        root.sync_all()?;
        directory(directory_path.parent().ok_or(Error::Configuration)?)?.sync_all()?;
        Ok(receipt)
    }
    pub fn manifest_digest(&self) -> Result<[u8; 32]> {
        Ok(*self.manifest.parse::<ObjectId>()?.as_bytes())
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
        return Err(Error::UnsafePath("publication directory"));
    }
    Ok(file)
}
fn read_file(parent: &File, name: &str) -> Result<File> {
    let file = File::from(
        openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(io::Error::from)?,
    );
    let meta = file.metadata()?;
    if !meta.is_file()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
        || meta.nlink() != 1
    {
        return Err(Error::UnsafePath("publication file"));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::PermissionsExt};

    #[test]
    fn atomic_publication_never_replaces_an_existing_empty_or_populated_destination() {
        let root = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let destination = root.path().join("tree");
        let make_stage = || {
            let stage = tempfile::Builder::new()
                .permissions(fs::Permissions::from_mode(0o700))
                .tempdir_in(root.path())
                .unwrap();
            fs::write(
                stage.path().join("manifest.json"),
                b"trusted-writer-fixture",
            )
            .unwrap();
            fs::set_permissions(
                stage.path().join("manifest.json"),
                fs::Permissions::from_mode(0o600),
            )
            .unwrap();
            stage
        };
        let publication = Publication::new(destination.clone(), [1; 32], Arc::new(()));
        fs::create_dir(&destination).unwrap();
        assert!(publication.publish(make_stage().path()).is_err());
        assert_eq!(fs::read_dir(&destination).unwrap().count(), 0);
        fs::remove_dir(&destination).unwrap();
        let stage = make_stage();
        publication.publish(stage.path()).unwrap();
        let receipt = fs::read(destination.join("publication.json")).unwrap();
        assert!(publication.publish(make_stage().path()).is_err());
        assert_eq!(
            fs::read(destination.join("publication.json")).unwrap(),
            receipt
        );
        assert!(PublicationReceipt::load(&destination, [2; 32]).is_err());
        assert!(PublicationReceipt::load(&destination, [1; 32]).is_ok());
    }

    #[test]
    fn publication_receipt_v1_has_fixed_field_names_and_encoding() {
        let receipt = PublicationReceipt {
            version: 1,
            binding: ObjectId::from_bytes([1; 32]).to_string(),
            manifest: ObjectId::from_bytes([2; 32]).to_string(),
        };
        assert_eq!(
            serde_json::to_string(&receipt).unwrap(),
            concat!(
                "{\"version\":1,\"binding\":\"sha256:0101010101010101010101010101010101010101010101010101010101010101\",",
                "\"manifest\":\"sha256:0202020202020202020202020202020202020202020202020202020202020202\"}"
            )
        );
    }
}
