//! Server-side verification of a recovery pin against an exact Git commit.
use crate::{
    archive,
    database::Database,
    source::{Backend, Failure},
};
use deadpool::managed::Pool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use treeharbor_core::RevisionRef;
use treeharbor_metadata::{
    Scope,
    access::{Permission, SecretToken},
    github::Command,
};
use treeharbor_storage::{ManagedStore, TenantId};
use uuid::Uuid;
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
    decision_key: String,
    git_revision: String,
}

fn parse(body: &[u8]) -> Result<(Wire, Scope, treeharbor_core::GitOid), Failure> {
    let w: Wire = serde_json::from_slice(body).map_err(|_| fail(400, "invalid_request"))?;
    let id: Uuid = w
        .repository
        .parse()
        .map_err(|_| fail(400, "invalid_request"))?;
    if w.version != 1 || w.epoch <= 0 || id.is_nil() || id.to_string() != w.repository {
        return Err(fail(400, "invalid_request"));
    }
    let expected: RevisionRef = w
        .git_revision
        .parse()
        .map_err(|_| fail(400, "invalid_request"))?;
    if expected.to_string() != w.git_revision {
        return Err(fail(400, "invalid_request"));
    }
    let RevisionRef::Git(expected) = expected else {
        return Err(fail(400, "invalid_request"));
    };
    let scope = Scope::new(w.tenant.clone(), id).map_err(|_| fail(400, "invalid_request"))?;
    Ok((w, scope, expected))
}

pub(crate) async fn verify(
    body: &[u8],
    credential: &SecretToken,
    backend: Backend,
    pool: Pool<Database>,
) -> Result<Value, Failure> {
    let (w, scope, expected) = parse(body)?;
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
    if let Some(receipt) = db
        .metadata
        .github_restore_mapping_replay(&auth, &cmd, &w.decision_key, &w.git_revision)
        .await
        .map_err(crate::source::metadata)?
    {
        return Ok(json!({"version":1,"result":receipt}));
    }
    let permit = Arc::new(
        backend
            .read_permits
            .clone()
            .try_acquire_owned()
            .map_err(|_| fail(503, "source_read_busy"))?,
    );
    let config = backend.config;
    let scratch_root = config.root.clone();
    let tenant = TenantId::new(scope.tenant()).map_err(|_| fail(400, "invalid_request"))?;
    let opening = permit.clone();
    let (store, file) = tokio::task::spawn_blocking(move || {
        let _permit = opening;
        let file =
            tempfile::tempfile_in(&config.root).map_err(|_| fail(500, "source_mapping_storage"))?;
        let store = ManagedStore::open(config.root, tenant, &config.backend)
            .map_err(|_| fail(500, "source_mapping_storage"))?;
        Ok::<_, Failure>((store, file))
    })
    .await
    .map_err(|_| fail(500, "source_worker_failed"))??;
    let read = db
        .metadata
        .open_restore_source_export(&auth, cmd.epoch, &w.decision_key, 120)
        .await
        .map_err(crate::source::metadata)?;
    let root = read.root().id();
    let spooled =
        crate::source_export::spool(&mut db.metadata, &store, &read, file, permit.clone()).await;
    let projected = match spooled {
        Err(e) => Err(e),
        Ok(mut file) => {
            let hold = permit.clone();
            tokio::task::spawn_blocking(move || {
                let _permit = hold;
                let scratch = tempfile::Builder::new()
                    .prefix(".treeharbor-restore-source-")
                    .permissions(std::fs::Permissions::from_mode(0o700))
                    .tempdir_in(scratch_root)
                    .map_err(|_| fail(500, "source_mapping_storage"))?;
                let pack = archive::receive(&mut file, root, &scratch.path().join("source"))
                    .map_err(|_| fail(500, "source_mapping_objects"))?;
                treeharbor_git_bridge::verify_mapping(
                    pack.store(),
                    root,
                    expected,
                    scratch.path(),
                    treeharbor_git_bridge::Limits {
                        timeout: Duration::from_secs(60),
                        ..Default::default()
                    },
                )
                .map_err(|e| match e {
                    treeharbor_git_bridge::Error::Limit => fail(413, "source_mapping_limit"),
                    treeharbor_git_bridge::Error::Io(_)
                    | treeharbor_git_bridge::Error::Configuration => {
                        fail(500, "source_mapping_storage")
                    }
                    _ => fail(409, "source_mapping_mismatch"),
                })
            })
            .await
            .unwrap_or_else(|_| Err(fail(500, "source_worker_failed")))
        }
    };
    // Close on computation failure too. On handler cancellation the read hold has
    // a TTL; the permanent decision root remains. Final recording rechecks fences.
    let closed = db
        .metadata
        .finish_source_export(read)
        .await
        .map_err(crate::source::metadata);
    let proof = projected?;
    closed?;
    let receipt = db
        .metadata
        .record_github_restore_mapping(&auth, &cmd, &w.decision_key, &w.git_revision, &proof)
        .await
        .map_err(crate::source::metadata)?;
    Ok(json!({"version":1,"result":receipt}))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn version_one_request_names_only_a_retained_decision_and_expected_git_revision() {
        let bytes = include_bytes!("../tests/fixtures/restore-source-request-v1.json");
        let (w, scope, git) = parse(bytes).unwrap_or_else(|_| panic!("valid vector"));
        assert_eq!(scope.tenant(), "example");
        assert_eq!(w.decision_key, "preserve-001");
        assert_eq!(RevisionRef::Git(git).to_string(), w.git_revision);
        let value: Value = serde_json::from_slice(bytes).unwrap();
        for field in [
            "native_revision",
            "source_holder",
            "git_mapping_verified",
            "proof",
            "git",
            "path",
        ] {
            let mut bad = value.clone();
            bad[field] = json!("not accepted");
            assert!(parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
        for (field, setting) in [
            ("version", json!(2)),
            ("epoch", json!(0)),
            ("repository", json!(Uuid::nil())),
            (
                "git_revision",
                json!(format!("native:sha256:{}", "11".repeat(32))),
            ),
        ] {
            let mut bad = value.clone();
            bad[field] = setting;
            assert!(parse(&serde_json::to_vec(&bad).unwrap()).is_err());
        }
    }
}
