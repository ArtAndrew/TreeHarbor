//! Durable ChangeSet rebase on scoped immutable inputs. No ref movement or CI reuse.
use crate::{
    database::Database,
    source::{Backend, Failure},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::OwnedSemaphorePermit;
use treeharbor_merge::{Limits, PreparedRebase, RebaseOutcome, prepare_rebase};
use treeharbor_metadata::{
    Metadata, Scope,
    access::{Authorization, Permission, SecretToken},
    integration::Command,
    rebases::{RebaseLeases, RebaseRead},
};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
type Permit = Arc<OwnedSemaphorePermit>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn meta(e: treeharbor_metadata::Error) -> Failure {
    crate::source_read::metadata(e)
}
fn core(e: treeharbor_core::Error) -> Failure {
    match e {
        treeharbor_core::Error::TooLarge => fail(413, "rebase_limit"),
        _ => fail(500, "rebase_objects"),
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
    rebase: String,
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
        uuid(&w.rebase)?,
    ))
}
enum Computed {
    Ready(Box<PreparedRebase>, Vec<PreparedObject>),
    Rejected(&'static str, bool, Value),
}
#[allow(clippy::too_many_arguments)]
async fn execute(
    db: &mut Metadata,
    auth: &Authorization,
    cmd: &Command,
    id: Uuid,
    read: &RebaseRead,
    store: &ManagedStore,
    permit: Permit,
) -> Result<Value> {
    let scratch = crate::native::rebase_source(db, store, read, permit.clone()).await?;
    let input = read.input().clone();
    let hold = permit.clone();
    let computed = tokio::task::spawn_blocking(move || {
        let _permit = hold;
        match prepare_rebase(&scratch, &input, Limits::default()) {
            Ok(RebaseOutcome::Ready(c)) => {
                let objects = scratch
                    .publications(&[
                        c.applied().operations(),
                        c.applied().result_snapshot(),
                        c.revision(),
                    ])
                    .map_err(core)?;
                Ok(Computed::Ready(c, objects))
            }
            Ok(RebaseOutcome::Conflicted(c)) => Ok(Computed::Rejected(
                "merge_conflict",
                true,
                crate::native::conflicts(&c),
            )),
            Err(treeharbor_merge::Error::Unsupported(_)) => {
                Ok(Computed::Rejected("merge_unsupported", false, json!({})))
            }
            Err(
                treeharbor_merge::Error::Limit(_)
                | treeharbor_merge::Error::Core(treeharbor_core::Error::TooLarge),
            ) => Ok(Computed::Rejected("merge_limit", false, json!({}))),
            Err(_) => Err(fail(500, "change_rebase_objects")),
        }
    })
    .await
    .map_err(|_| fail(500, "rebase_worker_failed"))??;
    let (computed, objects) = match computed {
        Computed::Rejected(reason, conflict, details) => {
            return db
                .finish_change_rebase_failure(auth, cmd, id, reason, conflict, details)
                .await
                .map_err(meta);
        }
        Computed::Ready(c, objects) => (c, objects),
    };
    let mut leases = BTreeMap::new();
    for object in objects {
        let lease = db
            .reserve_change_rebase_object(auth, cmd.epoch, id, store, &object)
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
            .map_err(|_| fail(500, "rebase_worker_failed"))?
            .map_err(core)?;
            db.publish_object(&lease, &durable).await.map_err(meta)?;
        }
        leases.insert(lease.key().id(), lease);
    }
    let get = |id| leases.get(&id).ok_or(fail(500, "rebase_objects"));
    let roots = RebaseLeases {
        operations: get(computed.applied().operations())?,
        result: get(computed.applied().result_snapshot())?,
        revision: get(computed.revision())?,
    };
    let result = db
        .finish_change_rebase(auth, cmd, id, &computed, &roots)
        .await
        .map_err(meta);
    if result.is_ok() {
        for lease in leases.values() {
            let _ = db.release_object_hold(lease).await;
        }
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
        .authorize(credential, &scope, Permission::ChangeWrite)
        .await
        .map_err(meta)?;
    if let Some(value) = db
        .metadata
        .change_rebase_replay(&auth, &cmd, id)
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
            .map_err(|_| fail(503, "change_rebase_busy"))?,
    );
    let read = db
        .metadata
        .open_change_rebase(&auth, cmd.epoch, id)
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
        .map_err(|_| fail(500, "rebase_worker_failed"))?
        .map_err(core)?;
        execute(&mut db.metadata, &auth, &cmd, id, &read, &store, permit).await
    }
    .await;
    let result = match result {
        Err(failure) if matches!(failure.status, 413 | 429) => db
            .metadata
            .finish_change_rebase_failure(&auth, &cmd, id, "rebase_limit", false, json!({}))
            .await
            .map_err(meta),
        other => other,
    };
    let _ = db.metadata.close_change_rebase_read(read).await;
    result.map(|v| json!({"version":1,"result":v}))
}
