//! Computes immutable file operations against protected server source. Memory is
//! bounded computation scratch only: accepted bytes are fsynced in ManagedStore,
//! then versions, pins, completion and outbox commit in PostgreSQL.
use crate::{
    database::Database,
    source::{Backend, Failure},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;
use treeharbor_core::{
    AppliedChange, ChangeOperation, Changes, EntryMode, EntryRef, Object, ObjectId, ObjectStore,
    RepoPath, prepare_change,
};
use treeharbor_metadata::{
    Authority, Metadata, Scope,
    access::{Authorization, Permission, SecretToken},
    catalog::ObjectLease,
    proposals::{ProposalCommand, ProposalRead, ProposalStart},
    reads::{ReadBatch, ReadNode, ReadObject},
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
type Permit = Arc<OwnedSemaphorePermit>;
const MAX_SCRATCH: usize = 128 * 1024 * 1024;
const MAX_OBJECTS: usize = 4096;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn meta(error: treeharbor_metadata::Error) -> Failure {
    crate::source_read::metadata(error)
}
fn core(error: treeharbor_core::Error) -> Failure {
    match error {
        treeharbor_core::Error::PredicateMismatch => fail(409, "change_base_mismatch"),
        treeharbor_core::Error::TooLarge => fail(413, "proposal_limit"),
        treeharbor_core::Error::InvalidPath(_) | treeharbor_core::Error::OverlappingPaths => {
            fail(400, "invalid_change")
        }
        _ => fail(500, "proposal_object_error"),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    key: String,
    authority: String,
    epoch: i64,
    upload: String,
    changeset: String,
    previous_version: i64,
    operations: Vec<Operation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Operation {
    path_hex: String,
    expected: Option<Entry>,
    replacement: Option<Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    mode: u32,
    object: String,
}
impl Entry {
    fn parse(self) -> Result<EntryRef> {
        Ok(EntryRef {
            mode: EntryMode::try_from(self.mode).map_err(|_| fail(400, "invalid_change"))?,
            target: self
                .object
                .parse()
                .map_err(|_| fail(400, "invalid_change"))?,
        })
    }
}
fn uuid(raw: &str) -> Result<Uuid> {
    let id: Uuid = raw.parse().map_err(|_| fail(400, "invalid_request"))?;
    if id.to_string() != raw {
        return Err(fail(400, "invalid_request"));
    }
    Ok(id)
}
fn parse(bytes: &[u8]) -> Result<(Scope, ProposalCommand)> {
    let wire: Wire = serde_json::from_slice(bytes).map_err(|_| fail(400, "invalid_request"))?;
    if wire.version != 1
        || wire.epoch <= 0
        || wire.previous_version < 0
        || wire.operations.len() > 256
    {
        return Err(fail(400, "invalid_request"));
    }
    let operations = wire
        .operations
        .into_iter()
        .map(|o| {
            Ok(ChangeOperation {
                path: RepoPath::new(crate::source_read::decode_hex(&o.path_hex, 4096)?)
                    .map_err(|_| fail(400, "invalid_change"))?,
                expected: o.expected.map(Entry::parse).transpose()?,
                replacement: o.replacement.map(Entry::parse).transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((
        Scope::new(wire.tenant, uuid(&wire.repository)?)
            .map_err(|_| fail(400, "invalid_request"))?,
        ProposalCommand {
            key: wire.key,
            authority: match wire.authority.as_str() {
                "standalone" => Authority::Standalone,
                "connected" => Authority::Connected,
                _ => return Err(fail(400, "invalid_request")),
            },
            epoch: wire.epoch,
            upload: uuid(&wire.upload)?,
            changeset: uuid(&wire.changeset)?,
            previous_version: wire.previous_version,
            operations: Changes::new(operations).map_err(|_| fail(400, "invalid_change"))?,
        },
    ))
}
struct Inner {
    objects: BTreeMap<ObjectId, Object>,
    written: Vec<ObjectId>,
    bytes: usize,
}
pub(crate) struct Scratch(RefCell<Inner>);
impl Scratch {
    pub(crate) fn new() -> Self {
        Self(RefCell::new(Inner {
            objects: BTreeMap::new(),
            written: vec![],
            bytes: 0,
        }))
    }
    pub(crate) fn load(&self, read: ReadObject) -> treeharbor_core::Result<()> {
        let mut inner = self.0.borrow_mut();
        if !inner.objects.contains_key(&read.id()) {
            if inner.objects.len() >= MAX_OBJECTS
                || read.encoded_length() > (MAX_SCRATCH - inner.bytes) as u64
            {
                return Err(treeharbor_core::Error::TooLarge);
            }
            inner.bytes += read.encoded_length() as usize;
            inner.objects.insert(read.id(), read.into_object());
        }
        Ok(())
    }
    fn add(&self, object: Object, generated: bool) -> treeharbor_core::Result<ObjectId> {
        let bytes = object.encode()?;
        let id = ObjectId::hash_frame(&bytes);
        let mut inner = self.0.borrow_mut();
        if !inner.objects.contains_key(&id) {
            if inner.objects.len() >= MAX_OBJECTS || bytes.len() > MAX_SCRATCH - inner.bytes {
                return Err(treeharbor_core::Error::TooLarge);
            }
            inner.bytes += bytes.len();
            inner.objects.insert(id, object);
            if generated {
                inner.written.push(id);
            }
        }
        Ok(id)
    }
    pub(crate) fn publications(
        self,
        roots: &[ObjectId],
    ) -> treeharbor_core::Result<Vec<PreparedObject>> {
        let mut inner = self.0.into_inner();
        // Discard intermediate trees that do not belong to either final root.
        let generated: BTreeSet<_> = inner.written.iter().copied().collect();
        let mut reachable = BTreeSet::new();
        let mut pending = roots.to_vec();
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            if let Some(object) = inner.objects.get(&id) {
                pending.extend(
                    object
                        .references()
                        .into_iter()
                        .filter_map(|(id, _)| generated.contains(&id).then_some(id)),
                );
            }
        }
        let mut order: Vec<_> = inner
            .written
            .into_iter()
            .filter(|id| reachable.contains(id))
            .collect();
        for &root in roots {
            if !order.contains(&root) {
                order.push(root);
            }
        }
        order
            .into_iter()
            .map(|id| {
                PreparedObject::new(
                    &inner
                        .objects
                        .remove(&id)
                        .ok_or(treeharbor_core::Error::MissingObject(id))?,
                )
            })
            .collect()
    }
}
impl ObjectStore for Scratch {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        self.0
            .borrow()
            .objects
            .get(&id)
            .cloned()
            .ok_or(treeharbor_core::Error::MissingObject(id))
    }
    fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
        self.add(object.clone(), true)
    }
}
async fn verify(batch: ReadBatch, permit: Permit) -> Result<ReadObject> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        batch.read()
    })
    .await
    .map_err(|_| fail(500, "proposal_worker_failed"))?
    .map_err(meta)?
    .pop()
    .ok_or(fail(500, "proposal_object_error"))
}
async fn source(
    db: &mut Metadata,
    store: &ManagedStore,
    read: &ProposalRead,
    node: ReadNode,
    permit: Permit,
) -> Result<ReadObject> {
    let batch = db
        .open_proposal_source_batch(
            read,
            store,
            &[node],
            treeharbor_metadata::reads::MAX_READ_BYTES,
        )
        .await
        .map_err(meta)?;
    verify(batch, permit).await
}
fn child(object: &ReadObject, id: ObjectId) -> Result<ReadNode> {
    object
        .children()
        .find(|n| n.id() == id)
        .ok_or(fail(500, "proposal_object_error"))
}

async fn compute(
    db: &mut Metadata,
    store: &ManagedStore,
    read: &ProposalRead,
    command: &ProposalCommand,
    permit: Permit,
) -> Result<(AppliedChange, Vec<PreparedObject>)> {
    let scratch = Scratch::new();
    let commit = source(db, store, read, read.root(), permit.clone()).await?;
    let Object::NativeCommit(commit_value) = commit.object() else {
        return Err(fail(500, "proposal_object_error"));
    };
    if commit_value.snapshot != read.snapshot() {
        return Err(fail(500, "proposal_object_error"));
    }
    let snapshot = source(
        db,
        store,
        read,
        child(&commit, read.snapshot())?,
        permit.clone(),
    )
    .await?;
    let Object::Snapshot { root } = snapshot.object() else {
        return Err(fail(500, "proposal_object_error"));
    };
    let root = *root;
    let root_node = source(db, store, read, child(&snapshot, root)?, permit.clone()).await?;
    let mut directory_bytes = snapshot.encoded_length() + root_node.encoded_length();
    scratch.load(snapshot).map_err(core)?;
    let mut directories = BTreeMap::new();
    directories.insert(root, root_node);
    // Fetch ancestor directories of explicit edits, not whole history or blobs of
    // unchanged files. Every node was derived from verified pinned source bytes.
    for op in command.operations.operations() {
        let mut current = root;
        let parts: Vec<_> = op.path.components().collect();
        for part in &parts[..parts.len() - 1] {
            let parent = directories
                .get(&current)
                .ok_or(fail(500, "proposal_object_error"))?;
            let Object::Directory(dir) = parent.object() else {
                return Err(fail(500, "proposal_object_error"));
            };
            let Some(entry) = dir.find(part) else {
                break;
            };
            if entry.mode != EntryMode::Directory {
                return Err(fail(409, "change_base_mismatch"));
            }
            if !directories.contains_key(&entry.target) {
                if directories.len() >= MAX_OBJECTS {
                    return Err(fail(413, "proposal_limit"));
                }
                let value = source(
                    db,
                    store,
                    read,
                    child(parent, entry.target)?,
                    permit.clone(),
                )
                .await?;
                directory_bytes += value.encoded_length();
                if directory_bytes > MAX_SCRATCH as u64 {
                    return Err(fail(413, "proposal_limit"));
                }
                directories.insert(entry.target, value);
            }
            current = entry.target;
        }
    }
    for (_, object) in directories {
        scratch.load(object).map_err(core)?;
    }
    let blobs: BTreeSet<_> = command
        .operations
        .operations()
        .iter()
        .filter_map(|o| o.replacement.map(|r| r.target))
        .collect();
    for id in blobs {
        let batch = db.open_proposal_blob(read, store, id).await.map_err(meta)?;
        let object = verify(batch, permit.clone()).await?;
        scratch.load(object).map_err(core)?;
    }
    let operations = command.operations.clone();
    let base = read.snapshot();
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let applied = prepare_change(&scratch, base, &operations).map_err(core)?;
        let objects = scratch
            .publications(&[applied.operations(), applied.result_snapshot()])
            .map_err(core)?;
        Ok((applied, objects))
    })
    .await
    .map_err(|_| fail(500, "proposal_worker_failed"))?
}
async fn publish(
    db: &mut Metadata,
    store: &ManagedStore,
    auth: &Authorization,
    upload: Uuid,
    applied: &AppliedChange,
    objects: Vec<PreparedObject>,
    permit: Permit,
) -> Result<(ObjectLease, ObjectLease)> {
    let mut ops = None;
    let mut result = None;
    for object in objects {
        let lease = db
            .reserve_proposal_object(auth, upload, store, &object)
            .await
            .map_err(meta)?;
        if lease.requires_upload() {
            let depot = store.clone();
            let generation = lease.key().generation();
            let hold = permit.clone();
            let receipt = tokio::task::spawn_blocking(move || {
                let _permit = hold;
                depot.put(generation, &object)
            })
            .await
            .map_err(|_| fail(500, "proposal_worker_failed"))?
            .map_err(core)?;
            db.publish_object(&lease, &receipt).await.map_err(meta)?;
        }
        if lease.key().id() == applied.operations() {
            ops = Some(lease);
        } else if lease.key().id() == applied.result_snapshot() {
            result = Some(lease);
        }
        // Keep every child hold until its parent is durable; bounded holds expire
        // after publication or process loss. Never unpin a child between writes.
    }
    Ok((
        ops.ok_or(fail(500, "proposal_object_error"))?,
        result.ok_or(fail(500, "proposal_object_error"))?,
    ))
}
async fn execute(
    db: &mut Metadata,
    auth: &Authorization,
    read: &ProposalRead,
    command: &ProposalCommand,
    store: &ManagedStore,
    permit: Permit,
) -> Result<Value> {
    let (applied, objects) = compute(db, store, read, command, permit.clone()).await?;
    let (ops, result) = publish(db, store, auth, command.upload, &applied, objects, permit).await?;
    let outcome = db
        .finish_change_proposal(auth, command, &applied, &ops, &result)
        .await;
    // Acknowledged roots have permanent pins; failures leave bounded temporary
    // holds. Do not turn an already committed result into an ambiguous error just
    // because optional early hold cleanup could not complete.
    if outcome.is_ok() {
        let _ = db.release_object_hold(&ops).await;
        let _ = db.release_object_hold(&result).await;
    }
    outcome.map_err(meta)
}
pub(crate) async fn submit(
    bytes: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
) -> Result<Value> {
    let (scope, command) = parse(bytes)?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &scope, Permission::ChangeWrite)
        .await
        .map_err(meta)?;
    let read = match db
        .metadata
        .open_change_proposal(&auth, &command)
        .await
        .map_err(meta)?
    {
        ProposalStart::Replay(value) => return Ok(json!({"version":1,"result":value})),
        ProposalStart::Ready(read) => read,
    };
    let outcome = async {
        let permit = Arc::new(
            backend
                .permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| fail(503, "proposal_busy"))?,
        );
        let tenant = TenantId::new(scope.tenant()).map_err(core)?;
        let config = backend.config;
        let hold = permit.clone();
        let store = tokio::task::spawn_blocking(move || {
            let _permit = hold;
            ManagedStore::open(config.root, tenant, &config.backend)
        })
        .await
        .map_err(|_| fail(500, "proposal_worker_failed"))?
        .map_err(core)?;
        execute(&mut db.metadata, &auth, &read, &command, &store, permit).await
    }
    .await;
    let _ = db.metadata.close_change_proposal(*read).await;
    outcome.map(|v| json!({"version":1,"result":v}))
}
