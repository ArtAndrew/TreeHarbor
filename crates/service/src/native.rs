//! Native candidate computation over scoped source leases. Computation and object
//! fsync happen outside authority transactions; the final receipt is durable.
use crate::{
    database::Database,
    proposals::Scratch,
    source::{Backend, Failure},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;
use treeharbor_core::{EntryMode, EntryRef, Object, ObjectId, ObjectStore};
use treeharbor_merge::{
    CandidateOutcome, ConflictKind, Limits, PreparedCandidate, prepare_candidate,
};
use treeharbor_metadata::{
    Metadata, Scope,
    access::{Authorization, Permission, SecretToken},
    integration::{CandidateRead, Command},
    reads::ReadNode,
    rebases::RebaseRead,
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
type Permit = Arc<OwnedSemaphorePermit>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn meta(e: treeharbor_metadata::Error) -> Failure {
    use treeharbor_metadata::Error;
    match e {
        Error::IntegrationState => fail(409, "integration_state_changed"),
        Error::StaleCandidate => fail(409, "stale_candidate"),
        Error::AdmissionBlocked => fail(409, "admission_blocked"),
        e => crate::source_read::metadata(e),
    }
}
fn core(e: treeharbor_core::Error) -> Failure {
    match e {
        treeharbor_core::Error::TooLarge => fail(413, "native_candidate_limit"),
        _ => fail(500, "native_candidate_objects"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    key: String,
    epoch: i64,
    candidate: String,
}
fn uuid(s: &str) -> Result<Uuid> {
    let id: Uuid = s.parse().map_err(|_| fail(400, "invalid_request"))?;
    if id.to_string() != s {
        return Err(fail(400, "invalid_request"));
    }
    Ok(id)
}
fn parse(bytes: &[u8]) -> Result<(Scope, Command, Uuid)> {
    let w: Wire = serde_json::from_slice(bytes).map_err(|_| fail(400, "invalid_request"))?;
    if w.version != 1 || w.epoch <= 0 {
        return Err(fail(400, "invalid_request"));
    }
    Ok((
        Scope::new(w.tenant, uuid(&w.repository)?).map_err(|_| fail(400, "invalid_request"))?,
        Command {
            key: w.key,
            epoch: w.epoch,
        },
        uuid(&w.candidate)?,
    ))
}
enum InputRead<'a> {
    Candidate(&'a CandidateRead),
    Rebase(&'a RebaseRead),
}
impl InputRead<'_> {
    fn roots(&self) -> Vec<ReadNode> {
        match self {
            Self::Candidate(r) => r.roots(),
            Self::Rebase(r) => r.roots(),
        }
    }
    fn input(&self) -> &treeharbor_merge::CandidateInput {
        match self {
            Self::Candidate(r) => r.input(),
            Self::Rebase(r) => r.input(),
        }
    }
}
pub(crate) async fn rebase_source(
    db: &mut Metadata,
    store: &ManagedStore,
    read: &RebaseRead,
    permit: Permit,
) -> Result<Scratch> {
    Loader::new(db, store, InputRead::Rebase(read), permit)
        .load()
        .await
}
struct Loader<'a> {
    db: &'a mut Metadata,
    store: &'a ManagedStore,
    read: InputRead<'a>,
    permit: Permit,
    scratch: Scratch,
    known: BTreeMap<ObjectId, (usize, ReadNode)>,
    loaded: BTreeSet<ObjectId>,
    entries: usize,
    path_bytes: usize,
}
impl<'a> Loader<'a> {
    fn new(
        db: &'a mut Metadata,
        store: &'a ManagedStore,
        read: InputRead<'a>,
        permit: Permit,
    ) -> Self {
        let known = read
            .roots()
            .into_iter()
            .enumerate()
            .map(|(source, node)| (node.id(), (source, node)))
            .collect();
        Self {
            db,
            store,
            read,
            permit,
            scratch: Scratch::new(),
            known,
            loaded: BTreeSet::new(),
            entries: 0,
            path_bytes: 0,
        }
    }
    async fn get(&mut self, id: ObjectId) -> Result<Object> {
        if self.loaded.contains(&id) {
            return self.scratch.get(id).map_err(core);
        }
        let (source, node) = self
            .known
            .get(&id)
            .ok_or(fail(500, "native_candidate_graph"))?
            .clone();
        let batch = match &self.read {
            InputRead::Candidate(read) => {
                self.db
                    .read_native_candidate(read, source, self.store, &[node], 128 * 1024 * 1024)
                    .await
            }
            InputRead::Rebase(read) => {
                self.db
                    .read_change_rebase(read, source, self.store, &[node], 128 * 1024 * 1024)
                    .await
            }
        }
        .map_err(meta)?;
        let permit = self.permit.clone();
        let object = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            batch.read()
        })
        .await
        .map_err(|_| fail(500, "native_worker_failed"))?
        .map_err(meta)?
        .pop()
        .ok_or(fail(500, "native_candidate_graph"))?;
        for child in object.children() {
            if self.known.len() >= 200_016 && !self.known.contains_key(&child.id()) {
                return Err(fail(413, "native_candidate_limit"));
            }
            self.known.entry(child.id()).or_insert((source, child));
        }
        self.scratch.load(object).map_err(core)?;
        self.loaded.insert(id);
        self.scratch.get(id).map_err(core)
    }
    async fn tree(&mut self, snapshot: ObjectId) -> Result<BTreeMap<Vec<u8>, EntryRef>> {
        let Object::Snapshot { root } = self.get(snapshot).await? else {
            return Err(fail(500, "native_candidate_graph"));
        };
        let mut pending = vec![(Vec::new(), root)];
        let mut leaves = BTreeMap::new();
        while let Some((prefix, id)) = pending.pop() {
            let Object::Directory(dir) = self.get(id).await? else {
                return Err(fail(500, "native_candidate_graph"));
            };
            for e in dir.entries() {
                let mut path = prefix.clone();
                if !path.is_empty() {
                    path.push(b'/');
                }
                path.extend_from_slice(e.name.as_bytes());
                self.entries += 1;
                self.path_bytes += path.len();
                if self.entries > 200_000 || self.path_bytes > 32 * 1024 * 1024 || path.len() > 4096
                {
                    return Err(fail(413, "native_candidate_limit"));
                }
                if e.value.mode == EntryMode::Directory {
                    pending.push((path, e.value.target));
                } else {
                    leaves.insert(path, e.value);
                }
            }
        }
        Ok(leaves)
    }
    async fn load(mut self) -> Result<Scratch> {
        let i = self.read.input().clone();
        self.get(i.base_revision).await?;
        self.get(i.current_revision).await?;
        let Object::ChangeOperations(ops) = self.get(i.original_operations).await? else {
            return Err(fail(500, "native_candidate_graph"));
        };
        let base = self.tree(i.base_snapshot).await?;
        let current = self.tree(i.current_snapshot).await?;
        let proposed = self.tree(i.proposed_snapshot).await?;
        let paths: BTreeSet<_> = base
            .keys()
            .chain(current.keys())
            .chain(proposed.keys())
            .collect();
        let mut blobs = BTreeSet::new();
        for path in paths {
            let a = base.get(path);
            let b = current.get(path);
            let c = proposed.get(path);
            if a != b || b != c {
                blobs.extend([a, b, c].into_iter().flatten().map(|e| e.target));
            }
        }
        // Reapplying the submitted operations verifies replacement blob kinds,
        // including identical replacements omitted by the three-tree comparison.
        blobs.extend(
            ops.operations()
                .iter()
                .filter_map(|o| o.replacement.map(|r| r.target)),
        );
        for id in blobs {
            self.get(id).await?;
        }
        Ok(self.scratch)
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(crate) fn conflicts(items: &[treeharbor_merge::Conflict]) -> Value {
    // Bounded response, lossless paths; the count tells clients when to inspect
    // remaining conflicts locally. Content bytes are never placed in an error.
    let mut rows = Vec::new();
    let mut size = 0;
    for c in items.iter().take(16) {
        let kind = match c.kind {
            ConflictKind::Content => "content",
            ConflictKind::Binary => "binary",
            ConflictKind::Symlink => "symlink",
            ConflictKind::Type => "type",
            ConflictKind::Mode => "mode",
            ConflictKind::ModifyDelete => "modify_delete",
            ConflictKind::RenameDelete => "rename_delete",
            ConflictKind::RenameRename => "rename_rename",
            ConflictKind::DestinationCollision => "destination_collision",
            ConflictKind::FileDirectory => "file_directory",
        };
        let row = json!({"path_hex":hex(c.path.as_bytes()),"related_path_hex":c.related_path.as_ref().map(|p|hex(p.as_bytes())),"kind":kind});
        size += row.to_string().len();
        if size > 30_000 {
            break;
        }
        rows.push(row);
    }
    json!({"count":items.len(),"truncated":rows.len()<items.len(),"conflicts":rows})
}
enum Computed {
    Ready(Box<PreparedCandidate>, Vec<PreparedObject>),
    Rejected(&'static str, bool, Value),
}
#[allow(clippy::too_many_arguments)]
async fn execute(
    db: &mut Metadata,
    auth: &Authorization,
    cmd: &Command,
    id: Uuid,
    read: &CandidateRead,
    store: &ManagedStore,
    permit: Permit,
) -> Result<Value> {
    let scratch = Loader::new(db, store, InputRead::Candidate(read), permit.clone())
        .load()
        .await?;
    let input = read.input().clone();
    let hold = permit.clone();
    let computed = tokio::task::spawn_blocking(move || {
        let _permit = hold;
        match prepare_candidate(&scratch, &input, Limits::default()) {
            Ok(CandidateOutcome::Ready(c)) => {
                let objects = scratch.publications(&[c.revision()]).map_err(core)?;
                Ok(Computed::Ready(c, objects))
            }
            Ok(CandidateOutcome::Conflicted(c)) => {
                Ok(Computed::Rejected("merge_conflict", true, conflicts(&c)))
            }
            Err(treeharbor_merge::Error::Unsupported(_)) => {
                Ok(Computed::Rejected("merge_unsupported", false, json!({})))
            }
            Err(
                treeharbor_merge::Error::Limit(_)
                | treeharbor_merge::Error::Core(treeharbor_core::Error::TooLarge),
            ) => Ok(Computed::Rejected("merge_limit", false, json!({}))),
            Err(_) => Err(fail(500, "native_candidate_objects")),
        }
    })
    .await
    .map_err(|_| fail(500, "native_worker_failed"))??;
    let (computed, objects) = match computed {
        Computed::Rejected(reason, conflict, details) => {
            return db
                .finish_native_preparation_failure(auth, cmd, id, reason, conflict, details)
                .await
                .map_err(meta);
        }
        Computed::Ready(c, objects) => (c, objects),
    };
    let mut root = None;
    for object in objects {
        let lease = db
            .reserve_native_candidate_object(auth, id, store, &object)
            .await
            .map_err(meta)?;
        if lease.requires_upload() {
            let store = store.clone();
            let generation = lease.key().generation();
            let hold = permit.clone();
            let durable = tokio::task::spawn_blocking(move || {
                let _permit = hold;
                store.put(generation, &object)
            })
            .await
            .map_err(|_| fail(500, "native_worker_failed"))?
            .map_err(core)?;
            db.publish_object(&lease, &durable).await.map_err(meta)?;
        }
        if lease.key().id() == computed.revision() {
            root = Some(lease);
        }
    }
    let root = root.ok_or(fail(500, "native_candidate_objects"))?;
    let result = db
        .finish_native_candidate(auth, cmd, id, &computed, &root)
        .await
        .map_err(meta);
    if result.is_ok() {
        let _ = db.release_object_hold(&root).await;
    }
    result
}
pub(crate) async fn prepare(
    bytes: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
) -> Result<Value> {
    let (scope, cmd, id) = parse(bytes)?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &scope, Permission::IntegrationControl)
        .await
        .map_err(meta)?;
    if let Some(value) = db
        .metadata
        .native_preparation_replay(&auth, &cmd, id)
        .await
        .map_err(meta)?
    {
        return Ok(json!({"version":1,"result":value}));
    }
    let permit = Arc::new(
        backend
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(503, "native_candidate_busy"))?,
    );
    let read = db
        .metadata
        .open_native_candidate(&auth, id)
        .await
        .map_err(meta)?;
    let result = async {
        let tenant = TenantId::new(scope.tenant()).map_err(core)?;
        let config = backend.config;
        let hold = permit.clone();
        let store = tokio::task::spawn_blocking(move || {
            let _permit = hold;
            ManagedStore::open(config.root, tenant, &config.backend)
        })
        .await
        .map_err(|_| fail(500, "native_worker_failed"))?
        .map_err(core)?;
        execute(&mut db.metadata, &auth, &cmd, id, &read, &store, permit).await
    }
    .await;
    let _ = db.metadata.close_native_candidate_read(read).await;
    result.map(|v| json!({"version":1,"result":v}))
}
