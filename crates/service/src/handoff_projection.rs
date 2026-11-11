//! Verify a frozen GitHub projection using the managed native store.
use crate::{
    archive_retention::fail,
    database::Database,
    source::{Backend, Failure, metadata},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;
use treeharbor_metadata::{
    Scope,
    access::{Permission, SecretToken},
    integration::{Command, HandoffProjectionSpec, HandoffProjectionWork},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

pub(crate) const PATH: &str = "/v1/native/handoff/verify-projection";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    epoch: i64,
    key: String,
    specification: HandoffProjectionSpec,
}

fn parse(body: &[u8]) -> Result<(Scope, Command, HandoffProjectionSpec), Failure> {
    let invalid = || fail(400, "invalid_request");
    let wire: Wire = serde_json::from_slice(body).map_err(|_| invalid())?;
    let repository: Uuid = wire.repository.parse().map_err(|_| invalid())?;
    if wire.version != 1
        || wire.epoch <= 0
        || repository.is_nil()
        || repository.to_string() != wire.repository
    {
        return Err(invalid());
    }
    Ok((
        Scope::new(wire.tenant, repository).map_err(|_| invalid())?,
        Command {
            key: wire.key,
            epoch: wire.epoch,
        },
        wire.specification,
    ))
}

pub(crate) async fn verify(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
    deadline: Instant,
) -> Result<Value, Failure> {
    let (scope, command, spec) = parse(body)?;
    let permit = backend
        .read_permits
        .try_acquire_owned()
        .map_err(|_| fail(503, "source_read_busy"))?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &scope, Permission::RepositoryWrite)
        .await
        .map_err(metadata)?;
    let work = match db
        .metadata
        .prepare_handoff_projection(&auth, &command, &spec, deadline)
        .await
        .map_err(metadata)?
    {
        HandoffProjectionWork::Replay(result) => return Ok(json!({"version":1,"result":result})),
        HandoffProjectionWork::Verify(work) => work,
    };
    drop(db);
    let config = backend.config;
    let tenant = TenantId::new(scope.tenant()).map_err(|_| fail(400, "invalid_request"))?;
    let proof = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let store = ManagedStore::open(&config.root, tenant, &config.backend)
            .map_err(|_| fail(500, "handoff_storage_failed"))?;
        work.verify(&store, &config.root, deadline)
            .map_err(metadata)
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))??;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let result = db
        .metadata
        .record_handoff_projection(&auth, &command, proof)
        .await
        .map_err(metadata)?;
    Ok(json!({"version":1,"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_projection_matches_v1_wire() {
        let raw = include_bytes!("../tests/fixtures/native-handoff-projection-v1.json");
        let (scope, command, specification) =
            parse(raw).unwrap_or_else(|_| panic!("invalid projection vector"));
        assert_eq!(scope.tenant(), "fixture");
        assert_eq!(
            scope.repository().to_string(),
            "00112233-4455-6677-8899-aabbccddeeff"
        );
        assert_eq!(command.epoch, 4);
        assert_eq!(command.key, "fixture-frozen-projection");
        let mut wire: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(json!(specification), wire["specification"]);
        wire["specification"]["validated"] = json!(true);
        assert!(parse(&serde_json::to_vec(&wire).unwrap()).is_err());
    }
}
