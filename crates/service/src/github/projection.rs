//! Prepare one server-derived commit. No caller-supplied object bytes or GitHub IO.
use crate::{
    database::Database,
    source::{Backend, Failure},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use treeharbor_metadata::{
    Scope,
    access::{Permission, SecretToken},
    github::Command,
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    key: String,
    epoch: i64,
    projection: String,
}
pub(crate) async fn prepare(
    bytes: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
) -> Result<Value> {
    let w: Wire = serde_json::from_slice(bytes).map_err(|_| fail(400, "invalid_request"))?;
    let canonical = |s: &str| -> Result<Uuid> {
        let id: Uuid = s.parse().map_err(|_| fail(400, "invalid_request"))?;
        if id.is_nil() || id.to_string() != s {
            return Err(fail(400, "invalid_request"));
        }
        Ok(id)
    };
    if w.version != 1 || w.epoch <= 0 {
        return Err(fail(400, "invalid_request"));
    }
    let scope = Scope::new(w.tenant, canonical(&w.repository)?)
        .map_err(|_| fail(400, "invalid_request"))?;
    let projection = canonical(&w.projection)?;
    let cmd = Command {
        key: w.key,
        epoch: w.epoch,
    };
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &scope, Permission::RepositoryWrite)
        .await
        .map_err(crate::source::metadata)?;
    if let Some(v) = db
        .metadata
        .github_projection_preparation_replay(&auth, &cmd, projection)
        .await
        .map_err(crate::source::metadata)?
    {
        return Ok(json!({"version":1,"result":v}));
    }
    let permit = Arc::new(
        backend
            .permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(503, "projection_busy"))?,
    );
    let hold = permit.clone();
    let tenant = TenantId::new(scope.tenant()).map_err(|_| fail(500, "projection_objects"))?;
    let store = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        ManagedStore::open(backend.config.root, tenant, &backend.config.backend)
    })
    .await
    .map_err(|_| fail(500, "projection_worker_failed"))?
    .map_err(|_| fail(500, "projection_objects"))?;
    let (object, lease) = db
        .metadata
        .reserve_github_projection(&auth, cmd.epoch, projection, &store)
        .await
        .map_err(crate::source::metadata)?;
    drop(db);
    let generation = lease.key().generation();
    let hold = permit.clone();
    let receipt = tokio::task::spawn_blocking(move || {
        let _hold = hold;
        store.put(generation, &object)
    })
    .await
    .map_err(|_| fail(500, "projection_worker_failed"))?
    .map_err(|_| fail(500, "projection_objects"))?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let result = db
        .metadata
        .finish_github_projection(&auth, &cmd, projection, &lease, &receipt)
        .await
        .map_err(crate::source::metadata)?;
    Ok(json!({"version":1,"result":result}))
}
