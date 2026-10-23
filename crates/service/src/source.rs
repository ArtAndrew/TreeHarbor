//! Binary source-object upload adapter. Files and PostgreSQL remain separate
//! durability domains; only final authenticated publication creates membership.
use crate::{Error, Result, config::StorageConfig, database::Database, files};
use deadpool::managed::Pool;
use http_body_util::{BodyExt, Limited};
use hyper::{Request, body::Incoming};
use std::{fs, sync::Arc, time::Duration};
use tokio::sync::Semaphore;
use treeharbor_core::{FRAME_HEADER, MAX_PAYLOAD, ObjectId};
use treeharbor_metadata::{
    Scope,
    access::{Permission, SecretToken},
    commands::CommandError,
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
    object: ObjectId,
    change: bool,
    github: bool,
}
pub(crate) fn route(path: &str) -> Option<Route> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 9
        || parts[1] != "v1"
        || !matches!(parts[2], "source" | "changes" | "github")
        || parts[5] != "uploads"
        || parts[7] != "objects"
    {
        return None;
    }
    let repo: Uuid = parts[4].parse().ok()?;
    let upload: Uuid = parts[6].parse().ok()?;
    if repo.to_string() != parts[4] || upload.to_string() != parts[6] {
        return None;
    }
    Some(Route {
        scope: Scope::new(parts[3], repo).ok()?,
        upload,
        object: parts[8].parse().ok()?,
        change: parts[2] == "changes",
        github: parts[2] == "github",
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
        | "execution_state_changed"
        | "rebase_state_changed"
        | "version_conflict"
        | "host_binding_state"
        | "host_projection_state"
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
    let bytes = match tokio::time::timeout(
        Duration::from_secs(5),
        Limited::new(request.into_body(), MAX_PAYLOAD + FRAME_HEADER).collect(),
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
    let (store, object, permit) = tokio::task::spawn_blocking(move || {
        let object = PreparedObject::from_frame(route.object, bytes.to_vec())
            .map_err(|_| fail(400, "invalid_object"))?;
        let tenant = TenantId::new(&tenant).map_err(|_| fail(400, "invalid_request"))?;
        let store = ManagedStore::open(config.root, tenant, &config.backend)
            .map_err(|_| fail(500, "storage_error"))?;
        Ok::<_, Failure>((store, object, permit))
    })
    .await
    .map_err(|_| fail(500, "storage_error"))??;
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
