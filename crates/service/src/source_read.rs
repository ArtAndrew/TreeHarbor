//! Bounded source delivery from an owned workspace, never a global digest oracle.
use crate::{
    database::Database,
    pack::hex,
    source::{Backend, Failure},
};
use bytes::Bytes;
use deadpool::managed::Pool;
use hyper::{HeaderMap, header::HeaderValue};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::OwnedSemaphorePermit;
use treeharbor_core::{EntryMode, Name, Object, ObjectId, RepoPath};
use treeharbor_metadata::{
    Authority, Metadata, Scope,
    access::{Permission, SecretToken},
    reads::{MAX_READ_BYTES, ReadNode, ReadObject},
    workspace_reads::{SourceFence, SourceIdentity, WorkspaceRead},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    tenant: String,
    repository: String,
    workspace: String,
    generation: i64,
    authority: String,
    epoch: i64,
    operation: Operation,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    File {
        path_hex: String,
    },
    List {
        path_hex: String,
        after_hex: Option<String>,
        limit: usize,
    },
}
enum Query {
    File,
    List {
        after: Option<Vec<u8>>,
        limit: usize,
    },
}
struct Parsed {
    scope: Scope,
    fence: SourceFence,
    path: Vec<u8>,
    query: Query,
}
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
type Result<T> = std::result::Result<T, Failure>;
pub(crate) fn metadata(error: treeharbor_metadata::Error) -> Failure {
    match error {
        treeharbor_metadata::Error::ReadLimit => fail(413, "source_read_limit"),
        treeharbor_metadata::Error::ExecutionState => fail(409, "execution_state_changed"),
        other => crate::source::metadata(other),
    }
}
fn uuid(value: &str) -> Result<Uuid> {
    let id: Uuid = value.parse().map_err(|_| fail(400, "invalid_request"))?;
    if id.to_string() != value {
        return Err(fail(400, "invalid_request"));
    }
    Ok(id)
}
pub(crate) fn decode_hex(value: &str, max: usize) -> Result<Vec<u8>> {
    if value.len() > max * 2
        || !value.len().is_multiple_of(2)
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(fail(400, "invalid_request"));
    }
    (0..value.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&value[i..i + 2], 16).map_err(|_| fail(400, "invalid_request")))
        .collect()
}
fn parse(bytes: &[u8]) -> Result<Parsed> {
    let wire: Request = serde_json::from_slice(bytes).map_err(|_| fail(400, "invalid_request"))?;
    if wire.version != 1 || wire.generation <= 0 || wire.epoch <= 0 {
        return Err(fail(400, "invalid_request"));
    }
    let scope = Scope::new(wire.tenant, uuid(&wire.repository)?)
        .map_err(|_| fail(400, "invalid_request"))?;
    let fence = SourceFence {
        workspace: uuid(&wire.workspace)?,
        generation: wire.generation,
        epoch: wire.epoch,
        authority: match wire.authority.as_str() {
            "connected" => Authority::Connected,
            "standalone" => Authority::Standalone,
            _ => return Err(fail(400, "invalid_request")),
        },
    };
    let (path, query) = match wire.operation {
        Operation::File { path_hex } => (decode_hex(&path_hex, 4096)?, Query::File),
        Operation::List {
            path_hex,
            after_hex,
            limit,
        } => {
            if !(1..=256).contains(&limit) {
                return Err(fail(400, "invalid_request"));
            }
            let after = after_hex.map(|s| decode_hex(&s, 255)).transpose()?;
            if let Some(name) = &after {
                Name::new(name.clone()).map_err(|_| fail(400, "invalid_request"))?;
            }
            (decode_hex(&path_hex, 4096)?, Query::List { after, limit })
        }
    };
    if !path.is_empty() {
        RepoPath::new(path.clone()).map_err(|_| fail(400, "invalid_request"))?;
    } else if matches!(query, Query::File) {
        return Err(fail(400, "invalid_request"));
    }
    Ok(Parsed {
        scope,
        fence,
        path,
        query,
    })
}

// Capacity travels with Bytes even after Hyper has consumed/dropped the body.
// A slow receiver cannot release the slot while a large buffer remains queued.
struct HeldBytes {
    bytes: Vec<u8>,
    _permit: Arc<OwnedSemaphorePermit>,
}
impl AsRef<[u8]> for HeldBytes {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
pub(crate) struct Delivery {
    pub bytes: Bytes,
    pub headers: HeaderMap,
}
enum Found {
    File {
        id: ObjectId,
        mode: EntryMode,
        frame: Vec<u8>,
    },
    List {
        id: ObjectId,
        entries: Vec<Value>,
        next: Option<String>,
    },
}
async fn read_one(
    db: &mut Metadata,
    store: &ManagedStore,
    session: &WorkspaceRead,
    node: ReadNode,
    budget: &mut u64,
    permit: Arc<OwnedSemaphorePermit>,
) -> Result<ReadObject> {
    if *budget == 0 {
        return Err(fail(413, "source_read_limit"));
    }
    let batch = db
        .open_workspace_source_batch(session, store, &[node], *budget)
        .await
        .map_err(metadata)?;
    let value = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        batch.read()
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))?
    .map_err(metadata)?
    .pop()
    .ok_or(fail(500, "source_invalid"))?;
    *budget = budget
        .checked_sub(value.encoded_length())
        .ok_or(fail(413, "source_read_limit"))?;
    Ok(value)
}
fn child(object: &ReadObject, id: ObjectId) -> Result<ReadNode> {
    object
        .children()
        .find(|n| n.id() == id)
        .ok_or(fail(500, "source_invalid"))
}
async fn resolve(
    db: &mut Metadata,
    store: &ManagedStore,
    session: &WorkspaceRead,
    request: &Parsed,
    permit: Arc<OwnedSemaphorePermit>,
) -> Result<Found> {
    let mut budget = MAX_READ_BYTES;
    let commit = read_one(
        db,
        store,
        session,
        session.root(),
        &mut budget,
        permit.clone(),
    )
    .await?;
    let Object::NativeCommit(value) = commit.object() else {
        return Err(fail(500, "source_invalid"));
    };
    if value.snapshot != session.identity().snapshot {
        return Err(fail(500, "source_invalid"));
    }
    let snapshot = read_one(
        db,
        store,
        session,
        child(&commit, value.snapshot)?,
        &mut budget,
        permit.clone(),
    )
    .await?;
    let Object::Snapshot { root } = snapshot.object() else {
        return Err(fail(500, "source_invalid"));
    };
    let mut current = read_one(
        db,
        store,
        session,
        child(&snapshot, *root)?,
        &mut budget,
        permit.clone(),
    )
    .await?;
    let mut mode = EntryMode::Directory;
    for name in request.path.split(|b| *b == b'/').filter(|s| !s.is_empty()) {
        let Object::Directory(dir) = current.object() else {
            return Err(fail(404, "source_path_not_found"));
        };
        let entry = dir.find(name).ok_or(fail(404, "source_path_not_found"))?;
        mode = entry.mode;
        let next = child(&current, entry.target)?;
        current = read_one(db, store, session, next, &mut budget, permit.clone()).await?;
    }
    let id = current.id();
    match &request.query {
        Query::File => {
            if !matches!(current.object(), Object::Blob(_)) || mode == EntryMode::Directory {
                return Err(fail(409, "source_path_type"));
            }
            let object = current.into_object();
            let frame = tokio::task::spawn_blocking(move || {
                let _permit = permit;
                object.encode()
            })
            .await
            .map_err(|_| fail(500, "source_worker_failed"))?
            .map_err(|_| fail(500, "source_invalid"))?;
            Ok(Found::File { id, mode, frame })
        }
        Query::List { after, limit } => {
            let Object::Directory(dir) = current.object() else {
                return Err(fail(409, "source_path_type"));
            };
            let start = after.as_ref().map_or(0, |a| {
                dir.entries()
                    .partition_point(|e| e.name.as_bytes() <= a.as_slice())
            });
            let selected: Vec<_> = dir.entries()[start..].iter().take(limit + 1).collect();
            let next = if selected.len() > *limit {
                Some(hex(selected[limit - 1].name.as_bytes()))
            } else {
                None
            };
            let entries=selected.into_iter().take(*limit).map(|e|json!({"name_hex":hex(e.name.as_bytes()),"mode":mode_name(e.value.mode),"object":e.value.target.to_string()})).collect();
            Ok(Found::List { id, entries, next })
        }
    }
}
pub(crate) fn mode_name(mode: EntryMode) -> &'static str {
    match mode {
        EntryMode::Directory => "040000",
        EntryMode::File => "100644",
        EntryMode::Executable => "100755",
        EntryMode::Symlink => "120000",
    }
}
fn context(request: &Parsed, identity: &SourceIdentity) -> Value {
    json!({"workspace":request.fence.workspace.to_string(),"generation":request.fence.generation,"authority":request.fence.authority.as_str(),"epoch":request.fence.epoch,"base":identity.revision.to_string(),"snapshot":identity.snapshot.to_string(),"profile_digest":identity.profile.to_string()})
}
pub(crate) async fn read(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
) -> Result<Delivery> {
    let request = parse(body)?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &request.scope, Permission::Read)
        .await
        .map_err(metadata)?;
    let permit = Arc::new(
        backend
            .read_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(503, "source_read_busy"))?,
    );
    let tenant = TenantId::new(request.scope.tenant()).map_err(|_| fail(500, "source_invalid"))?;
    let opening_permit = permit.clone();
    let store = tokio::task::spawn_blocking(move || {
        let _permit = opening_permit;
        ManagedStore::open(&backend.config.root, tenant, &backend.config.backend)
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))?
    .map_err(|_| fail(500, "source_storage_failed"))?;
    let session = db
        .metadata
        .open_workspace_source(&auth, request.fence, 120)
        .await
        .map_err(metadata)?;
    let result = resolve(&mut db.metadata, &store, &session, &request, permit.clone()).await;
    let identity = db
        .metadata
        .finish_workspace_source(session)
        .await
        .map_err(metadata)?;
    let found = result?;
    let mut headers = HeaderMap::new();
    let bytes = match found {
        Found::File { id, mode, frame } => {
            headers.insert(
                "content-type",
                HeaderValue::from_static("application/octet-stream"),
            );
            headers.insert("x-treeharbor-version", HeaderValue::from_static("1"));
            for (name, value) in [
                (
                    "x-treeharbor-workspace",
                    request.fence.workspace.to_string(),
                ),
                (
                    "x-treeharbor-generation",
                    request.fence.generation.to_string(),
                ),
                (
                    "x-treeharbor-authority",
                    request.fence.authority.as_str().into(),
                ),
                ("x-treeharbor-epoch", request.fence.epoch.to_string()),
                ("x-treeharbor-revision", identity.revision.to_string()),
                ("x-treeharbor-snapshot", identity.snapshot.to_string()),
                ("x-treeharbor-profile", identity.profile.to_string()),
                ("x-treeharbor-object", id.to_string()),
                ("x-treeharbor-mode", mode_name(mode).into()),
            ] {
                headers.insert(
                    name,
                    HeaderValue::from_str(&value).map_err(|_| fail(500, "source_invalid"))?,
                );
            }
            frame
        }
        Found::List { id, entries, next } => {
            headers.insert("content-type", HeaderValue::from_static("application/json"));
            let mut value = context(&request, &identity);
            value["path_hex"] = json!(hex(&request.path));
            value["object"] = json!(id.to_string());
            value["entries"] = json!(entries);
            value["next_after_hex"] = json!(next);
            serde_json::to_vec(&json!({"version":1,"result":value}))
                .map_err(|_| fail(500, "source_invalid"))?
        }
    };
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("connection", HeaderValue::from_static("close"));
    Ok(Delivery {
        headers,
        bytes: Bytes::from_owner(HeldBytes {
            bytes,
            _permit: permit,
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wire_v1_rejects_ambiguous_paths_and_unbounded_requests() {
        let raw = include_bytes!("../tests/fixtures/source-read-v1.json");
        let request = parse(raw).ok().unwrap();
        assert_eq!(request.scope.tenant(), "example");
        assert_eq!(request.fence.generation, 7);
        assert_eq!(request.path, b"src/main.rs");
        assert!(matches!(request.query, Query::File));
        let original: Value = serde_json::from_slice(raw).unwrap();
        let mut cases = Vec::new();
        for path in [
            "",
            "2e2e2f736563726574",
            "2f657463",
            "7372632f2e676974",
            "6A",
            "1",
        ] {
            let mut value = original.clone();
            value["operation"]["path_hex"] = json!(path);
            cases.push(value);
        }
        let mut value = original.clone();
        value["operation"]["kind"] = json!("object");
        cases.push(value);
        let mut value = original.clone();
        value["principal"] = json!("owner");
        cases.push(value);
        let mut value = original.clone();
        value["version"] = json!(2);
        cases.push(value);
        let mut value = original.clone();
        value["operation"] = json!({"kind":"list","path_hex":"","limit":257});
        cases.push(value);
        for value in cases {
            assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    #[tokio::test]
    async fn response_capacity_lives_until_last_queued_byte_owner_is_dropped() {
        let slots = Arc::new(tokio::sync::Semaphore::new(1));
        let permit = Arc::new(slots.clone().try_acquire_owned().unwrap());
        let bytes = Bytes::from_owner(HeldBytes {
            bytes: b"queued source bytes".to_vec(),
            _permit: permit,
        });
        let queued = bytes.slice(2..);
        drop(bytes);
        assert!(slots.clone().try_acquire_owned().is_err());
        drop(queued);
        assert!(slots.try_acquire_owned().is_ok());
    }
}
