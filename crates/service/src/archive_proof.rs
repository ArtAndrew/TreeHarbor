//! Trusted server-side verification of a retained archive source set.
use crate::{
    database::Database,
    source::{Backend, Failure, metadata},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Instant;
use treeharbor_git_bridge::Error as GitError;
use treeharbor_metadata::{
    Scope,
    access::{Permission, SecretToken},
    github::{ArchiveProofWork, Command},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

pub(crate) const PATH: &str = "/v1/github/archive/verify";

fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    epoch: i64,
    stage_key: String,
    key: String,
}

fn parse(body: &[u8]) -> Result<(Scope, Command, String), Failure> {
    let invalid = || fail(400, "invalid_request");
    let w: Wire = serde_json::from_slice(body).map_err(|_| invalid())?;
    let repo: Uuid = w.repository.parse().map_err(|_| invalid())?;
    if w.version != 1 || w.epoch <= 0 || repo.is_nil() || repo.to_string() != w.repository {
        return Err(invalid());
    }
    Ok((
        Scope::new(w.tenant, repo).map_err(|_| invalid())?,
        Command {
            key: w.key,
            epoch: w.epoch,
        },
        w.stage_key,
    ))
}

pub(crate) async fn verify(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
    deadline: Instant,
) -> Result<Value, Failure> {
    let (scope, command, stage_key) = parse(body)?;
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
    let retained = match db
        .metadata
        .prepare_archive_proof(&auth, &command, &stage_key)
        .await
        .map_err(metadata)?
    {
        ArchiveProofWork::Replay(result) => return Ok(json!({"version":1,"result":result})),
        ArchiveProofWork::Verify(retained) => retained,
    };
    drop(db);
    let config = backend.config;
    let tenant = TenantId::new(scope.tenant()).map_err(|_| fail(400, "invalid_request"))?;
    let proof = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let store = ManagedStore::open(&config.root, tenant, &config.backend)
            .map_err(|_| fail(500, "source_mapping_storage"))?;
        retained
            .verify(&store, &config.root, deadline)
            .map_err(|error| match error {
                GitError::Limit => fail(413, "source_mapping_limit"),
                GitError::Io(_) | GitError::Configuration => fail(500, "source_mapping_storage"),
                _ => fail(409, "source_mapping_mismatch"),
            })
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))??;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let result = db
        .metadata
        .record_archive_proof(&auth, &command, proof)
        .await
        .map_err(metadata)?;
    Ok(json!({"version":1,"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_proof_wire_accepts_only_a_staged_identity() {
        let bytes = include_bytes!("../tests/fixtures/archive-proof-request-v1.json");
        let (scope, command, stage) =
            parse(bytes).unwrap_or_else(|_| panic!("expected v1 fixture"));
        assert_eq!(scope.tenant(), "example");
        assert_eq!(command.epoch, 4);
        assert_eq!(command.key, "prove-001");
        assert_eq!(stage, "archive-11111111-1111-4111-8111-111111111111-stage");
        let fixture: Value = serde_json::from_slice(bytes).unwrap();
        for key in ["branches", "generation", "verified", "source_path"] {
            let mut invalid = fixture.clone();
            invalid[key] = json!(true);
            assert!(parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
}
