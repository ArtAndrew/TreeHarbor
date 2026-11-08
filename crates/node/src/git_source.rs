//! Stage the authorized Git history outside the agent's working files.
use crate::{Error, Prepare, Result, storage::Operation};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::Permissions,
    io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use treeharbor_core::{GitOid, ObjectId, ObjectStore, RevisionRef};
use treeharbor_git_bridge::{Limits, WorkspaceSeed, seed_workspace};
use treeharbor_metadata::{
    Metadata,
    exports::SourceExport,
    reads::{MAX_READ_BYTES, MAX_READ_OBJECTS},
};
use treeharbor_storage::{LocalStore, ManagedStore, TenantId};

pub(crate) struct GitSource {
    _staging: Arc<tempfile::TempDir>,
    store: LocalStore,
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
        let staging = Arc::new(
            tempfile::Builder::new()
                .prefix("prepare-git-")
                .permissions(Permissions::from_mode(0o700))
                .tempdir_in(parent)?,
        );
        let tenant = TenantId::new(plan.scope().tenant()).map_err(|_| Error::Identity)?;
        let store = LocalStore::open(staging.path(), &tenant).map_err(io::Error::other)?;
        let read = db.open_git_preparation_source(auth, plan, 600).await?;
        let result = collect(db, &read, backend, &store, operation, &staging, limits).await;
        let close = db.finish_source_export(read).await;
        result?;
        close?;
        Ok(Self {
            _staging: staging,
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

async fn collect(
    db: &mut Metadata,
    read: &SourceExport,
    backend: &ManagedStore,
    store: &LocalStore,
    operation: Arc<Operation>,
    staging: &Arc<tempfile::TempDir>,
    limits: Limits,
) -> Result<()> {
    let root = read.root();
    let mut pending = BTreeMap::from([(root.id(), root)]);
    let mut seen = BTreeSet::new();
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while !pending.is_empty() {
        let nodes: Vec<_> = pending.values().take(MAX_READ_OBJECTS).cloned().collect();
        let batch = db
            .read_source_export(read, backend, &nodes, MAX_READ_BYTES)
            .await?;
        let store = store.clone();
        let guard = operation.clone();
        let staging = staging.clone();
        let (objects, total) = tokio::task::spawn_blocking(move || -> Result<_> {
            let _staging = staging;
            guard.verify()?;
            let mut total = bytes;
            let mut next = Vec::with_capacity(batch.len());
            for read in batch.read()? {
                let id = read.id();
                let object = read.object();
                let length = object.encode().map_err(io::Error::other)?.len() as u64;
                total = total
                    .checked_add(length)
                    .filter(|n| *n <= limits.source_bytes)
                    .ok_or(Error::Configuration)?;
                let children: Vec<_> = read.children().collect();
                if store.put(object).map_err(io::Error::other)? != id {
                    return Err(Error::Identity);
                }
                next.push((id, children));
            }
            Ok((next, total))
        })
        .await??;
        bytes = total;
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
    Ok(())
}
