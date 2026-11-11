//! Resume a proposal archive upload without modifying the prepared directory.
use super::{Credential, Error, Result, Uuid, files, json, proposals::STORE_TENANT};
use crate::import::{Finish, Transfer, retain_journal};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};
use treeharbor_core::{FRAME_HEADER, MAX_PAYLOAD, Object, ObjectId, ObjectStore};
use treeharbor_metadata::{
    commands::MAX_REQUEST_BYTES,
    github::{
        ARCHIVE_PROPOSALS_FORMAT, ArchiveProposalManifest, ArchiveProposalStageSpec,
        archive_proposals_fingerprint,
    },
};
use treeharbor_storage::{LocalStore, TenantId};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    /// Key returned by archive-retain.
    #[arg(long)]
    retained_key: String,
    /// Private directory returned by archive-proposals.
    #[arg(long)]
    prepared: PathBuf,
    /// Exact manifest_sha256 returned by archive-proposals.
    #[arg(long)]
    fingerprint: String,
    /// Private persistent upload journal, retained on retry.
    #[arg(long)]
    state: PathBuf,
    #[arg(long, value_parser = clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long, default_value_t = 3600, value_parser = clap::value_parser!(i32).range(1..=3600))]
    ttl_seconds: i32,
    #[arg(long, default_value_t = 60, value_parser = clap::value_parser!(u64).range(1..=3600))]
    request_timeout_seconds: u64,
}

#[derive(Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Intent {
    tenant: String,
    repository: Uuid,
    epoch: i64,
    ttl_seconds: i32,
    specification: ArchiveProposalStageSpec,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    version: u8,
    upload: Uuid,
    intent: Intent,
}

struct Prepared {
    manifest: Object,
    manifest_id: ObjectId,
    store: LocalStore,
}

impl ObjectStore for Prepared {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        if id == self.manifest_id {
            Ok(self.manifest.clone())
        } else {
            self.store.get(id)
        }
    }

    fn put(&self, _object: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "read-only proposal archive",
        ))
    }
}

fn read(
    path: &Path,
    key: String,
    expected: &str,
) -> Result<(Prepared, ArchiveProposalStageSpec, Vec<ObjectId>)> {
    let path = files::private_directory(path)?;
    let bytes = files::read_private(&path.join("manifest.json"), MAX_PAYLOAD)?;
    let fingerprint = archive_proposals_fingerprint(&bytes).to_string();
    if fingerprint != expected {
        return Err(Error::PublicationState);
    }
    let manifest: ArchiveProposalManifest =
        serde_json::from_slice(&bytes).map_err(|_| Error::PublicationState)?;
    if manifest.format != ARCHIVE_PROPOSALS_FORMAT || manifest.version != 1 {
        return Err(Error::PublicationState);
    }
    let encoded_bytes = manifest
        .encoded_bytes
        .checked_add((FRAME_HEADER + bytes.len()) as u64)
        .and_then(|n| i64::try_from(n).ok())
        .ok_or(Error::Size)?;
    let blob = Object::Blob(bytes);
    let manifest_id = blob.id().map_err(crate::pack::Error::Object)?;
    let mut ids = Vec::with_capacity(manifest.objects.len() + 1);
    let mut seen = BTreeSet::from([manifest_id]);
    for raw in manifest.objects {
        let id: ObjectId = raw.parse().map_err(crate::pack::Error::Object)?;
        if id.to_string() != raw || !seen.insert(id) {
            return Err(Error::PublicationState);
        }
        ids.push(id);
    }
    ids.push(manifest_id);
    let spec = ArchiveProposalStageSpec {
        version: 1,
        retained_key: key,
        manifest_object: manifest_id.to_string(),
        manifest_sha256: fingerprint,
        object_count: ids.len() as i64,
        encoded_bytes,
    };
    spec.key().map_err(Error::ArchiveArtifact)?;
    files::private_directory(&path.join("objects").join(STORE_TENANT))?;
    files::private_directory(&path.join("staging").join(STORE_TENANT))?;
    let tenant = TenantId::new(STORE_TENANT).map_err(crate::pack::Error::Object)?;
    let store = LocalStore::open(path, &tenant).map_err(crate::pack::Error::Object)?;
    Ok((
        Prepared {
            manifest: blob,
            manifest_id,
            store,
        },
        spec,
        ids,
    ))
}

pub(in crate::github) async fn run(options: Options) -> Result<bool> {
    let credential = Credential::load(&options.credential_file)?;
    let (store, specification, ids) = tokio::task::spawn_blocking(move || {
        read(
            &options.prepared,
            options.retained_key,
            &options.fingerprint,
        )
    })
    .await
    .map_err(|_| Error::ImportWorker)??;
    let proposed = Journal {
        version: 1,
        upload: Uuid::new_v4(),
        intent: Intent {
            tenant: credential.scope.tenant().into(),
            repository: credential.scope.repository(),
            epoch: options.epoch,
            ttl_seconds: options.ttl_seconds,
            specification,
        },
    };
    let journal: Journal = retain_journal(&options.state, &proposed, MAX_REQUEST_BYTES)?;
    if journal.version != 1 || journal.upload.is_nil() || journal.intent != proposed.intent {
        return Err(Error::ImportState);
    }
    let intent = journal.intent;
    let upload = journal.upload;
    let spec = intent.specification;
    let transfer = Transfer {
        socket: &options.socket,
        credential: &credential,
        timeout: Duration::from_secs(options.request_timeout_seconds),
        upload,
        begin: json!({"method":"source_begin","key":format!("archive-proposals-{upload}-begin"),
            "authority":"connected","epoch":intent.epoch,"upload":upload,"ttl_seconds":intent.ttl_seconds,
            "max_objects":spec.object_count,"max_bytes":spec.encoded_bytes}),
        finish: Finish::ArchiveUpload {
            path: crate::archive_proposal_stage::PATH,
            fields: json!({"epoch":intent.epoch,"upload":upload,"specification":spec}),
        },
    };
    let reply = transfer.send(ids, store).await?;
    if reply.status.is_success() {
        let result = &reply.body["result"];
        if reply.body["version"] != 1
            || result["version"] != 1
            || result["state"] != "staged"
            || result["authority"] != "connected"
            || result["key"] != spec.key().map_err(Error::ArchiveArtifact)?
            || result["tenant"] != intent.tenant
            || result["repository"] != json!(intent.repository)
            || result["epoch"] != intent.epoch
            || result["upload"] != json!(upload)
            || result["specification"] != json!(spec)
        {
            return Err(Error::Http);
        }
    }
    println!("{}", reply.body);
    Ok(reply.status.is_success())
}
