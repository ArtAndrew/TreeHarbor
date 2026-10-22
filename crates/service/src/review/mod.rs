//! Scoped review of immutable versions/revisions; no mutation or merge authority.
mod render;
use crate::{
    database::Database,
    source::{Backend, Failure},
    source_read::{Delivery, metadata},
};
use bytes::Bytes;
use deadpool::managed::Pool;
use hyper::{HeaderMap, header::HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
use tokio::sync::OwnedSemaphorePermit;
use treeharbor_core::*;
use treeharbor_metadata::{
    Authority, Metadata, Scope,
    access::{Permission, SecretToken},
    reads::ReadNode,
    review::{ReviewFence, ReviewRead, ReviewTarget},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
type Permit = Arc<OwnedSemaphorePermit>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn invalid(_: impl std::fmt::Display) -> Failure {
    fail(500, "review_invalid")
}
fn limit() -> Failure {
    fail(413, "review_limit")
}
fn core(e: treeharbor_core::Error) -> Failure {
    match e {
        treeharbor_core::Error::TooLarge => limit(),
        _ => invalid(e),
    }
}
fn uuid(s: &str) -> Result<Uuid> {
    let id: Uuid = s.parse().map_err(|_| fail(400, "invalid_request"))?;
    if id.to_string() != s {
        return Err(fail(400, "invalid_request"));
    }
    Ok(id)
}
fn native(s: &str) -> Result<ObjectId> {
    match s.parse().map_err(|_| fail(400, "invalid_request"))? {
        RevisionRef::Native(id) => Ok(id),
        _ => Err(fail(400, "native_revision_required")),
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    authority: String,
    epoch: i64,
    query: Query,
    limit: usize,
    after: Option<String>,
    text: bool,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Query {
    Change { changeset: String, version: i64 },
    Revisions { before: String, after: String },
}
struct Request {
    scope: Scope,
    fence: ReviewFence,
    limit: usize,
    after: Option<Vec<u8>>,
    text: bool,
}
fn parse(body: &[u8]) -> Result<Request> {
    let w: Wire = serde_json::from_slice(body).map_err(|_| fail(400, "invalid_request"))?;
    if w.version != 1 || w.epoch <= 0 || !(1..=100).contains(&w.limit) {
        return Err(fail(400, "invalid_request"));
    }
    let authority = match w.authority.as_str() {
        "standalone" => Authority::Standalone,
        "connected" => Authority::Connected,
        _ => return Err(fail(400, "invalid_request")),
    };
    let target = match w.query {
        Query::Change { changeset, version } => {
            if version <= 0 {
                return Err(fail(400, "invalid_request"));
            }
            ReviewTarget::Change {
                changeset: uuid(&changeset)?,
                version,
            }
        }
        Query::Revisions { before, after } => ReviewTarget::Revisions {
            before: native(&before)?,
            after: native(&after)?,
        },
    };
    let after = w
        .after
        .map(|s| crate::source_read::decode_hex(&s, 4096))
        .transpose()?;
    if let Some(p) = &after {
        RepoPath::new(p.clone()).map_err(|_| fail(400, "invalid_request"))?;
    }
    Ok(Request {
        scope: Scope::new(w.tenant, uuid(&w.repository)?)
            .map_err(|_| fail(400, "invalid_request"))?,
        fence: ReviewFence {
            authority,
            epoch: w.epoch,
            target,
        },
        limit: w.limit,
        after,
        text: w.text,
    })
}
#[derive(Default)]
struct Graph(BTreeMap<ObjectId, Object>);
impl ObjectStore for Graph {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        self.0
            .get(&id)
            .cloned()
            .ok_or(treeharbor_core::Error::MissingObject(id))
    }
    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::Storage("read-only review".into()))
    }
}
struct Loader<'a> {
    db: &'a mut Metadata,
    read: &'a ReviewRead,
    store: &'a ManagedStore,
    permit: Permit,
    graph: Graph,
    known: BTreeMap<ObjectId, (usize, ReadNode)>,
    bytes: u64,
    entries: usize,
    paths: usize,
    dirs: usize,
}
impl<'a> Loader<'a> {
    fn new(
        db: &'a mut Metadata,
        read: &'a ReviewRead,
        store: &'a ManagedStore,
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
            read,
            store,
            permit,
            graph: Graph::default(),
            known,
            bytes: 128 * 1024 * 1024,
            entries: 0,
            paths: 0,
            dirs: 0,
        }
    }
    async fn get(&mut self, id: ObjectId, byte_limit: u64) -> Result<Object> {
        if let Some(o) = self.graph.0.get(&id) {
            return Ok(o.clone());
        }
        let (source, node) = self
            .known
            .get(&id)
            .cloned()
            .ok_or_else(|| invalid("review frontier"))?;
        let batch = self
            .db
            .read_review(
                self.read,
                source,
                self.store,
                &[node],
                byte_limit.min(self.bytes),
            )
            .await
            .map_err(|e| {
                if matches!(e, treeharbor_metadata::Error::ReadLimit) && self.bytes < byte_limit {
                    limit()
                } else {
                    metadata(e)
                }
            })?;
        let permit = self.permit.clone();
        let object = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            batch.read()
        })
        .await
        .map_err(invalid)?
        .map_err(metadata)?
        .pop()
        .ok_or_else(|| invalid("empty read"))?;
        self.bytes = self
            .bytes
            .checked_sub(object.encoded_length())
            .ok_or_else(limit)?;
        for child in object.children() {
            if self.known.len() >= 200_016 && !self.known.contains_key(&child.id()) {
                return Err(limit());
            }
            self.known.entry(child.id()).or_insert((source, child));
        }
        if self.graph.0.len() >= 40_100 {
            return Err(limit());
        }
        let value = object.into_object();
        self.graph.0.insert(id, value.clone());
        Ok(value)
    }
    async fn directory(&mut self, id: ObjectId) -> Result<Directory> {
        self.dirs += 1;
        if self.dirs > 20_000 {
            return Err(limit());
        }
        let Object::Directory(dir) = self.get(id, 128 * 1024 * 1024).await? else {
            return Err(invalid("directory"));
        };
        self.entries = self
            .entries
            .checked_add(dir.entries().len())
            .ok_or_else(limit)?;
        if self.entries > 200_000 {
            return Err(limit());
        }
        Ok(dir)
    }
    fn path(&mut self, prefix: &[u8], name: &Name) -> Result<Vec<u8>> {
        let mut path = prefix.to_vec();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(name.as_bytes());
        self.paths = self.paths.checked_add(path.len()).ok_or_else(limit)?;
        if self.paths > 32 * 1024 * 1024 || path.len() > 4096 {
            return Err(limit());
        }
        Ok(path)
    }
    async fn prepare(&mut self) -> Result<[ObjectId; 2]> {
        let snapshots = self.read.snapshots();
        for root in self.read.roots() {
            self.get(root.id(), 128 * 1024 * 1024).await?;
        }
        let mut roots = Vec::new();
        for snapshot in snapshots {
            let Object::Snapshot { root } = self.get(snapshot, 128 * 1024 * 1024).await? else {
                return Err(invalid("snapshot"));
            };
            roots.push(root);
        }
        let mut pending = vec![(Vec::new(), roots[0], roots[1])];
        while let Some((prefix, before, after)) = pending.pop() {
            let b = self.directory(before).await?;
            if before == after {
                continue;
            }
            let a = self.directory(after).await?;
            let left: BTreeMap<_, _> = b.entries().iter().map(|e| (&e.name, e.value)).collect();
            for e in a.entries() {
                let path = self.path(&prefix, &e.name)?;
                if let Some(old) = left.get(&e.name)
                    && old != &e.value
                    && old.mode == EntryMode::Directory
                    && e.value.mode == EntryMode::Directory
                {
                    pending.push((path, old.target, e.value.target));
                }
            }
        }
        Ok(snapshots)
    }
    async fn expanded(
        &mut self,
        ops: Changes,
    ) -> Result<BTreeMap<Vec<u8>, (Option<EntryRef>, Option<EntryRef>)>> {
        let mut rows = BTreeMap::new();
        let mut pending: Vec<_> = ops
            .operations()
            .iter()
            .map(|o| (o.path.as_bytes().to_vec(), o.expected, o.replacement))
            .collect();
        while let Some((path, before, after)) = pending.pop() {
            if before == after {
                continue;
            }
            if rows.len() >= 16_000 {
                return Err(limit());
            }
            rows.insert(path.clone(), (before, after));
            let mut dirs = Vec::new();
            for e in [before, after] {
                dirs.push(match e {
                    Some(e) if e.mode == EntryMode::Directory => self.directory(e.target).await?,
                    _ => Directory::new(vec![]).map_err(core)?,
                });
            }
            let left: BTreeMap<_, _> = dirs[0]
                .entries()
                .iter()
                .map(|e| (&e.name, e.value))
                .collect();
            let right: BTreeMap<_, _> = dirs[1]
                .entries()
                .iter()
                .map(|e| (&e.name, e.value))
                .collect();
            let names: BTreeSet<_> = left.keys().chain(right.keys()).copied().collect();
            for name in names {
                let a = left.get(name).copied();
                let b = right.get(name).copied();
                if a != b {
                    pending.push((self.path(&path, name)?, a, b));
                }
            }
        }
        Ok(rows)
    }
    async fn content(&mut self, e: Option<EntryRef>) -> Result<render::Content> {
        let Some(e) = e else {
            return Ok(render::Content::Bytes(vec![]));
        };
        if e.mode == EntryMode::Directory {
            return Ok(render::Content::Omitted("directory"));
        }
        match self
            .get(e.target, (render::TEXT_BYTES + FRAME_HEADER) as u64)
            .await
        {
            Ok(Object::Blob(bytes)) => Ok(render::Content::Bytes(bytes)),
            Ok(_) => Err(invalid("review blob")),
            Err(f) if f.code == "source_read_limit" => Ok(render::Content::Omitted("size_limit")),
            Err(e) => Err(e),
        }
    }
}

async fn answer(
    db: &mut Metadata,
    store: &ManagedStore,
    read: &ReviewRead,
    r: &Request,
    permit: Permit,
) -> Result<Vec<u8>> {
    let mut loader = Loader::new(db, read, store, permit.clone());
    let snapshots = loader.prepare().await?;
    let graph = std::mem::take(&mut loader.graph);
    let hold = permit.clone();
    let (graph, ops) = tokio::task::spawn_blocking(move || {
        let _permit = hold;
        let ops = diff_snapshots(
            &graph,
            snapshots[0],
            snapshots[1],
            TreeDiffLimits {
                operations: 16_000,
                ..TreeDiffLimits::default()
            },
        );
        (graph, ops)
    })
    .await
    .map_err(invalid)?;
    loader.graph = graph;
    let rows = loader.expanded(ops.map_err(core)?).await?;
    let start = match &r.after {
        None => 0,
        Some(path) => {
            rows.keys()
                .position(|p| p == path)
                .ok_or_else(|| fail(400, "review_cursor"))?
                + 1
        }
    };
    let count = rows.len();
    let mut items = Vec::new();
    let mut last = None;
    let mut work = 16_000_000usize;
    for (path, (before, after)) in rows.iter().skip(start).take(r.limit) {
        let text = if r.text {
            let a = loader.content(*before).await?;
            let b = loader.content(*after).await?;
            let hold = permit.clone();
            let (value, remaining) = tokio::task::spawn_blocking(move || {
                let _permit = hold;
                let value = render::text(a, b, &mut work);
                (value, work)
            })
            .await
            .map_err(invalid)?;
            work = remaining;
            value
        } else {
            json!({"status":"omitted","reason":"not_requested"})
        };
        let path = render::hex(path);
        last = Some(path.clone());
        items.push(json!({"path_hex":path,"kind":render::change_kind(*before,*after),"before":render::entry(*before),"after":render::entry(*after),"text":text}));
    }
    let next = if start + items.len() < count {
        last
    } else {
        None
    };
    render::encode(
        &json!({"version":1,"result":{"tenant":r.scope.tenant(),"repository":r.scope.repository().to_string(),"authority":r.fence.authority.as_str(),"epoch":r.fence.epoch,"identity":read.identity(),"before_snapshot":snapshots[0].to_string(),"after_snapshot":snapshots[1].to_string(),"profile":render::PROFILE,"total_changes":count,"next_after":next,"changes":items}}),
    )
}
struct Held {
    bytes: Vec<u8>,
    _permit: Permit,
}
impl AsRef<[u8]> for Held {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
pub(crate) async fn read(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
) -> Result<Delivery> {
    let r = parse(body)?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &r.scope, Permission::Read)
        .await
        .map_err(metadata)?;
    let permit = Arc::new(
        backend
            .read_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(503, "source_read_busy"))?,
    );
    let tenant = TenantId::new(r.scope.tenant()).map_err(invalid)?;
    let hold = permit.clone();
    let store = tokio::task::spawn_blocking(move || {
        let _permit = hold;
        ManagedStore::open(backend.config.root, tenant, &backend.config.backend)
    })
    .await
    .map_err(invalid)?
    .map_err(invalid)?;
    let read = db
        .metadata
        .open_review(&auth, r.fence)
        .await
        .map_err(metadata)?;
    let result = answer(&mut db.metadata, &store, &read, &r, permit.clone()).await;
    db.metadata.finish_review(read).await.map_err(metadata)?;
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("connection", HeaderValue::from_static("close"));
    Ok(Delivery {
        bytes: Bytes::from_owner(Held {
            bytes: result?,
            _permit: permit,
        }),
        headers,
    })
}
