//! Optional, operator-owned filesystem for a shared prepared source image.
use crate::{Error, Node, Result, private, storage::Storage};
use serde::{Deserialize, Serialize};
use std::{fs, os::unix::fs::MetadataExt, path::Path, sync::Arc};
use treeharbor_runtime::{Volume, VolumeIdentity};
use uuid::Uuid;

const RECORD: &str = "cow-source-volume.json";
pub(crate) const OWNER: &str = ".treeharbor-source-volume.json";
const LIMIT: usize = 16 * 1024;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Registration {
    version: u8,
    storage: Uuid,
    volume: VolumeIdentity,
}

fn record(storage: &Storage) -> Result<Option<(Registration, Vec<u8>)>> {
    let path = storage.root.join(RECORD);
    match fs::symlink_metadata(&path) {
        Ok(_) => (),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    let bytes = private::read(&path, LIMIT)?;
    let registration: Registration = serde_json::from_slice(&bytes)?;
    if registration.version != 1
        || registration.storage != storage.identity.storage()
        || serde_json::to_vec(&registration)? != bytes
    {
        return Err(Error::Identity);
    }
    Ok(Some((registration, bytes)))
}

fn persist(path: &Path, bytes: &[u8]) -> Result<()> {
    if path.try_exists()? {
        if private::read(path, LIMIT)? != bytes {
            return Err(Error::Identity);
        }
        // Complete a previous publication whose parent-directory sync failed.
        fs::File::open(path.parent().ok_or(Error::Private)?)?.sync_all()?;
    } else {
        private::write_new(path, bytes)?;
    }
    Ok(())
}

impl Node {
    /// Attach an empty, independent ext4 source filesystem before this node's
    /// first overlay preparation. Never mount, move or replace an existing image.
    pub fn register_cow_source_volume(&self, mount: &Path, limit: u64) -> Result<()> {
        let _registration = self.volume_registration.lock().map_err(|_| Error::Busy)?;
        let _image = self.storage.cow_image.try_lock().map_err(|_| Error::Busy)?;
        self.storage.verify()?;
        if !(64 * 1024 * 1024..=1024 * 1024 * 1024).contains(&limit) {
            return Err(Error::Configuration);
        }
        let volume = Volume::open(mount, &mount.join("source"), limit)?;
        let identity = volume.identity();
        private::directory(&identity.files)?;
        if identity.device == fs::metadata(&self.storage.root)?.dev()
            || self.storage.root.starts_with(&identity.mount)
            || identity.mount.starts_with(&self.storage.root)
        {
            return Err(Error::Identity);
        }
        let bytes = serde_json::to_vec(&Registration {
            version: 1,
            storage: self.identity().storage(),
            volume: identity.clone(),
        })?;
        if bytes.len() > LIMIT {
            return Err(Error::Configuration);
        }
        if let Some((_, previous)) = record(&self.storage)? {
            if previous != bytes {
                return Err(Error::Identity);
            }
        } else {
            // A source cannot be relocated underneath already bound views.
            if crate::cow::directory(&self.storage)?.try_exists()?
                || fs::read_dir(&identity.files)?.next().is_some()
            {
                return Err(Error::Incomplete);
            }
            for entry in fs::read_dir(&identity.mount)? {
                let entry = entry?;
                if !["source", "lost+found", OWNER]
                    .iter()
                    .any(|name| entry.file_name() == *name)
                {
                    return Err(Error::Identity);
                }
            }
        }
        // The mount claim precedes the node mapping; retry may finish that
        // mapping, but another storage identity may never adopt the volume.
        persist(&identity.mount.join(OWNER), &bytes)?;
        persist(&self.storage.root.join(RECORD), &bytes)?;
        volume.verify()?;
        self.storage.verify()
    }
}

pub(crate) fn open(storage: &Storage) -> Result<Option<Arc<Volume>>> {
    let mut held = storage.cow_volume.lock().map_err(|_| Error::Busy)?;
    let Some((registration, bytes)) = record(storage)? else {
        if held.is_some() {
            return Err(Error::Identity);
        }
        return Ok(None);
    };
    if held.is_none() {
        *held = Some(Arc::new(Volume::open(
            &registration.volume.mount,
            &registration.volume.files,
            registration.volume.limit_bytes,
        )?));
    }
    let volume = held.as_ref().ok_or(Error::Identity)?;
    volume.verify()?;
    if volume.identity() != &registration.volume
        || private::read(&registration.volume.mount.join(OWNER), LIMIT)? != bytes
    {
        return Err(Error::Identity);
    }
    Ok(Some(volume.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_matches_independent_vector() {
        let record = Registration {
            version: 1,
            storage: Uuid::from_u128(2),
            volume: VolumeIdentity {
                mount: "/srv/lower".into(),
                files: "/srv/lower/source".into(),
                device: 1234,
                mount_inode: 2,
                files_inode: 14,
                mount_id: 31,
                capacity_bytes: 120000000,
                limit_bytes: 134217728,
            },
        };
        assert_eq!(
            serde_json::to_vec(&record).unwrap(),
            include_bytes!("../tests/fixtures/cow-volume-v1.json")
        );
    }

    #[test]
    #[ignore = "requires an empty disposable ext4 TH_TEST_COW_SOURCE_VOLUME"]
    fn dedicated_source_claim_fences_old_slots_other_nodes_and_changed_records() {
        use std::os::unix::fs::PermissionsExt;
        let mount = std::path::PathBuf::from(
            std::env::var_os("TH_TEST_COW_SOURCE_VOLUME").expect("dedicated test mount"),
        );
        let parent = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let old = Node::create(&parent.path().join("old"), "tenant", "old", 1).unwrap();
        let slot = Uuid::new_v4();
        old.register_volume(slot, &mount, &mount.join("source"), 64 * 1024 * 1024)
            .unwrap();
        let path = parent.path().join("source-node");
        let node = Node::create(&path, "tenant", "source-node", 1).unwrap();
        let storage = node.identity().storage();
        // Arbitrary host directories never enable filesystem-wide sync.
        assert!(
            node.register_cow_source_volume(parent.path(), 64 * 1024 * 1024)
                .is_err()
        );
        node.register_cow_source_volume(&mount, 64 * 1024 * 1024)
            .unwrap();
        node.register_cow_source_volume(&mount, 64 * 1024 * 1024)
            .unwrap();
        let marker = mount.join(OWNER);
        let original = fs::read(&marker).unwrap();
        assert!(
            node.register_cow_source_volume(&mount, 128 * 1024 * 1024)
                .is_err()
        );
        assert_eq!(fs::read(&marker).unwrap(), original);
        assert!(old.placement(slot).is_err());
        assert!(
            old.register_cow_source_volume(&mount, 64 * 1024 * 1024)
                .is_err()
        );
        assert!(
            old.register_volume(
                Uuid::new_v4(),
                &mount,
                &mount.join("source"),
                64 * 1024 * 1024
            )
            .is_err()
        );
        let volume = open(&node.storage).unwrap().unwrap();
        let retained = mount.join("source/retained");
        fs::write(&retained, b"original image").unwrap();
        fs::write(&marker, b"{}").unwrap();
        assert!(open(&node.storage).is_err());
        assert_eq!(fs::read(&retained).unwrap(), b"original image");
        fs::write(&marker, original).unwrap();
        volume.verify().unwrap();
        drop(volume);
        drop(node);
        let reopened = Node::open(&path, "tenant", "source-node", storage, 1).unwrap();
        reopened
            .register_cow_source_volume(&mount, 64 * 1024 * 1024)
            .unwrap();
        let volume = open(&reopened.storage).unwrap().unwrap();
        assert_eq!(volume.files(), mount.join("source"));
        assert_eq!(fs::read(retained).unwrap(), b"original image");
        // The fixture's caller unmounts and removes its disposable backing disk.
    }
}
