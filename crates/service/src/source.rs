//! Binary source-object upload adapter. Files and PostgreSQL remain separate
//! durability domains; only final authenticated publication creates membership.
use crate::{Error, Result, config::StorageConfig, database::Database, files, source_batch};
use deadpool::managed::Pool;
use http_body_util::{BodyExt, Limited};
use hyper::{Request, body::Incoming};
use std::{fs, sync::Arc, time::Duration};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use treeharbor_core::{FRAME_HEADER, MAX_PAYLOAD, ObjectId};
use treeharbor_metadata::{
    Scope,
    access::{Authorization, Permission, SecretToken},
    commands::CommandError,
    uploads::Purpose,
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;

#[derive(Clone)]
pub(crate) struct Backend {
    pub config: StorageConfig,
    pub permits: Arc<Semaphore>,
    pub read_permits: Arc<Semaphore>,
}
impl Backend {
    pub async fn open(mut config: StorageConfig, pool: &Pool<Database>) -> Result<Self> {
        config.root = files::private_directory(&config.root)?;
        let marker = config.root.join("backend-id");
        if !marker.try_exists()? {
            if fs::read_dir(&config.root)?.next().is_some() {
                return Err(Error::Config);
            }
            let id = Uuid::new_v4().to_string();
            if let Err(e) = files::write_private(&marker, id.as_bytes()) {
                // Another startup may have won the no-clobber creation.
                if !marker.try_exists()? {
                    return Err(e);
                }
            }
        }
        let bytes = files::read_private(&marker, 36)?;
        let id: Uuid = std::str::from_utf8(&bytes)
            .map_err(|_| Error::Config)?
            .parse()
            .map_err(|_| Error::Config)?;
        if id.to_string().as_bytes() != bytes {
            return Err(Error::Config);
        }
        let mut db = pool.get().await.map_err(|_| Error::Database)?;
        db.metadata
            .bind_storage_backend(&config.backend, id)
            .await
            .map_err(|_| Error::Config)?;
        Ok(Self {
            config,
            permits: Arc::new(Semaphore::new(2)),
            read_permits: Arc::new(Semaphore::new(2)),
        })
    }
}
pub(crate) struct Route {
    scope: Scope,
    upload: Uuid,
    object: Option<ObjectId>,
    change: bool,
    github: bool,
}
impl Route {
    pub(crate) fn is_change(&self) -> bool {
        self.change
    }
}
pub(crate) fn route(path: &str) -> Option<Route> {
    let parts: Vec<_> = path.split('/').collect();
    let (purpose, tenant, repo, upload, object) = match parts.as_slice() {
        [
            "",
            "v1",
            purpose @ ("source" | "changes" | "github"),
            tenant,
            repo,
            "uploads",
            upload,
            "objects",
            object,
        ] => (
            *purpose,
            *tenant,
            *repo,
            *upload,
            Some(object.parse().ok()?),
        ),
        [
            "",
            "v1",
            purpose @ ("source" | "github"),
            tenant,
            repo,
            "uploads",
            upload,
            "blobs",
        ] => (*purpose, *tenant, *repo, *upload, None),
        _ => return None,
    };
    let repository: Uuid = repo.parse().ok()?;
    let upload_id: Uuid = upload.parse().ok()?;
    if repository.to_string() != repo || upload_id.to_string() != upload {
        return None;
    }
    Some(Route {
        scope: Scope::new(tenant, repository).ok()?,
        upload: upload_id,
        object,
        change: purpose == "changes",
        github: purpose == "github",
    })
}
pub(crate) struct Failure {
    pub status: u16,
    pub code: &'static str,
}
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
pub(crate) fn metadata(error: treeharbor_metadata::Error) -> Failure {
    let code = CommandError::Metadata(error).code();
    let status = match code {
        "invalid_request" => 400,
        "unauthenticated" => 401,
        "forbidden" => 403,
        "not_found" => 404,
        "upload_limit" => 429,
        "repository_fenced"
        | "native_handoff_busy"
        | "native_handoff_mismatch"
        | "execution_state_changed"
        | "rebase_state_changed"
        | "version_conflict"
        | "host_binding_state"
        | "archive_observation_mismatch"
        | "archive_artifact_mismatch"
        | "archive_activation_busy"
        | "archive_history_diverged"
        | "host_recovery_required"
        | "restore_context_changed"
        | "restore_decision_conflict"
        | "restore_context_limit"
        | "host_projection_state"
        | "host_publication_state"
        | "host_inspection_state"
        | "host_queue_state"
        | "host_pull_state"
        | "idempotency_conflict"
        | "upload_state_changed"
        | "upload_membership"
        | "object_busy"
        | "lease_expired" => 409,
        _ => 500,
    };
    fail(status, code)
}
pub(crate) async fn put(
    request: Request<Incoming>,
    credential: &SecretToken,
    route: Route,
    backend: Backend,
    pool: Pool<Database>,
) -> std::result::Result<serde_json::Value, Failure> {
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(
            credential,
            &route.scope,
            if route.change {
                Permission::ChangeWrite
            } else if route.github {
                Permission::HostRead
            } else {
                Permission::RepositoryWrite
            },
        )
        .await
        .map_err(metadata)?;
    // Refuse unknown/foreign sessions before consuming a large body.
    let state = if route.change {
        db.metadata.change_upload_status(&auth, route.upload).await
    } else if route.github {
        db.metadata
            .github_source_upload_status(&auth, route.upload)
            .await
    } else {
        db.metadata.source_upload_status(&auth, route.upload).await
    }
    .map_err(metadata)?;
    if state["state"] != "active" || state["expired"] != false {
        return Err(fail(409, "upload_state_changed"));
    }
    drop(db);
    let permit = backend
        .permits
        .clone()
        .try_acquire_owned()
        .map_err(|_| fail(503, "upload_busy"))?;
    let limit = if route.object.is_some() {
        MAX_PAYLOAD + FRAME_HEADER
    } else {
        source_batch::MAX_BYTES
    };
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        Limited::new(request.into_body(), limit).collect(),
    )
    .await
    {
        Ok(Ok(body)) => body.to_bytes(),
        Ok(Err(e)) if e.is::<http_body_util::LengthLimitError>() => {
            return Err(fail(413, "request_too_large"));
        }
        Ok(Err(_)) => return Err(fail(400, "invalid_body")),
        Err(_) => return Err(fail(408, "body_timeout")),
    };
    let tenant = route.scope.tenant().to_owned();
    let config = backend.config;
    let (store, mut objects, permit) = tokio::task::spawn_blocking(move || {
        let objects = match route.object {
            Some(id) => vec![
                PreparedObject::from_frame(id, bytes.to_vec())
                    .map_err(|_| fail(400, "invalid_object"))?,
            ],
            None => {
                source_batch::decode(&bytes).ok_or_else(|| fail(400, "invalid_object_batch"))?
            }
        };
        let tenant = TenantId::new(&tenant).map_err(|_| fail(400, "invalid_request"))?;
        let store = ManagedStore::open(config.root, tenant, &config.backend)
            .map_err(|_| fail(500, "storage_error"))?;
        Ok::<_, Failure>((store, objects, permit))
    })
    .await
    .map_err(|_| fail(500, "storage_error"))??;
    if route.object.is_none() {
        return put_blobs(route, auth, store, objects, permit, pool).await;
    }
    let object = objects.pop().ok_or_else(|| fail(500, "storage_error"))?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    enum Ticket {
        Source(treeharbor_metadata::uploads::UploadTicket),
        Change(treeharbor_metadata::proposals::ChangeBlobTicket),
        Github(treeharbor_metadata::uploads::UploadTicket),
    }
    let ticket = if route.change {
        Ticket::Change(
            db.metadata
                .reserve_change_blob(&auth, route.upload, &store, &object)
                .await
                .map_err(metadata)?,
        )
    } else if route.github {
        Ticket::Github(
            db.metadata
                .reserve_github_source_object(&auth, route.upload, &store, &object)
                .await
                .map_err(metadata)?,
        )
    } else {
        Ticket::Source(
            db.metadata
                .reserve_source_object(&auth, route.upload, &store, &object)
                .await
                .map_err(metadata)?,
        )
    };
    drop(db);
    let generation = match &ticket {
        Ticket::Source(t) => t.generation(),
        Ticket::Github(t) => t.generation(),
        Ticket::Change(t) => t.generation(),
    };
    // Cancellation must not detach unbounded fsync workers: this owned permit
    // stays inside the blocking task until its IO actually stops.
    let (receipt, _permit) = tokio::task::spawn_blocking(move || {
        store
            .put(generation, &object)
            .map(|receipt| (receipt, permit))
            .map_err(|_| fail(500, "storage_error"))
    })
    .await
    .map_err(|_| fail(500, "storage_error"))??;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    match ticket {
        Ticket::Source(t) => db.metadata.publish_source_object(&auth, &t, &receipt).await,
        Ticket::Github(t) => {
            db.metadata
                .publish_github_source_object(&auth, &t, &receipt)
                .await
        }
        Ticket::Change(t) => db.metadata.publish_change_blob(&auth, &t, &receipt).await,
    }
    .map_err(metadata)?;
    Ok(
        serde_json::json!({"version":1,"result":{"object":receipt.id().to_string(),"state":"stored"}}),
    )
}

async fn put_blobs(
    route: Route,
    auth: Authorization,
    store: ManagedStore,
    objects: Vec<PreparedObject>,
    permit: OwnedSemaphorePermit,
    pool: Pool<Database>,
) -> std::result::Result<serde_json::Value, Failure> {
    let purpose = if route.github {
        Purpose::Github
    } else {
        Purpose::Human
    };
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let tickets = db
        .metadata
        .reserve_source_blob_batch(&auth, route.upload, &store, &objects, purpose)
        .await
        .map_err(metadata)?;
    drop(db);
    let (tickets, receipts, _permit) = tokio::task::spawn_blocking(move || {
        let receipts = tickets
            .iter()
            .zip(&objects)
            .map(|(ticket, object)| store.put(ticket.generation(), object))
            .collect::<treeharbor_core::Result<Vec<_>>>()
            .map_err(|_| fail(500, "storage_error"))?;
        Ok::<_, Failure>((tickets, receipts, permit))
    })
    .await
    .map_err(|_| fail(500, "storage_error"))??;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    db.metadata
        .publish_source_blob_batch(&auth, &tickets, &receipts, purpose)
        .await
        .map_err(metadata)?;
    let ids: Vec<_> = receipts
        .iter()
        .map(|receipt| receipt.id().to_string())
        .collect();
    Ok(serde_json::json!({"version":1,"result":{"objects":ids,"state":"stored"}}))
}
