//! Authenticated commit-only history reads from retained repository roots.
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
use treeharbor_core::{Commit, Object, ObjectId, ObjectStore, RevisionRef};
use treeharbor_history::History;
use treeharbor_metadata::{
    Authority, Metadata, Scope,
    access::{Permission, SecretToken},
    exports::{ExportFence, SourceExport},
    reads::MAX_READ_OBJECTS,
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;
const MAX_COMMITS: usize = 10_000;
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_EDGES: usize = 80_000;
const MAX_RESPONSE: usize = 1024 * 1024;
type Result<T> = std::result::Result<T, Failure>;
type Permit = Arc<OwnedSemaphorePermit>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn invalid(_: impl std::fmt::Display) -> Failure {
    fail(500, "history_invalid")
}
fn limit() -> Failure {
    fail(413, "history_limit")
}
fn native(value: &str) -> Result<ObjectId> {
    let revision: RevisionRef = value.parse().map_err(|_| fail(400, "invalid_request"))?;
    if revision.to_string() != value {
        return Err(fail(400, "invalid_request"));
    }
    match revision {
        RevisionRef::Native(id) => Ok(id),
        _ => Err(fail(400, "native_revision_required")),
    }
}
fn revision(id: ObjectId) -> String {
    RevisionRef::Native(id).to_string()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    authority: String,
    epoch: i64,
    query: WireQuery,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum WireQuery {
    Log {
        revision: String,
        limit: usize,
        after: Option<String>,
        first_parent: bool,
    },
    Show {
        revision: String,
        commit: String,
    },
    IsAncestor {
        ancestor: String,
        head: String,
    },
    MergeBases {
        left: String,
        right: String,
    },
}
enum Query {
    Log {
        limit: usize,
        after: Option<ObjectId>,
        first_parent: bool,
    },
    Show(ObjectId),
    IsAncestor,
    MergeBases,
}
struct Request {
    scope: Scope,
    authority: Authority,
    epoch: i64,
    roots: Vec<ObjectId>,
    query: Query,
}
fn parse(bytes: &[u8]) -> Result<Request> {
    let w: Wire = serde_json::from_slice(bytes).map_err(|_| fail(400, "invalid_request"))?;
    let repo: Uuid = w
        .repository
        .parse()
        .map_err(|_| fail(400, "invalid_request"))?;
    if w.version != 1 || w.epoch <= 0 || repo.to_string() != w.repository {
        return Err(fail(400, "invalid_request"));
    }
    let authority = match w.authority.as_str() {
        "standalone" => Authority::Standalone,
        "connected" => Authority::Connected,
        _ => return Err(fail(400, "invalid_request")),
    };
    let (roots, query) = match w.query {
        WireQuery::Log {
            revision,
            limit,
            after,
            first_parent,
        } => {
            if !(1..=256).contains(&limit) {
                return Err(fail(400, "invalid_request"));
            }
            (
                vec![native(&revision)?],
                Query::Log {
                    limit,
                    after: after.map(|s| native(&s)).transpose()?,
                    first_parent,
                },
            )
        }
        WireQuery::Show { revision, commit } => {
            (vec![native(&revision)?], Query::Show(native(&commit)?))
        }
        WireQuery::IsAncestor { ancestor, head } => {
            (vec![native(&ancestor)?, native(&head)?], Query::IsAncestor)
        }
        WireQuery::MergeBases { left, right } => {
            (vec![native(&left)?, native(&right)?], Query::MergeBases)
        }
    };
    Ok(Request {
        scope: Scope::new(w.tenant, repo).map_err(|_| fail(400, "invalid_request"))?,
        authority,
        epoch: w.epoch,
        roots,
        query,
    })
}
// Request-local verified computation cache, never the authoritative object store.
#[derive(Default)]
struct Graph(BTreeMap<ObjectId, Commit>);
impl ObjectStore for Graph {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        self.0
            .get(&id)
            .cloned()
            .map(Object::NativeCommit)
            .ok_or(treeharbor_core::Error::MissingObject(id))
    }
    fn put(&self, _: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject("read-only history"))
    }
}
struct Budget {
    bytes: u64,
    edges: usize,
}
async fn load(
    db: &mut Metadata,
    store: &ManagedStore,
    read: &SourceExport,
    graph: &mut Graph,
    budget: &mut Budget,
    permit: Permit,
) -> Result<()> {
    let root = read.root().id();
    let mut pending = BTreeMap::from([(root, read.root())]);
    while !pending.is_empty() {
        pending.retain(|id, _| !graph.0.contains_key(id));
        if pending.is_empty() {
            break;
        }
        if graph.0.len() + pending.len() > MAX_COMMITS || budget.bytes == 0 {
            return Err(limit());
        }
        let nodes: Vec<_> = pending.values().take(MAX_READ_OBJECTS).cloned().collect();
        let batch = db
            .read_source_export(read, store, &nodes, budget.bytes)
            .await
            .map_err(metadata)?;
        let hold = permit.clone();
        let objects = tokio::task::spawn_blocking(move || {
            let _permit = hold;
            batch.read()
        })
        .await
        .map_err(|_| fail(500, "history_worker_failed"))?
        .map_err(metadata)?;
        for object in objects {
            let id = object.id();
            let Object::NativeCommit(c) = object.object() else {
                return Err(invalid("kind"));
            };
            budget.bytes = budget
                .bytes
                .checked_sub(object.encoded_length())
                .ok_or_else(limit)?;
            budget.edges = budget
                .edges
                .checked_add(c.parents.len())
                .ok_or_else(limit)?;
            if budget.edges > MAX_EDGES {
                return Err(limit());
            }
            let parents: BTreeSet<_> = c.parents.iter().copied().collect();
            for child in object.children().filter(|n| parents.contains(&n.id())) {
                if !graph.0.contains_key(&child.id()) {
                    pending.entry(child.id()).or_insert(child);
                }
            }
            pending.remove(&id);
            graph.0.insert(id, c.clone());
            if graph.0.len() + pending.len() > MAX_COMMITS {
                return Err(limit());
            }
        }
    }
    if graph
        .0
        .get(&root)
        .is_none_or(|c| c.snapshot != read.snapshot())
    {
        return Err(invalid("root"));
    }
    Ok(())
}
fn commit_value(id: ObjectId, c: &Commit) -> Value {
    json!({"revision":revision(id),"parents":c.parents.iter().copied().map(revision).collect::<Vec<_>>(),"snapshot":c.snapshot.to_string(),"changes":c.changes.to_string(),"author_name":c.author_name,"author_email":c.author_email,"timestamp_seconds":c.timestamp_seconds,"timezone_minutes":c.timezone_minutes,"message":c.message,
        "git_origin":c.git_origin.as_ref().map(|o|json!({"revision":RevisionRef::Git(o.oid).to_string(),"raw_commit":o.raw.to_string()}))})
}
fn answer(request: &Request, graph: &Graph) -> Result<Vec<u8>> {
    let history = History::new(graph, MAX_COMMITS);
    let value = match &request.query {
        Query::Show(id) => {
            // Only the single authorized anchor's ancestry was loaded.
            let c = graph
                .0
                .get(id)
                .ok_or(fail(404, "history_commit_not_found"))?;
            json!({"kind":"show","commit":commit_value(*id,c)})
        }
        Query::IsAncestor => {
            json!({"kind":"is_ancestor","is_ancestor":history.is_ancestor(request.roots[0],request.roots[1]).map_err(invalid)?})
        }
        Query::MergeBases => {
            json!({"kind":"merge_bases","bases":history.merge_bases(request.roots[0],request.roots[1]).map_err(invalid)?.into_iter().map(revision).collect::<Vec<_>>()})
        }
        Query::Log {
            limit,
            after,
            first_parent,
        } => {
            let log = if *first_parent {
                history
                    .first_parent_log(request.roots[0], MAX_COMMITS)
                    .map_err(invalid)?
            } else {
                history.topological_log(request.roots[0]).map_err(invalid)?
            };
            let start = match after {
                None => 0,
                Some(id) => {
                    log.iter()
                        .position(|(i, _)| i == id)
                        .ok_or(fail(400, "invalid_history_cursor"))?
                        + 1
                }
            };
            let end = (start + limit).min(log.len());
            let rows: Vec<_> = log[start..end]
                .iter()
                .map(|(id, c)| commit_value(*id, c))
                .collect();
            let next = (end < log.len()).then(|| revision(log[end - 1].0));
            json!({"kind":"log","order":if *first_parent {"first_parent"}else{"children_before_parents_id_v1"},"commits":rows,"next_after":next,"total_commits":log.len()})
        }
    };
    let output = json!({"version":1,"result":{"authority":request.authority.as_str(),"epoch":request.epoch,"roots":request.roots.iter().copied().map(revision).collect::<Vec<_>>(),"history":value}});
    let mut writer = ResponseWriter(Vec::new());
    serde_json::to_writer(&mut writer, &output).map_err(|_| limit())?;
    Ok(writer.0)
}
struct ResponseWriter(Vec<u8>);
impl std::io::Write for ResponseWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if self.0.len().saturating_add(bytes.len()) > MAX_RESPONSE {
            return Err(std::io::Error::other("history response limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
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
    let tenant = TenantId::new(request.scope.tenant()).map_err(invalid)?;
    let opening = permit.clone();
    let store = tokio::task::spawn_blocking(move || {
        let _permit = opening;
        ManagedStore::open(backend.config.root, tenant, &backend.config.backend)
    })
    .await
    .map_err(|_| fail(500, "history_worker_failed"))?
    .map_err(invalid)?;
    let mut reads = Vec::new();
    let result = async {
        for root in &request.roots {
            reads.push(
                db.metadata
                    .open_source_export(
                        &auth,
                        ExportFence {
                            authority: request.authority,
                            epoch: request.epoch,
                            revision: *root,
                        },
                        120,
                    )
                    .await
                    .map_err(metadata)?,
            );
        }
        let mut graph = Graph::default();
        let mut budget = Budget {
            bytes: MAX_BYTES,
            edges: 0,
        };
        for read in &reads {
            load(
                &mut db.metadata,
                &store,
                read,
                &mut graph,
                &mut budget,
                permit.clone(),
            )
            .await?;
        }
        let hold = permit.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = hold;
            answer(&request, &graph)
        })
        .await
        .map_err(|_| fail(500, "history_worker_failed"))?
    }
    .await;
    let mut rejected = None;
    // Close ALL holds, including when opening the second anchor failed.
    for read in reads {
        if let Err(e) = db.metadata.finish_source_export(read).await {
            rejected.get_or_insert_with(|| metadata(e));
        }
    }
    if let Some(e) = rejected {
        return Err(e);
    }
    let bytes = result?;
    let mut headers = HeaderMap::new();
    headers.insert("content-type", HeaderValue::from_static("application/json"));
    headers.insert("cache-control", HeaderValue::from_static("no-store"));
    headers.insert("connection", HeaderValue::from_static("close"));
    Ok(Delivery {
        bytes: Bytes::from_owner(Held {
            bytes,
            _permit: permit,
        }),
        headers,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> (Request, Graph) {
        let request = parse(include_bytes!("../tests/fixtures/history-read-v1.json"))
            .ok()
            .unwrap();
        let line = include_str!("../../core/tests/fixtures/git-origin-v2.txt")
            .lines()
            .nth(5)
            .unwrap();
        let hex = line.split_whitespace().nth(1).unwrap();
        let frame: Vec<_> = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect();
        let Object::NativeCommit(commit) =
            Object::decode_verified(request.roots[0], &frame).unwrap()
        else {
            panic!()
        };
        let graph = Graph(BTreeMap::from([(request.roots[0], commit)]));
        (request, graph)
    }
    #[test]
    fn independent_wire_preserves_exact_commit_metadata() {
        let (request, graph) = fixture();
        assert_eq!(
            answer(&request, &graph).ok().unwrap(),
            include_bytes!("../tests/fixtures/history-response-v1.json")
                .strip_suffix(b"\n")
                .unwrap()
        );
    }
    #[test]
    fn history_wire_rejects_unbounded_ambiguous_and_privileged_fields() {
        let original: Value =
            serde_json::from_slice(include_bytes!("../tests/fixtures/history-read-v1.json"))
                .unwrap();
        let rev = original["query"]["revision"].clone();
        let mut cases = Vec::new();
        for (field, value) in [
            ("epoch", json!(0)),
            ("version", json!(2)),
            ("principal", json!("admin")),
            ("authority", json!("both")),
            ("repository", json!("11111111111141118111111111111111")),
        ] {
            let mut v = original.clone();
            v[field] = value;
            cases.push(v);
        }
        for query in [
            json!({"kind":"show","revision":rev,"commit":"git:sha1:0000000000000000000000000000000000000000"}),
            json!({"kind":"show","revision":rev,"commit":rev,"admin":true}),
            json!({"kind":"log","revision":rev,"limit":0,"first_parent":false}),
            json!({"kind":"log","revision":rev,"limit":257,"first_parent":false}),
            json!({"kind":"log","revision":rev,"limit":1,"after":"main","first_parent":false}),
        ] {
            let mut v = original.clone();
            v["query"] = query;
            cases.push(v);
        }
        for value in cases {
            assert!(parse(&serde_json::to_vec(&value).unwrap()).is_err());
        }
    }
    #[test]
    fn missing_history_invalid_cursor_and_large_escaped_metadata_fail() {
        let (mut request, mut graph) = fixture();
        let missing = ObjectId::from_bytes([255; 32]);
        request.query = Query::Show(missing);
        assert_eq!(answer(&request, &graph).unwrap_err().status, 404);
        request.query = Query::Log {
            limit: 1,
            after: Some(missing),
            first_parent: false,
        };
        assert_eq!(
            answer(&request, &graph).unwrap_err().code,
            "invalid_history_cursor"
        );
        request.query = Query::Log {
            limit: 1,
            after: None,
            first_parent: false,
        };
        graph.0.get_mut(&request.roots[0]).unwrap().parents = vec![missing];
        assert!(answer(&request, &graph).is_err());
        graph.0.get_mut(&request.roots[0]).unwrap().parents.clear();
        graph.0.get_mut(&request.roots[0]).unwrap().message = "\0".repeat(MAX_RESPONSE / 4);
        assert_eq!(answer(&request, &graph).unwrap_err().status, 413);
    }
}
