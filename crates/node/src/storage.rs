use crate::{Error, Result, private};
use rustix::fs::{FlockOperation, OFlags, flock};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Weak},
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    version: u8,
    tenant: String,
    node: String,
    storage: Uuid,
    device: u64,
    inode: u64,
}
impl Identity {
    pub fn storage(&self) -> Uuid {
        self.storage
    }
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn node(&self) -> &str {
        &self.node
    }
}
pub(crate) struct Storage {
    pub root: PathBuf,
    pub identity: Identity,
    lock: File,
}
impl Storage {
    pub fn verify(&self) -> Result<()> {
        private::directory(&self.root)?;
        private::directory(&self.root.join("slots"))?;
        let meta = fs::symlink_metadata(&self.root)?;
        let current: Identity =
            serde_json::from_slice(&private::read(&self.root.join("identity.json"), 2048)?)?;
        let lock_path = fs::symlink_metadata(self.root.join("node.lock"))?;
        let lock = self.lock.metadata()?;
        private::file(&self.lock)?;
        if current != self.identity
            || meta.dev() != current.device
            || meta.ino() != current.inode
            || lock_path.dev() != lock.dev()
            || lock_path.ino() != lock.ino()
            || !lock_path.is_file()
        {
            return Err(Error::Identity);
        }
        Ok(())
    }
}
pub(crate) struct Operation {
    pub storage: Arc<Storage>,
    _slot: OwnedMutexGuard<()>,
    _capacity: OwnedSemaphorePermit,
}
#[derive(Clone)]
pub struct Node {
    pub(crate) storage: Arc<Storage>,
    capacity: Arc<Semaphore>,
    slots: Arc<Mutex<BTreeMap<Uuid, Weak<AsyncMutex<()>>>>>,
}
impl Node {
    /// Initialize a NEW path only. Failed initialization never adopts existing
    /// files or invents a replacement identity. The parent must be private.
    pub fn create(path: &Path, tenant: &str, node: &str, concurrent: usize) -> Result<Self> {
        validate(tenant, node, concurrent)?;
        private::directory(path.parent().ok_or(Error::Private)?)?;
        fs::DirBuilder::new().mode(0o700).create(path)?;
        let path = private::directory(path)?;
        let meta = fs::symlink_metadata(&path)?;
        let identity = Identity {
            version: 1,
            tenant: tenant.into(),
            node: node.into(),
            storage: Uuid::new_v4(),
            device: meta.dev(),
            inode: meta.ino(),
        };
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(path.join("node.lock"))?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        lock.sync_all()?;
        private::make_directory(&path.join("slots"))?;
        private::write_new(&path.join("identity.json"), &serde_json::to_vec(&identity)?)?;
        File::open(path.parent().ok_or(Error::Private)?)?.sync_all()?;
        Ok(Self::from_storage(
            Storage {
                root: path,
                identity,
                lock,
            },
            concurrent,
        ))
    }
    /// Open only this exact provisioned identity and physical root. No missing
    /// file, changed inode or storage UUID is repaired by silently reinitializing.
    pub fn open(
        path: &Path,
        tenant: &str,
        node: &str,
        storage: Uuid,
        concurrent: usize,
    ) -> Result<Self> {
        validate(tenant, node, concurrent)?;
        let path = private::directory(path)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags((OFlags::NOFOLLOW | OFlags::NONBLOCK).bits() as i32)
            .open(path.join("node.lock"))?;
        private::file(&lock)?;
        flock(&lock, FlockOperation::NonBlockingLockExclusive).map_err(|_| Error::Busy)?;
        let identity: Identity =
            serde_json::from_slice(&private::read(&path.join("identity.json"), 2048)?)?;
        if identity.version != 1
            || identity.tenant != tenant
            || identity.node != node
            || identity.storage != storage
            || storage.is_nil()
        {
            return Err(Error::Identity);
        }
        let owned = Storage {
            root: path,
            identity,
            lock,
        };
        owned.verify()?;
        private::directory(&owned.root.join("slots"))?;
        Ok(Self::from_storage(owned, concurrent))
    }
    fn from_storage(storage: Storage, concurrent: usize) -> Self {
        Self {
            storage: Arc::new(storage),
            capacity: Arc::new(Semaphore::new(concurrent)),
            slots: Arc::new(Mutex::new(BTreeMap::new())),
        }
    }
    pub fn identity(&self) -> &Identity {
        &self.storage.identity
    }
    pub(crate) async fn operation(&self, slot: Uuid) -> Result<Arc<Operation>> {
        let lock = {
            let mut slots = self.slots.lock().map_err(|_| Error::Busy)?;
            slots.retain(|_, lock| lock.strong_count() != 0);
            if let Some(lock) = slots.get(&slot).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(AsyncMutex::new(()));
                slots.insert(slot, Arc::downgrade(&lock));
                lock
            }
        };
        let slot = lock.lock_owned().await;
        let capacity = self
            .capacity
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Busy)?;
        self.storage.verify()?;
        Ok(Arc::new(Operation {
            storage: self.storage.clone(),
            _slot: slot,
            _capacity: capacity,
        }))
    }
}
fn validate(tenant: &str, node: &str, concurrent: usize) -> Result<()> {
    if treeharbor_storage::TenantId::new(tenant).is_err()
        || node.is_empty()
        || node.len() > 128
        || !node
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
        || !(1..=64).contains(&concurrent)
    {
        return Err(Error::Configuration);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, time::Duration};

    #[tokio::test]
    async fn cancelled_waiter_keeps_storage_and_slot_locked_until_actual_io_finishes() {
        let parent = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let root = parent.path().join("node");
        let node = Node::create(&root, "tenant", "node-1", 2).unwrap();
        let storage = node.identity().storage();
        let slot = Uuid::new_v4();
        let guard = node.operation(slot).await.unwrap();
        let (started, entered) = tokio::sync::oneshot::channel();
        let (release, blocked) = std::sync::mpsc::channel();
        let weak = Arc::downgrade(&guard);
        let waiter = tokio::spawn(async move {
            tokio::task::spawn_blocking(move || {
                let _guard = guard;
                started.send(()).unwrap();
                blocked.recv().unwrap();
            })
            .await
            .unwrap();
        });
        entered.await.unwrap();
        waiter.abort();
        assert!(waiter.await.unwrap_err().is_cancelled());
        assert!(
            tokio::time::timeout(Duration::from_millis(30), node.operation(slot))
                .await
                .is_err()
        );
        // A different slot can run, but a second process cannot acquire this root.
        drop(node.operation(Uuid::new_v4()).await.unwrap());
        drop(node);
        assert!(Node::open(&root, "tenant", "node-1", storage, 2).is_err());
        release.send(()).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while weak.upgrade().is_some() {
            assert!(tokio::time::Instant::now() < deadline);
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        drop(Node::open(&root, "tenant", "node-1", storage, 2).unwrap());
    }
}
