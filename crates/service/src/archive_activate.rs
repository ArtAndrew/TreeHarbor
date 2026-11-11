//! Local endpoint for the final authority transaction.
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
    github::{ArchiveActivateSpec, ArchiveActivateWork, Command},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

pub(crate) const PATH: &str = "/v1/github/archive/activate";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    epoch: i64,
    key: String,
    specification: ArchiveActivateSpec,
}

fn parse(body: &[u8]) -> Result<(Scope, Command, ArchiveActivateSpec), Failure> {
    let invalid = || fail(400, "invalid_request");
    let wire: Wire = serde_json::from_slice(body).map_err(|_| invalid())?;
    let id: Uuid = wire.repository.parse().map_err(|_| invalid())?;
    if wire.version != 1 || wire.epoch <= 0 || id.is_nil() || id.to_string() != wire.repository {
        return Err(invalid());
    }
    Ok((
        Scope::new(wire.tenant, id).map_err(|_| invalid())?,
        Command {
            key: wire.key,
            epoch: wire.epoch,
        },
        wire.specification,
    ))
}

pub(crate) async fn activate(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
    deadline: Instant,
) -> Result<Value, Failure> {
    let (scope, command, specification) = parse(body)?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &scope, Permission::RepositoryWrite)
        .await
        .map_err(metadata)?;
    let policy_auth = db
        .metadata
        .authorize(credential, &scope, Permission::PolicyWrite)
        .await
        .map_err(metadata)?;
    let work = match db
        .metadata
        .prepare_archive_activation(&auth, &policy_auth, &command, &specification)
        .await
        .map_err(metadata)?
    {
        ArchiveActivateWork::Replay(result) => return Ok(json!({"version":1,"result":result})),
        ArchiveActivateWork::Verify(work) => work,
    };
    drop(db);
    let permit = backend
        .read_permits
        .try_acquire_owned()
        .map_err(|_| fail(503, "source_read_busy"))?;
    let tenant = TenantId::new(scope.tenant()).map_err(|_| fail(400, "invalid_request"))?;
    let proof = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let config = backend.config;
        let store = ManagedStore::open(&config.root, tenant, &config.backend)
            .map_err(|_| fail(500, "archive_storage_failed"))?;
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
        .activate_archive(&auth, &policy_auth, &command, &specification, proof)
        .await
        .map_err(metadata)?;
    Ok(json!({"version":1,"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use treeharbor_core::ObjectId;

    #[test]
    fn archive_activation_matches_v1_request_vector() {
        let raw = include_bytes!("../tests/fixtures/github-archive-activate-v1.json");
        let (scope, command, spec) =
            parse(raw).unwrap_or_else(|_| panic!("invalid activation vector"));
        let wire: Value = serde_json::from_slice(raw).unwrap();
        assert_eq!(scope.tenant(), wire["tenant"]);
        assert_eq!(scope.repository().to_string(), wire["repository"]);
        assert_eq!(command.key, "fixture-archive-activate");
        assert_eq!(command.epoch, wire["epoch"].as_i64().unwrap());
        assert_eq!(json!(spec), wire["specification"]);
        assert_eq!(
            ObjectId::from_bytes(spec.policy.digest().unwrap()).to_string(),
            "sha256:c5f34244af8e79a1752d7b515c5f845ff0565640f0693c938dadcccf6ddac6ca"
        );
        let mut invalid = wire;
        invalid["specification"]["checks_already_passed"] = json!(true);
        assert!(parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}
