use crate::{LocalStore, TenantId, ensure_private_dir, io_error, sync_directory};
use std::{
    fs,
    path::{Path, PathBuf},
};
use treeharbor_core::{Error, Object, ObjectId, ObjectStore, Result};

/// Encode once before reservation/upload. The frame and its descriptors cannot
/// be independently mutated, so publication metadata is derived from the bytes.
pub struct PreparedObject {
    bytes: Vec<u8>,
    id: ObjectId,
    kind: u8,
    references: Vec<(ObjectId, u8)>,
}
impl PreparedObject {
    pub fn new(object: &Object) -> Result<Self> {
        let bytes = object.encode()?;
        Ok(Self {
            id: ObjectId::hash_frame(&bytes),
            kind: object.kind(),
            references: object.references(),
            bytes,
        })
    }
    pub fn id(&self) -> ObjectId {
        self.id
    }
    pub fn kind(&self) -> u8 {
        self.kind
    }
    pub fn length(&self) -> usize {
        self.bytes.len()
    }
}

/// Minted only after local no-clobber write and fsync. This proves an upload to
/// this configured backend, not a metadata pin or accepted agent ChangeSet.
#[derive(Debug)]
pub struct DurableObject {
    tenant: String,
    backend: String,
    generation: i64,
    id: ObjectId,
    kind: u8,
    length: i64,
    references: Vec<(ObjectId, u8)>,
}
impl DurableObject {
    pub fn tenant(&self) -> &str {
        &self.tenant
    }
    pub fn backend(&self) -> &str {
        &self.backend
    }
    pub fn generation(&self) -> i64 {
        self.generation
    }
    pub fn id(&self) -> ObjectId {
        self.id
    }
    pub fn kind(&self) -> u8 {
        self.kind
    }
    pub fn length(&self) -> i64 {
        self.length
    }
    pub fn references(&self) -> &[(ObjectId, u8)] {
        &self.references
    }
}

/// A configured service-owned backend. Backend labels must be unique and stable
/// in a deployment; do not remap an existing label to a different storage root.
/// Agents must never receive filesystem access to this directory.
#[derive(Clone, Debug)]
pub struct ManagedStore {
    root: PathBuf,
    tenant: TenantId,
    backend: String,
}
impl ManagedStore {
    pub fn open(root: impl AsRef<Path>, tenant: TenantId, backend: &str) -> Result<Self> {
        TenantId::new(backend)?;
        ensure_private_dir(root.as_ref())?;
        let root = root
            .as_ref()
            .canonicalize()
            .map_err(io_error)?
            .join("managed-v1");
        ensure_private_dir(&root)?;
        Ok(Self {
            root,
            tenant,
            backend: backend.into(),
        })
    }
    pub fn tenant(&self) -> &str {
        self.tenant.as_str()
    }
    pub fn backend(&self) -> &str {
        &self.backend
    }
    fn directory(&self, generation: i64) -> Result<PathBuf> {
        if generation <= 0 {
            return Err(Error::InvalidObject("storage generation must be positive"));
        }
        Ok(self.root.join(generation.to_string()))
    }
    pub fn put(&self, generation: i64, object: &PreparedObject) -> Result<DurableObject> {
        let store = LocalStore::open(self.directory(generation)?, &self.tenant)?;
        store.write_verified_frame(object.id, &object.bytes)?;
        Ok(DurableObject {
            tenant: self.tenant().into(),
            backend: self.backend.clone(),
            generation,
            id: object.id,
            kind: object.kind,
            length: object.length() as i64,
            references: object.references.clone(),
        })
    }
    pub fn get(&self, generation: i64, id: ObjectId) -> Result<Object> {
        let directory = self.directory(generation)?;
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(Error::MissingObject(id));
            }
            Err(error) => return Err(io_error(error)),
            Ok(_) => (),
        }
        LocalStore::open(directory, &self.tenant)?.get(id)
    }
    /// Internal maintenance primitive. The catalog orchestrator must commit
    /// RETIRING before invoking it. Never expose it as an agent API.
    /// The generation-qualified location prevents an old GC retry deleting a
    /// newer upload with the same logical digest.
    pub fn remove_retired_generation(&self, generation: i64, id: ObjectId) -> Result<()> {
        let directory = self.directory(generation)?;
        match fs::symlink_metadata(&directory) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
            Ok(_) => (),
        }
        let store = LocalStore::open(directory, &self.tenant)?;
        let path = store.path(id);
        match fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() || meta.file_type().is_symlink() => {
                return Err(Error::Storage("refusing nonregular GC target".into()));
            }
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io_error(error)),
        }
        match fs::remove_file(&path) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(io_error(error)),
        }
        sync_directory(
            path.parent()
                .ok_or(Error::InvalidObject("object directory"))?,
        )
    }
}
