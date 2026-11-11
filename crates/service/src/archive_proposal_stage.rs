//! Local owner-only endpoint for verifying and pinning prepared archive proposals.
use crate::{
    archive_retention::{fail, parse},
    database::Database,
    source::{Backend, Failure, metadata},
};
use deadpool::managed::Pool;
use serde_json::{Value, json};
use std::time::Instant;
use treeharbor_metadata::{
    access::{Permission, SecretToken},
    github::{ArchiveProposalStageSpec, ArchiveProposalStageWork},
};
use treeharbor_storage::{ManagedStore, TenantId};

pub(crate) const PATH: &str = "/v1/github/archive/proposals/stage";

pub(crate) async fn stage(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
    deadline: Instant,
) -> Result<Value, Failure> {
    let (scope, epoch, upload, spec) = parse::<ArchiveProposalStageSpec>(body)?;
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
        .prepare_archive_proposal_stage(&auth, epoch, upload, &spec)
        .await
        .map_err(metadata)?
    {
        ArchiveProposalStageWork::Replay(result) => {
            return Ok(json!({"version":1,"result":result}));
        }
        ArchiveProposalStageWork::Verify(work) => work,
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
        .record_archive_proposal_stage(&auth, proof)
        .await
        .map_err(metadata)?;
    Ok(json!({"version":1,"result":result}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use treeharbor_core::{Object, ObjectId};
    use treeharbor_metadata::github::{ArchiveProposalManifest, archive_proposals_fingerprint};

    #[test]
    fn archive_proposal_stage_matches_v1_wire_vector() {
        let raw = include_bytes!("../tests/fixtures/github-archive-proposal-stage-v1.json");
        let (scope, epoch, upload, spec) = parse::<ArchiveProposalStageSpec>(raw)
            .unwrap_or_else(|_| panic!("invalid proposal-stage vector"));
        let manifest = include_bytes!("../tests/fixtures/github-archive-proposals-v1.json")
            .strip_suffix(b"\n")
            .unwrap();
        let decoded: ArchiveProposalManifest = serde_json::from_slice(manifest).unwrap();
        let frame = Object::Blob(manifest.to_vec()).encode().unwrap();
        assert_eq!(scope.tenant(), decoded.observation.tenant);
        assert_eq!(scope.repository(), decoded.observation.repository);
        assert_eq!(epoch, decoded.observation.epoch);
        assert_eq!(upload.to_string(), "22222222-2222-4222-8222-222222222222");
        assert_eq!(
            spec.manifest_object,
            ObjectId::hash_frame(&frame).to_string()
        );
        assert_eq!(
            spec.manifest_sha256,
            archive_proposals_fingerprint(manifest).to_string()
        );
        assert_eq!(spec.object_count, decoded.objects.len() as i64 + 1);
        assert_eq!(
            spec.encoded_bytes,
            decoded.encoded_bytes as i64 + frame.len() as i64
        );
        assert_eq!(
            spec.key().unwrap(),
            decoded.observation.observation_key.replacen(
                "archive-observed-",
                "archive-proposals-",
                1
            )
        );
        let mut invalid: Value = serde_json::from_slice(raw).unwrap();
        invalid["specification"]["approved"] = json!(true);
        assert!(parse::<ArchiveProposalStageSpec>(&serde_json::to_vec(&invalid).unwrap()).is_err());
    }
}
