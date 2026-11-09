//! Stage the authorized Git history outside the agent's working files.
use crate::{Error, Prepare, Result, storage::Operation};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::File,
    io,
    os::unix::fs::FileExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use treeharbor_core::{GitOid, Object, ObjectId, ObjectStore, RevisionRef};
use treeharbor_git_bridge::{Limits, WorkspaceSeed, seed_workspace};
use treeharbor_metadata::{
    Metadata,
    exports::SourceExport,
    reads::{MAX_READ_BYTES, MAX_READ_OBJECTS},
};
use treeharbor_storage::ManagedStore;

pub(crate) struct GitSource {
    store: SourceSpool,
    git: PathBuf,
    native: ObjectId,
    origin: GitOid,
    limits: Limits,
}

impl GitSource {
    pub(crate) async fn stage(
        db: &mut Metadata,
        request: &Prepare<'_>,
        operation: Arc<Operation>,
        parent: &Path,
        git: &Path,
    ) -> Result<Self> {
        let Prepare {
            authorization: auth,
            plan,
            store: backend,
            limits,
        } = *request;
        let limits = Limits {
            source_bytes: limits.max_source_bytes.min(limits.max_workspace_bytes),
            stream_bytes: limits.max_workspace_bytes,
            ..Default::default()
        };
        let (&RevisionRef::Native(native), Some(origin)) = (plan.revision(), plan.git_revision())
        else {
            return Err(Error::Identity);
        };
        if !git.is_absolute() || !git.is_file() {
            return Err(Error::Configuration);
        }
        let store = SourceSpool::open(parent)?;
        let read = db.open_git_preparation_source(auth, plan, 600).await?;
        let result = collect(db, &read, backend, store, operation, limits).await;
        let close = db.finish_source_export(read).await;
        let store = result?;
        close?;
        Ok(Self {
            store,
            git: git.to_owned(),
            native,
            origin,
            limits,
        })
    }

    pub(crate) fn seed(self, files: &Path) -> io::Result<()> {
        seed_workspace(
            &self.store,
            WorkspaceSeed {
                git: &self.git,
                files,
                native: self.native,
                expected: self.origin,
                limits: self.limits,
            },
        )
        .map_err(io::Error::other)
    }
}

// The authorized frames are only scratch input to Git projection. A lost
// process rebuilds them from the pinned source, so only the final placement
// needs durable publication. The private spool has no path to reopen.
struct SourceSpool {
    file: File,
    offsets: BTreeMap<ObjectId, (u64, usize)>,
    length: u64,
}

impl SourceSpool {
    fn open(parent: &Path) -> io::Result<Self> {
        Ok(Self {
            file: tempfile::tempfile_in(parent)?,
            offsets: BTreeMap::new(),
            length: 0,
        })
    }

    fn append(&mut self, id: ObjectId, frame: &[u8], limit: u64) -> Result<()> {
        let end = self
            .length
            .checked_add(frame.len() as u64)
            .filter(|end| *end <= limit)
            .ok_or(Error::Configuration)?;
        if ObjectId::hash_frame(frame) != id || self.offsets.contains_key(&id) {
            return Err(Error::Identity);
        }
        self.file.write_all_at(frame, self.length)?;
        self.offsets.insert(id, (self.length, frame.len()));
        self.length = end;
        Ok(())
    }
}

impl ObjectStore for SourceSpool {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let &(offset, length) = self
            .offsets
            .get(&id)
            .ok_or(treeharbor_core::Error::MissingObject(id))?;
        let mut frame = vec![0; length];
        self.file
            .read_exact_at(&mut frame, offset)
            .map_err(|error| treeharbor_core::Error::Storage(error.to_string()))?;
        Object::decode_verified(id, &frame)
    }

    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::Storage(
            "Git source spool is read-only".into(),
        ))
    }
}

async fn collect(
    db: &mut Metadata,
    read: &SourceExport,
    backend: &ManagedStore,
    mut store: SourceSpool,
    operation: Arc<Operation>,
    limits: Limits,
) -> Result<SourceSpool> {
    let root = read.root();
    let mut pending = BTreeMap::from([(root.id(), root)]);
    let mut seen = BTreeSet::new();
    let mut edges = 0usize;
    while !pending.is_empty() {
        let nodes: Vec<_> = pending.values().take(MAX_READ_OBJECTS).cloned().collect();
        let batch = db
            .read_source_export(read, backend, &nodes, MAX_READ_BYTES)
            .await?;
        let guard = operation.clone();
        let (next_store, objects) = tokio::task::spawn_blocking(move || -> Result<_> {
            guard.verify()?;
            let mut next = Vec::with_capacity(batch.len());
            for read in batch.read()? {
                let id = read.id();
                let frame = read.object().encode().map_err(io::Error::other)?;
                store.append(id, &frame, limits.source_bytes)?;
                let children: Vec<_> = read.children().collect();
                next.push((id, children));
            }
            Ok((store, next))
        })
        .await??;
        store = next_store;
        for (id, children) in objects {
            pending.remove(&id);
            seen.insert(id);
            edges = edges
                .checked_add(children.len())
                .filter(|n| *n <= limits.edges)
                .ok_or(Error::Configuration)?;
            for child in children {
                if !seen.contains(&child.id()) {
                    pending.entry(child.id()).or_insert(child);
                }
            }
            if seen.len() + pending.len() > limits.objects {
                return Err(Error::Configuration);
            }
        }
    }
    Ok(store)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn spool_preserves_verified_objects_and_rejects_corruption() {
        let root = tempfile::tempdir().unwrap();
        let mut spool = SourceSpool::open(root.path()).unwrap();
        assert!(root.path().read_dir().unwrap().next().is_none());
        assert_eq!(
            spool.file.metadata().unwrap().permissions().mode() & 0o077,
            0
        );
        let first = Object::Blob(b"first".to_vec());
        let second = Object::Blob(b"second".to_vec());
        let first_id = first.id().unwrap();
        let second_id = second.id().unwrap();
        let first_frame = first.encode().unwrap();
        let second_frame = second.encode().unwrap();
        let limit = (first_frame.len() + second_frame.len()) as u64;
        spool.append(first_id, &first_frame, limit).unwrap();
        assert!(matches!(
            spool.append(second_id, &second_frame, limit - 1),
            Err(Error::Configuration)
        ));
        assert!(matches!(
            spool.get(second_id),
            Err(treeharbor_core::Error::MissingObject(id)) if id == second_id
        ));
        spool.append(second_id, &second_frame, limit).unwrap();
        assert_eq!(spool.get(first_id).unwrap(), first);
        assert_eq!(spool.get(second_id).unwrap(), second);
        spool.file.write_all_at(&[0xff], 0).unwrap();
        assert!(spool.get(first_id).is_err());
        assert_eq!(spool.get(second_id).unwrap(), second);
    }
}
