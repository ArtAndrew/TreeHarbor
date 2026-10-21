//! Trusted node-side native filesystem preparation. Files are private copies;
//! this crate does not start or stop a sandbox, authorize agents or mark READY.
#[cfg(not(unix))]
compile_error!("The native materializer currently supports Unix only");

mod filesystem;
mod publication;
mod recovery;
mod selection;
pub use publication::{Publication, PublicationReceipt};
pub use recovery::BaselineExpectation;
pub use selection::Selection;

use filesystem::{Manifest, Stage};
use std::{
    collections::{BTreeMap, VecDeque},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use treeharbor_core::{EntryMode, Object, ObjectId, RepoPath};
use treeharbor_metadata::{
    Metadata, Scope,
    reads::{MAX_READ_OBJECTS, ReadNode, ReadObject, ReadSession},
};
use treeharbor_storage::ManagedStore;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("filesystem preparation failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("source object contract failed: {0}")]
    Object(#[from] treeharbor_core::Error),
    #[error("metadata read failed: {0}")]
    Metadata(#[from] treeharbor_metadata::Error),
    #[error("preparation worker failed: {0}")]
    Worker(#[from] tokio::task::JoinError),
    #[error("manifest encoding failed: {0}")]
    Manifest(#[from] serde_json::Error),
    #[error("filesystem or source preparation limit exceeded")]
    Limit,
    #[error("unsupported or unsafe filesystem input: {0}")]
    UnsafePath(&'static str),
    #[error("local path prevents sparse expansion: {0:?}")]
    LocalConflict(Vec<u8>),
    #[error("requested sparse path does not exist: {0:?}")]
    MissingPath(Vec<u8>),
    #[error("root pin does not identify the expected snapshot")]
    WrongSnapshot,
    #[error("invalid materializer configuration")]
    Configuration,
    #[error("invalid, changed or incorrectly bound workspace baseline")]
    InvalidBaseline,
}
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_entries: usize,
    pub max_path_bytes: u64,
    pub max_file_bytes: u64,
    pub max_workspace_bytes: u64,
    pub max_source_bytes: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_entries: 1_000_000,
            max_path_bytes: 64 * 1024 * 1024,
            max_file_bytes: treeharbor_core::MAX_PAYLOAD as u64,
            max_workspace_bytes: 64 * 1024 * 1024 * 1024,
            max_source_bytes: 64 * 1024 * 1024 * 1024,
        }
    }
}
impl Limits {
    fn validate(self) -> Result<Self> {
        if self.max_entries == 0
            || self.max_path_bytes == 0
            || self.max_file_bytes == 0
            || self.max_workspace_bytes == 0
            || self.max_source_bytes == 0
        {
            return Err(Error::Configuration);
        }
        Ok(self)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Statistics {
    pub source_objects: u64,
    pub source_encoded_bytes: u64,
    pub source_bytes_written: u64,
    pub local_bytes_copied: u64,
    pub workspace_bytes: u64,
    pub filesystem_entries: usize,
}

/// A prepared filesystem result, not an active workspace or sandbox. The sibling
/// baseline manifest is outside `path()` and must never be writable by agents.
/// No automatic deletion on Drop: only the lifecycle controller may remove a
/// published tree after stopping its writers and retiring the workspace binding.
#[derive(Clone, Debug)]
pub struct MaterializedTree {
    root: PathBuf,
    manifest: Arc<Manifest>,
    stats: Option<Statistics>,
}
impl MaterializedTree {
    pub fn path(&self) -> PathBuf {
        self.root.join("files")
    }
    pub fn manifest_path(&self) -> PathBuf {
        self.root.join("manifest.json")
    }
    pub fn snapshot(&self) -> ObjectId {
        self.manifest.snapshot
    }
    /// Whether materialization copied previous local state. False does not mean
    /// the files have remained unchanged since their initial preparation.
    pub fn preserved_local_state(&self) -> bool {
        self.manifest.preserved_local_state
    }
    /// Preparation counters are unavailable after restart; they are not current
    /// disk usage and must not be reconstructed as fabricated zero measurements.
    pub fn statistics(&self) -> Option<Statistics> {
        self.stats
    }
    pub fn includes(&self, path: &RepoPath) -> bool {
        self.manifest.entries.contains_key(path)
    }
}

/// Share one instance per node to bound concurrent preparations. An IO task owns
/// its permit and staging directory even if its awaiting request is cancelled.
#[derive(Clone)]
pub struct Materializer {
    root: PathBuf,
    slots: Arc<Semaphore>,
}
impl Materializer {
    pub fn open(root: impl AsRef<Path>, concurrent_jobs: usize) -> Result<Self> {
        if !(1..=64).contains(&concurrent_jobs) {
            return Err(Error::Configuration);
        }
        filesystem::private_directory(root.as_ref())?;
        Ok(Self {
            root: root.as_ref().canonicalize()?,
            slots: Arc::new(Semaphore::new(concurrent_jobs)),
        })
    }

    /// Caller has authenticated this repository/root holder and selected a full
    /// tree or a tool-compatible sparse profile. A sparse profile is not an ACL.
    pub async fn prepare(
        &self,
        db: &mut Metadata,
        store: &ManagedStore,
        request: Preparation<'_>,
    ) -> Result<MaterializedTree> {
        self.build(db, store, request, None, None).await
    }

    /// Trusted controller path. The supplied lifetime guard stays alive in all
    /// blocking stages even when the awaiting future is cancelled. Publication
    /// is atomic/no-clobber, carries an exact binding receipt and never replaces
    /// an existing directory. This does not authorize execution or update the DB.
    pub async fn prepare_published(
        &self,
        db: &mut Metadata,
        store: &ManagedStore,
        request: Preparation<'_>,
        publication: Publication,
    ) -> Result<MaterializedTree> {
        self.build(db, store, request, None, Some(publication))
            .await
    }

    /// Internal controller operation. ALL writers to `previous` must already be
    /// stopped/frozen by the runtime. This call does not prove or perform that
    /// stop, and must never be exposed directly as an agent tool. Produces a new
    /// generation candidate; never edits or switches the previous directory.
    pub async fn expand_stopped(
        &self,
        db: &mut Metadata,
        store: &ManagedStore,
        request: Preparation<'_>,
        previous: &MaterializedTree,
    ) -> Result<MaterializedTree> {
        if previous.manifest.snapshot != request.snapshot
            || previous.manifest.tenant != request.scope.tenant()
            || previous.manifest.repository != request.scope.repository().to_string()
        {
            return Err(Error::WrongSnapshot);
        }
        self.build(db, store, request, Some(previous.clone()), None)
            .await
    }

    async fn build(
        &self,
        db: &mut Metadata,
        store: &ManagedStore,
        request: Preparation<'_>,
        previous: Option<MaterializedTree>,
        publication: Option<Publication>,
    ) -> Result<MaterializedTree> {
        let limits = request.limits.validate()?;
        let selection = match &previous {
            Some(p) => p.manifest.selection.union(&request.selection)?,
            None => request.selection,
        };
        let permit = self
            .slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| Error::Configuration)?;
        let parent = self.root.join(request.scope.tenant());
        let snapshot = request.snapshot;
        let tenant = request.scope.tenant().to_owned();
        let repository = request.scope.repository().to_string();
        let stage = tokio::task::spawn_blocking(move || {
            let mut stage = Stage::new(
                parent, tenant, repository, snapshot, selection, limits, permit, previous,
            )?;
            stage.publication = publication;
            Ok::<_, Error>(stage)
        })
        .await??;
        let session = db
            .open_read_session(request.scope, request.holder, 60)
            .await?;
        let result = build_source(db, store, &session, stage).await;
        let close = db.close_read_session(session).await;
        let stage = result?;
        close?;
        if stage.publication.is_some() {
            // A controller-owned publisher retains its lifetime guard and has a
            // deterministic recovery path, including a cancelled awaiting task.
            tokio::task::spawn_blocking(move || stage.publish()).await?
        } else {
            // Legacy unregistered results have no recovery identity; never leave
            // an unobserved background publisher behind on this path.
            stage.publish()
        }
    }
}

pub struct Preparation<'a> {
    pub scope: &'a Scope,
    pub holder: &'a str,
    pub snapshot: ObjectId,
    pub selection: Selection,
    pub limits: Limits,
}

#[derive(Clone)]
struct Task {
    path: Vec<u8>,
    node: ReadNode,
    mode: EntryMode,
}

async fn read_one(
    db: &mut Metadata,
    store: &ManagedStore,
    session: &ReadSession,
    node: ReadNode,
    mut stage: Stage,
) -> Result<(Stage, ReadObject)> {
    let batch = db
        .open_read_batch(session, store, &[node], stage.read_budget()?)
        .await?;
    tokio::task::spawn_blocking(move || {
        let read = batch.read()?.remove(0);
        stage.account_source(&read)?;
        Ok((stage, read))
    })
    .await?
}

async fn build_source(
    db: &mut Metadata,
    store: &ManagedStore,
    session: &ReadSession,
    mut stage: Stage,
) -> Result<Stage> {
    let mut renewed = Instant::now();
    let (next_stage, mut source) = read_one(db, store, session, session.root(), stage).await?;
    stage = next_stage;
    if let Object::NativeCommit(commit) = source.object() {
        let node = source
            .children()
            .find(|n| n.id() == commit.snapshot)
            .ok_or(Error::WrongSnapshot)?;
        (stage, source) = read_one(db, store, session, node, stage).await?;
    }
    let Object::Snapshot { root } = source.object() else {
        return Err(Error::WrongSnapshot);
    };
    if source.id() != stage.manifest.snapshot {
        return Err(Error::WrongSnapshot);
    }
    let node = source
        .children()
        .find(|n| n.id() == *root)
        .ok_or(Error::WrongSnapshot)?;
    let mut pending = VecDeque::from([Task {
        path: vec![],
        node,
        mode: EntryMode::Directory,
    }]);
    while !pending.is_empty() {
        if renewed.elapsed() >= Duration::from_secs(20) {
            db.renew_read_session(session, 60).await?;
            renewed = Instant::now();
        }
        let mut groups: BTreeMap<ObjectId, Vec<Task>> = BTreeMap::new();
        while groups.len() < MAX_READ_OBJECTS {
            let Some(task) = pending.pop_front() else {
                break;
            };
            groups.entry(task.node.id()).or_default().push(task);
        }
        let mut nodes: VecDeque<_> = groups.values().map(|v| v[0].node.clone()).collect();
        while !nodes.is_empty() {
            if renewed.elapsed() >= Duration::from_secs(20) {
                db.renew_read_session(session, 60).await?;
                renewed = Instant::now();
            }
            let frontier: Vec<_> = nodes.iter().cloned().collect();
            let batch = db
                .open_read_prefix(session, store, &frontier, stage.read_budget()?)
                .await?;
            for _ in 0..batch.len() {
                nodes.pop_front();
            }
            let work: BTreeMap<_, _> = frontier
                .iter()
                .take(batch.len())
                .map(|node| {
                    (
                        node.id(),
                        groups.remove(&node.id()).expect("frontier group"),
                    )
                })
                .collect();
            let (next_stage, added) = tokio::task::spawn_blocking(move || -> Result<_> {
                let mut added = Vec::new();
                for read in batch.read()? {
                    stage.account_source(&read)?;
                    let children: BTreeMap<_, _> = read.children().map(|n| (n.id(), n)).collect();
                    for task in &work[&read.id()] {
                        match read.object() {
                            Object::Directory(directory) if task.mode == EntryMode::Directory => {
                                stage.source_directory(&task.path)?;
                                for entry in directory.entries() {
                                    let mut path = task.path.clone();
                                    if !path.is_empty() {
                                        path.push(b'/');
                                    }
                                    path.extend_from_slice(entry.name.as_bytes());
                                    let path = RepoPath::new(path)?;
                                    if !stage.manifest.selection.relevant(&path) {
                                        continue;
                                    }
                                    stage.record(path.clone(), entry.value)?;
                                    if entry.value.mode != EntryMode::Directory
                                        && stage.preserve_leaf(&path)?
                                    {
                                        continue;
                                    }
                                    added.push(Task {
                                        path: path.as_bytes().to_vec(),
                                        node: children
                                            .get(&entry.value.target)
                                            .ok_or(Error::WrongSnapshot)?
                                            .clone(),
                                        mode: entry.value.mode,
                                    });
                                }
                            }
                            Object::Blob(bytes) if task.mode != EntryMode::Directory => {
                                stage.write_source(&task.path, task.mode, bytes)?
                            }
                            _ => return Err(Error::WrongSnapshot),
                        }
                    }
                }
                Ok((stage, added))
            })
            .await??;
            stage = next_stage;
            pending.extend(added);
        }
    }
    tokio::task::spawn_blocking(move || {
        stage.finish()?;
        Ok(stage)
    })
    .await?
}
