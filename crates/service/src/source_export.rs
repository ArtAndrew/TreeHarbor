//! Full source export through authenticated leases; private spool, bounded IO.
use crate::{
    archive,
    database::Database,
    source::{Backend, Failure},
    source_read::metadata,
};
use bytes::Bytes;
use deadpool::managed::Pool;
use hyper::body::{Body, Frame, SizeHint};
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Seek, Write},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, ReadBuf},
    sync::OwnedSemaphorePermit,
};
use treeharbor_core::{Object, RevisionRef};
use treeharbor_metadata::{
    Authority, Metadata, Scope,
    access::{Permission, SecretToken},
    exports::{ExportFence, SourceExport},
    reads::{MAX_READ_BYTES, MAX_READ_OBJECTS},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn invalid(_: impl std::fmt::Display) -> Failure {
    fail(500, "source_export_invalid")
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    authority: String,
    epoch: i64,
    revision: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectionWire {
    version: u8,
    tenant: String,
    repository: String,
    epoch: i64,
    projection: String,
}
enum Purpose {
    Source(ExportFence),
    Projection { epoch: i64, projection: Uuid },
}
fn projection_request(body: &[u8]) -> Result<(Scope, Purpose)> {
    let w: ProjectionWire =
        serde_json::from_slice(body).map_err(|_| fail(400, "invalid_request"))?;
    let id = |s: &str| -> Result<Uuid> {
        let id: Uuid = s.parse().map_err(|_| fail(400, "invalid_request"))?;
        if id.is_nil() || id.to_string() != s {
            return Err(fail(400, "invalid_request"));
        }
        Ok(id)
    };
    if w.version != 1 || w.epoch <= 0 {
        return Err(fail(400, "invalid_request"));
    }
    Ok((
        Scope::new(w.tenant, id(&w.repository)?).map_err(|_| fail(400, "invalid_request"))?,
        Purpose::Projection {
            epoch: w.epoch,
            projection: id(&w.projection)?,
        },
    ))
}

fn parse(body: &[u8]) -> Result<(Scope, ExportFence)> {
    let w: Wire = serde_json::from_slice(body).map_err(|_| fail(400, "invalid_request"))?;
    let repo: Uuid = w
        .repository
        .parse()
        .map_err(|_| fail(400, "invalid_request"))?;
    let revision: RevisionRef = w
        .revision
        .parse()
        .map_err(|_| fail(400, "invalid_request"))?;
    if w.version != 1
        || w.epoch <= 0
        || repo.to_string() != w.repository
        || revision.to_string() != w.revision
    {
        return Err(fail(400, "invalid_request"));
    }
    let RevisionRef::Native(revision) = revision else {
        return Err(fail(400, "native_revision_required"));
    };
    let authority = match w.authority.as_str() {
        "connected" => Authority::Connected,
        "standalone" => Authority::Standalone,
        _ => return Err(fail(400, "invalid_request")),
    };
    Ok((
        Scope::new(w.tenant, repo).map_err(|_| fail(400, "invalid_request"))?,
        ExportFence {
            authority,
            epoch: w.epoch,
            revision,
        },
    ))
}
async fn spool(
    db: &mut Metadata,
    store: &ManagedStore,
    read: &SourceExport,
    mut file: File,
    permit: Arc<OwnedSemaphorePermit>,
) -> Result<File> {
    file.write_all(&[0; archive::HEADER]).map_err(invalid)?;
    let root = read.root().id();
    let mut pending = BTreeMap::from([(root, read.root())]);
    let mut graph = archive::Graph::new();
    let mut bytes = 0u64;
    let mut edges = 0usize;
    while !pending.is_empty() {
        let nodes: Vec<_> = pending.values().take(MAX_READ_OBJECTS).cloned().collect();
        let batch = db
            .read_source_export(read, store, &nodes, MAX_READ_BYTES)
            .await
            .map_err(metadata)?;
        let hold = permit.clone();
        let (next, objects, written) = tokio::task::spawn_blocking(move || {
            let _permit = hold;
            let objects = batch.read().map_err(metadata)?;
            let mut written = bytes;
            for object in &objects {
                let frame = object.object().encode().map_err(invalid)?;
                written = written
                    .checked_add(frame.len() as u64)
                    .ok_or(fail(413, "source_export_limit"))?;
                if written > archive::MAX_BYTES {
                    return Err(fail(413, "source_export_limit"));
                }
                archive::record(&mut file, object.id(), &frame).map_err(invalid)?;
            }
            Ok::<_, Failure>((file, objects, written))
        })
        .await
        .map_err(|_| fail(500, "source_worker_failed"))??;
        file = next;
        bytes = written;
        for object in objects {
            let id = object.id();
            pending.remove(&id);
            if id == root
                && !matches!(object.object(),Object::NativeCommit(c) if c.snapshot==read.snapshot())
            {
                return Err(fail(500, "source_export_invalid"));
            }
            let refs = object.object().references();
            edges += refs.len();
            if edges > 2_000_000 {
                return Err(fail(413, "source_export_limit"));
            }
            graph.insert(id, (object.object().kind(), refs));
            for child in object.children() {
                if !graph.contains_key(&child.id()) {
                    pending.entry(child.id()).or_insert(child);
                }
            }
            if graph.len() + pending.len() > archive::MAX_OBJECTS {
                return Err(fail(413, "source_export_limit"));
            }
        }
    }
    // Validate duplicate edge types, cycles and exact closure before HTTP success.
    archive::order(&graph, root).map_err(invalid)?;
    file.rewind().map_err(invalid)?;
    archive::header(&mut file, root, read.snapshot(), graph.len(), bytes).map_err(invalid)?;
    file.rewind().map_err(invalid)?;
    Ok(file)
}
pub(crate) struct Delivery {
    file: tokio::fs::File,
    remaining: u64,
    buffer: Vec<u8>,
    _permit: Arc<OwnedSemaphorePermit>,
}
impl Body for Delivery {
    type Data = Bytes;
    type Error = std::io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<std::result::Result<Frame<Bytes>, Self::Error>>> {
        let this = self.get_mut();
        if this.remaining == 0 {
            return Poll::Ready(None);
        }
        let n = this.buffer.len().min(this.remaining as usize);
        let mut buf = ReadBuf::new(&mut this.buffer[..n]);
        match Pin::new(&mut this.file).poll_read(cx, &mut buf) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => {
                this.remaining = 0;
                Poll::Ready(Some(Err(e)))
            }
            Poll::Ready(Ok(())) => {
                if buf.filled().is_empty() {
                    this.remaining = 0;
                    return Poll::Ready(Some(Err(std::io::Error::from(
                        std::io::ErrorKind::UnexpectedEof,
                    ))));
                }
                let bytes = Bytes::copy_from_slice(buf.filled());
                this.remaining -= bytes.len() as u64;
                Poll::Ready(Some(Ok(Frame::data(bytes))))
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        self.remaining == 0
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(self.remaining)
    }
}
pub(crate) async fn export(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
    projection: bool,
) -> Result<Delivery> {
    let (scope, purpose) = if projection {
        projection_request(body)?
    } else {
        let (s, f) = parse(body)?;
        (s, Purpose::Source(f))
    };
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(
            credential,
            &scope,
            if projection {
                Permission::HostPublish
            } else {
                Permission::Read
            },
        )
        .await
        .map_err(metadata)?;
    let permit = Arc::new(
        backend
            .read_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(503, "source_read_busy"))?,
    );
    let opening = permit.clone();
    let tenant = TenantId::new(scope.tenant()).map_err(invalid)?;
    let (store, file) = tokio::task::spawn_blocking(move || {
        let _permit = opening;
        let file = tempfile::tempfile_in(&backend.config.root).map_err(invalid)?;
        let store = ManagedStore::open(backend.config.root, tenant, &backend.config.backend)
            .map_err(invalid)?;
        Ok::<_, Failure>((store, file))
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))??;
    let read = match purpose {
        Purpose::Source(fence) => db.metadata.open_source_export(&auth, fence, 120).await,
        Purpose::Projection { epoch, projection } => {
            db.metadata
                .open_github_projection_export(&auth, epoch, projection, 120)
                .await
        }
    }
    .map_err(metadata)?;
    let result = spool(&mut db.metadata, &store, &read, file, permit.clone()).await;
    db.metadata
        .finish_source_export(read)
        .await
        .map_err(metadata)?;
    let file = result?;
    let remaining = file.metadata().map_err(invalid)?.len();
    if remaining > archive::MAX_ARCHIVE {
        return Err(fail(413, "source_export_limit"));
    }
    Ok(Delivery {
        file: tokio::fs::File::from_std(file),
        remaining,
        buffer: vec![0; 64 * 1024],
        _permit: permit,
    })
}

#[cfg(test)]
mod projection_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn projection_export_accepts_only_an_exact_operation_not_an_arbitrary_root() {
        let valid = json!({"version":1,"tenant":"test","repository":"00112233-4455-6677-8899-aabbccddeeff","epoch":1,"projection":"ffeeddcc-bbaa-9988-7766-554433221100"});
        assert!(projection_request(&serde_json::to_vec(&valid).unwrap()).is_ok());
        for field in ["revision", "authority", "token", "key"] {
            let mut invalid = valid.clone();
            invalid[field] = json!("forged");
            assert!(projection_request(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        for (field, value) in [
            ("version", json!(2)),
            ("epoch", json!(0)),
            ("projection", json!(Uuid::nil().to_string())),
            ("repository", json!("00112233445566778899aabbccddeeff")),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            assert!(projection_request(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
}
