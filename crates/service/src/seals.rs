//! Acceptance of a ready managed capture; callers provide no object/result bytes.
use crate::{
    database::Database,
    source::{Backend, Failure},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use treeharbor_metadata::{
    Authority, Scope,
    access::{Permission, SecretToken},
    seals::{SealAcceptCommand, SealAcceptStart},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;
type Result<T> = std::result::Result<T, Failure>;
fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}
fn meta(e: treeharbor_metadata::Error) -> Failure {
    crate::source_read::metadata(e)
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
    seal: String,
    changeset: String,
    previous_version: i64,
    ttl_seconds: i32,
}
fn uuid(s: &str) -> Result<Uuid> {
    let id: Uuid = s.parse().map_err(|_| fail(400, "invalid_request"))?;
    if id.is_nil() || id.to_string() != s {
        return Err(fail(400, "invalid_request"));
    }
    Ok(id)
}
fn parse(bytes: &[u8]) -> Result<(Scope, SealAcceptCommand)> {
    let w: Wire = serde_json::from_slice(bytes).map_err(|_| fail(400, "invalid_request"))?;
    if w.version != 1
        || w.epoch <= 0
        || w.previous_version < 0
        || w.previous_version == i64::MAX
        || !(1..=3600).contains(&w.ttl_seconds)
    {
        return Err(fail(400, "invalid_request"));
    }
    Ok((
        Scope::new(w.tenant, uuid(&w.repository)?).map_err(|_| fail(400, "invalid_request"))?,
        SealAcceptCommand {
            key: w.key,
            authority: match w.authority.as_str() {
                "standalone" => Authority::Standalone,
                "connected" => Authority::Connected,
                _ => return Err(fail(400, "invalid_request")),
            },
            epoch: w.epoch,
            seal: uuid(&w.seal)?,
            changeset: uuid(&w.changeset)?,
            previous_version: w.previous_version,
            ttl_seconds: w.ttl_seconds,
        },
    ))
}
pub(crate) async fn accept(
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
        .open_seal_acceptance(&auth, &command)
        .await
        .map_err(meta)?
    {
        SealAcceptStart::Replay(v) => return Ok(json!({"version":1,"result":v})),
        SealAcceptStart::Ready(r) => r,
    };
    let outcome = async {
        let permit = Arc::new(
            backend
                .permits
                .clone()
                .try_acquire_owned()
                .map_err(|_| fail(503, "proposal_busy"))?,
        );
        let tenant =
            TenantId::new(scope.tenant()).map_err(|_| fail(500, "proposal_object_error"))?;
        let config = backend.config;
        let hold = permit.clone();
        let store = tokio::task::spawn_blocking(move || {
            let _permit = hold;
            ManagedStore::open(config.root, tenant, &config.backend)
        })
        .await
        .map_err(|_| fail(500, "proposal_worker_failed"))?
        .map_err(|_| fail(500, "proposal_object_error"))?;
        let (applied, objects) =
            crate::proposals::compute_seal(&mut db.metadata, &store, &read, permit.clone()).await?;
        let mut operations = None;
        let mut snapshot = None;
        for object in objects {
            let lease = db
                .metadata
                .reserve_seal_accept_object(&read, &store, &object)
                .await
                .map_err(meta)?;
            if lease.requires_upload() {
                let hold = permit.clone();
                let store = store.clone();
                let generation = lease.key().generation();
                let receipt = tokio::task::spawn_blocking(move || {
                    let _permit = hold;
                    store.put(generation, &object)
                })
                .await
                .map_err(|_| fail(500, "proposal_worker_failed"))?
                .map_err(|_| fail(500, "proposal_object_error"))?;
                db.metadata
                    .publish_object(&lease, &receipt)
                    .await
                    .map_err(meta)?;
            }
            if lease.key().id() == applied.operations() {
                operations = Some(lease);
            } else if lease.key().id() == applied.result_snapshot() {
                snapshot = Some(lease);
            }
        }
        let operations = operations.ok_or(fail(500, "proposal_object_error"))?;
        let snapshot = snapshot.ok_or(fail(500, "proposal_object_error"))?;
        let value = db
            .metadata
            .finish_seal_acceptance(&read, &applied, &operations, &snapshot)
            .await
            .map_err(meta)?;
        let _ = db.metadata.release_object_hold(&operations).await;
        let _ = db.metadata.release_object_hold(&snapshot).await;
        Ok(value)
    }
    .await;
    let _ = db.metadata.close_seal_acceptance(*read).await;
    outcome.map(|v| json!({"version":1,"result":v}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acceptance_wire_rejects_forged_identity_results_and_ambiguous_versions() {
        let valid = json!({"version":1,"tenant":"test","repository":Uuid::from_u128(1).to_string(),
            "key":"accept","authority":"standalone","epoch":1,"seal":Uuid::from_u128(2).to_string(),
            "changeset":Uuid::from_u128(3).to_string(),"previous_version":0,"ttl_seconds":300});
        assert!(parse(valid.to_string().as_bytes()).is_ok());
        for (field, value) in [
            ("principal", json!("admin")),
            ("operations", json!([])),
            ("result_snapshot", json!("forged")),
            ("version", json!(2)),
            ("epoch", json!(0)),
            ("previous_version", json!(-1)),
            ("previous_version", json!(i64::MAX)),
            ("ttl_seconds", json!(0)),
            ("ttl_seconds", json!(3601)),
            ("seal", json!(Uuid::nil().to_string())),
            ("repository", json!("00000000000000000000000000000001")),
        ] {
            let mut changed = valid.clone();
            changed[field] = value;
            assert!(parse(changed.to_string().as_bytes()).is_err(), "{field}");
        }
        let duplicate = valid.to_string().replacen('{', "{\"version\":1,", 1);
        assert!(parse(duplicate.as_bytes()).is_err());
    }
}
