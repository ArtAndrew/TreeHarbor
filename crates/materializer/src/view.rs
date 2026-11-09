//! Publish only the protected baseline of an already verified mounted view.
use crate::{Error, MaterializedTree, Publication, Result, publication};
use sha2::{Digest, Sha256};
use std::{
    fs::{OpenOptions, Permissions},
    io::{Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

impl MaterializedTree {
    /// The node owns the mount/lease/slot guard, supplies its durable record,
    /// and must re-observe the mounted view before acknowledging readiness.
    /// The ordinary directory opener refuses this representation.
    pub fn publish_view(
        self,
        publication: Publication,
        staging: &Path,
        files: PathBuf,
        expected: [u8; 32],
        record: &[u8],
    ) -> Result<Self> {
        if self.files.is_some()
            || !files.is_absolute()
            || files.starts_with(publication.destination())
        {
            return Err(Error::InvalidBaseline);
        }
        let manifest =
            publication::read_file(&publication::directory(&self.root)?, "manifest.json")?;
        const LIMIT: u64 = 16 * 1024 * 1024;
        let mut bytes = Vec::new();
        manifest.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT || <[u8; 32]>::from(Sha256::digest(&bytes)) != expected {
            return Err(Error::InvalidBaseline);
        }
        publication::directory(staging)?;
        let temp = tempfile::Builder::new()
            .prefix("prepare-")
            .permissions(Permissions::from_mode(0o700))
            .tempdir_in(staging)?;
        for (name, bytes) in [
            ("manifest.json", bytes.as_slice()),
            ("overlay.json", record),
        ] {
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(temp.path().join(name))?;
            output.write_all(bytes)?;
            output.sync_all()?;
        }
        let root = publication.publish(temp.path())?;
        let _ = temp.keep();
        Ok(Self {
            root,
            files: Some(files),
            manifest: self.manifest,
            stats: None,
        })
    }
}
