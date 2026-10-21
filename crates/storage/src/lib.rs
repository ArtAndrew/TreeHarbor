//! Local immutable objects. Durable metadata publication and GC live in the
//! metadata layer; storing a blob alone never acknowledges an agent change.

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use treeharbor_core::{Error, FRAME_HEADER, MAX_PAYLOAD, Object, ObjectId, ObjectStore, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TenantId(String);
impl TenantId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 64
            || !value
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(Error::InvalidObject("invalid tenant identifier"));
        }
        Ok(Self(value))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The root must belong to the service and must never be writable or mounted
/// inside agent runtimes. Filesystem identity isolation is an external boundary.
#[derive(Clone, Debug)]
pub struct LocalStore {
    objects: PathBuf,
    staging: PathBuf,
}

impl LocalStore {
    pub fn open(root: impl AsRef<Path>, tenant: &TenantId) -> Result<Self> {
        let root = root.as_ref();
        ensure_private_dir(root)?;
        let root = root.canonicalize().map_err(io_error)?;
        let objects = root.join("objects");
        ensure_private_dir(&objects)?;
        let objects = objects.join(tenant.as_str());
        ensure_private_dir(&objects)?;
        let staging = root.join("staging");
        ensure_private_dir(&staging)?;
        let staging = staging.join(tenant.as_str());
        ensure_private_dir(&staging)?;
        Ok(Self { objects, staging })
    }

    fn path(&self, id: ObjectId) -> PathBuf {
        let hex = id.hex();
        self.objects.join(&hex[..2]).join(&hex[2..])
    }

    pub fn contains(&self, id: ObjectId) -> Result<bool> {
        match self.get(id) {
            Ok(_) => Ok(true),
            Err(Error::MissingObject(_)) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Orphan staging files are intentionally retained on a hard crash. A
    /// future GC worker may remove them only after consulting upload leases.
    pub fn staging_directory(&self) -> &Path {
        &self.staging
    }
}

impl ObjectStore for LocalStore {
    fn get(&self, id: ObjectId) -> Result<Object> {
        let path = self.path(id);
        let metadata = fs::symlink_metadata(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                Error::MissingObject(id)
            } else {
                io_error(e)
            }
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::Storage("object is not a regular file".into()));
        }
        if metadata.len() > (FRAME_HEADER + MAX_PAYLOAD) as u64 {
            return Err(Error::TooLarge);
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        File::open(path)
            .map_err(io_error)?
            .take((FRAME_HEADER + MAX_PAYLOAD + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        Object::decode_verified(id, &bytes)
    }

    fn put(&self, object: &Object) -> Result<ObjectId> {
        let bytes = object.encode()?;
        let id = ObjectId::hash_frame(&bytes);
        let path = self.path(id);
        let parent = path
            .parent()
            .ok_or(Error::Storage("object parent missing".into()))?;
        ensure_private_dir(parent)?;
        let mut tmp = tempfile::NamedTempFile::new_in(&self.staging).map_err(io_error)?;
        tmp.write_all(&bytes).map_err(io_error)?;
        tmp.as_file().sync_all().map_err(io_error)?;
        match tmp.persist_noclobber(&path) {
            Ok(file) => file.sync_all().map_err(io_error)?,
            Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
                // Never repair corruption by overwriting an immutable address.
                if self.get(id)? != *object {
                    return Err(Error::InvalidObject("existing object differs"));
                }
            }
            Err(error) => return Err(io_error(error.error)),
        }
        sync_directory(parent)?;
        Ok(id)
    }
}

fn io_error(error: std::io::Error) -> Error {
    Error::Storage(error.to_string())
}
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .map_err(io_error)?
        .sync_all()
        .map_err(io_error)
}
fn ensure_private_dir(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => {
            sync_directory(path)?;
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                sync_directory(parent)?;
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::symlink_metadata(path).map_err(io_error)?;
            if !metadata.is_dir() || metadata.file_type().is_symlink() {
                return Err(Error::Storage(
                    "storage directory is not a private directory".into(),
                ));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    return Err(Error::Storage(
                        "storage directory must have mode 0700".into(),
                    ));
                }
            }
        }
        Err(e) => return Err(io_error(e)),
    }
    Ok(())
}
