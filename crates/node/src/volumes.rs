use crate::{Error, Node, Result, private};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};
use treeharbor_runtime::{Volume, VolumeIdentity};
use uuid::Uuid;

/// Operator-controlled, immutable mapping of one slot to a bounded filesystem.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VolumeRegistration {
    version: u8,
    storage: Uuid,
    slot: Uuid,
    volume: VolumeIdentity,
}

impl VolumeRegistration {
    pub fn storage(&self) -> Uuid {
        self.storage
    }
    pub fn slot(&self) -> Uuid {
        self.slot
    }
    pub fn limit_bytes(&self) -> u64 {
        self.volume.limit_bytes
    }
    pub fn identity(&self) -> &VolumeIdentity {
        &self.volume
    }
    pub fn digest(&self) -> Result<[u8; 32]> {
        let mut h = Sha256::new();
        h.update(b"treeharbor-node-volume\0v1\0");
        h.update(serde_json::to_vec(self)?);
        Ok(h.finalize().into())
    }
}
impl Node {
    /// Register an explicitly supplied, initially empty private placement on an
    /// independently bounded filesystem. Never provision mounts or adopt data.
    pub fn register_volume(
        &self,
        slot: Uuid,
        mount: &Path,
        placement: &Path,
        limit: u64,
    ) -> Result<VolumeRegistration> {
        let _registration = self.volume_registration.lock().map_err(|_| Error::Busy)?;
        let _slot = self
            .slot_lock(slot)?
            .try_lock_owned()
            .map_err(|_| Error::Busy)?;
        self.storage.verify()?;
        if slot.is_nil() || !(64 * 1024 * 1024..=64 * 1024 * 1024 * 1024).contains(&limit) {
            return Err(Error::Configuration);
        }
        let volume = Volume::open(mount, placement, limit)?;
        if volume.identity().device == fs::metadata(&self.storage.root)?.dev()
            || self.storage.root.starts_with(&volume.identity().mount)
            || self
                .storage
                .root
                .join("slots")
                .join(slot.to_string())
                .try_exists()?
        {
            return Err(Error::Identity);
        }
        let registration = VolumeRegistration {
            version: 1,
            storage: self.identity().storage(),
            slot,
            volume: volume.identity().clone(),
        };
        let root = private::make_directory(&self.storage.root.join("volumes"))?;
        let path = root.join(format!("{slot}.json"));
        let bytes = serde_json::to_vec(&registration)?;
        if bytes.len() > 16 * 1024 {
            return Err(Error::Configuration);
        }
        if path.try_exists()? {
            if private::read(&path, 16 * 1024)? != bytes {
                return Err(Error::Identity);
            }
        } else {
            if fs::read_dir(placement)?.next().is_some()
                || volume
                    .identity()
                    .mount
                    .join("treeharbor-execution.json")
                    .try_exists()?
            {
                return Err(Error::Incomplete);
            }
            // A mount cannot belong to two slots, even if their placement paths differ.
            for entry in fs::read_dir(&root)? {
                let entry = entry?;
                if entry
                    .file_name()
                    .as_encoded_bytes()
                    .starts_with(b"record-write-")
                {
                    continue;
                }
                let other: VolumeRegistration =
                    serde_json::from_slice(&private::read(&entry.path(), 16 * 1024)?)?;
                if other.volume.device == volume.identity().device {
                    return Err(Error::Identity);
                }
            }
            private::write_new(&path, &bytes)?;
        }
        self.storage.verify()?;
        volume.verify()?;
        Ok(registration)
    }
    pub fn registered_volume(&self, slot: Uuid) -> Result<VolumeRegistration> {
        self.storage.verify()?;
        let bytes = private::read(
            &self
                .storage
                .root
                .join("volumes")
                .join(format!("{slot}.json")),
            16 * 1024,
        )?;
        let record: VolumeRegistration = serde_json::from_slice(&bytes)?;
        if record.version != 1
            || record.storage != self.identity().storage()
            || record.slot != slot
            || slot.is_nil()
            || serde_json::to_vec(&record)? != bytes
        {
            return Err(Error::Identity);
        }
        Ok(record)
    }
    pub(crate) fn placement(&self, slot: Uuid) -> Result<(PathBuf, Option<Volume>)> {
        let record_path = self
            .storage
            .root
            .join("volumes")
            .join(format!("{slot}.json"));
        match fs::symlink_metadata(&record_path) {
            Ok(_) => {
                let record = self.registered_volume(slot)?;
                let volume = Volume::open(
                    &record.volume.mount,
                    &record.volume.files,
                    record.volume.limit_bytes,
                )?;
                if volume.identity() != &record.volume {
                    return Err(Error::Identity);
                }
                Ok((record.volume.files, Some(volume)))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok((self.storage.root.join("slots").join(slot.to_string()), None))
            }
            Err(e) => Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_volume_registration_vector() {
        let registration = VolumeRegistration {
            version: 1,
            storage: Uuid::from_u128(2),
            slot: Uuid::from_u128(3),
            volume: VolumeIdentity {
                mount: "/srv/volume".into(),
                files: "/srv/volume/placement".into(),
                device: 1234,
                mount_inode: 2,
                files_inode: 14,
                mount_id: 31,
                capacity_bytes: 120000000,
                limit_bytes: 134217728,
            },
        };
        assert_eq!(
            serde_json::to_vec(&registration).unwrap(),
            include_bytes!("../tests/fixtures/volume-v1.json")
        );
        assert_eq!(
            treeharbor_core::ObjectId::from_bytes(registration.digest().unwrap()).to_string(),
            format!(
                "sha256:{}",
                include_str!("../tests/fixtures/volume-v1.sha256").trim()
            )
        );
    }
}
