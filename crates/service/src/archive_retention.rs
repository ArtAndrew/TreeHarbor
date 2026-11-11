//! Local owner-only archive retention after verified source and inventory capture.
use crate::{
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
    github::{ArchiveArtifactSpec, ArchiveRetentionWork},
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;

pub(crate) const PATH: &str = "/v1/github/archive/retain";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    tenant: String,
    repository: String,
    epoch: i64,
    upload: String,
    specification: ArchiveArtifactSpec,
}

fn fail(status: u16, code: &'static str) -> Failure {
    Failure { status, code }
}

fn parse(body: &[u8]) -> Result<(Scope, i64, Uuid, ArchiveArtifactSpec), Failure> {
    let invalid = || fail(400, "invalid_request");
    let wire: Wire = serde_json::from_slice(body).map_err(|_| invalid())?;
    let uuid = |raw: &str| -> Result<Uuid, Failure> {
        let id: Uuid = raw.parse().map_err(|_| invalid())?;
        if id.is_nil() || id.to_string() != raw {
            return Err(invalid());
        }
        Ok(id)
    };
    if wire.version != 1 || wire.epoch <= 0 {
        return Err(invalid());
    }
    Ok((
        Scope::new(wire.tenant, uuid(&wire.repository)?).map_err(|_| invalid())?,
        wire.epoch,
        uuid(&wire.upload)?,
        wire.specification,
    ))
}

pub(crate) async fn retain(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
    deadline: Instant,
) -> Result<Value, Failure> {
    let (scope, epoch, upload, spec) = parse(body)?;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let auth = db
        .metadata
        .authorize(credential, &scope, Permission::RepositoryWrite)
        .await
        .map_err(metadata)?;
    let work = db
        .metadata
        .prepare_archive_retention(&auth, epoch, upload, &spec)
        .await
        .map_err(metadata)?;
    let work = match work {
        ArchiveRetentionWork::Replay(result) => return Ok(json!({"version":1,"result":result})),
        ArchiveRetentionWork::Verify(work) => work,
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
        work.verify(&store, deadline).map_err(metadata)
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))??;
    let mut db = pool
        .get()
        .await
        .map_err(|_| fail(503, "database_unavailable"))?;
    let result = db
        .metadata
        .record_archive_retention(&auth, proof)
        .await
        .map_err(metadata)?;
    Ok(json!({"version":1,"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
    use treeharbor_core::ObjectId;
    use treeharbor_metadata::github::{ArchiveArtifactManifest, Upstream, pack_archive_artifact};
    use treeharbor_storage::LocalStore;

    #[test]
    fn archive_retention_matches_v1_chunk_and_request_vector() {
        let raw = include_bytes!("../tests/fixtures/github-archive-retain-v1.json");
        let (scope, epoch, upload, expected) =
            parse(raw).unwrap_or_else(|_| panic!("invalid vector"));
        assert_eq!(scope.tenant(), "fixture");
        assert_eq!(epoch, 4);
        assert_eq!(upload.to_string(), "11111111-1111-4111-8111-111111111111");
        let mut manifest: ArchiveArtifactManifest<Upstream> = serde_json::from_slice(
            include_bytes!("../tests/fixtures/github-archive-preservation-v1.json"),
        )
        .unwrap();
        let mut bytes = vec![b'x'; 8 * 1024 * 1024];
        bytes.extend(b"artifact-v1\0\xff");
        manifest.bundle.bytes = bytes.len() as u64;
        manifest.bundle.sha256 = ObjectId::from_bytes(Sha256::digest(&bytes).into()).hex();
        let manifest = serde_json::to_vec(&manifest).unwrap();
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let store = LocalStore::open(temp.path(), &TenantId::new(scope.tenant()).unwrap()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let packed =
            pack_archive_artifact(&store, manifest.clone(), bytes.as_slice(), deadline).unwrap();
        assert_eq!(packed.specification, expected);
        assert_eq!(packed.objects.len(), 4);
        assert!(matches!(
            pack_archive_artifact(&store, manifest, &bytes[..bytes.len() - 1], deadline),
            Err(treeharbor_metadata::Error::ArchiveArtifactMismatch)
        ));
        let mut invalid: Value = serde_json::from_slice(raw).unwrap();
        invalid["verified"] = json!(true);
        assert!(parse(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}
