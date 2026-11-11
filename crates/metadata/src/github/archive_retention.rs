//! Retain the full archive with the recorded inventory and exact object generations.
use super::{
    archive_artifact::check_archive_artifact, archive_observation, archive_proof::ArchiveStore, *,
};
use crate::{
    catalog::{ObjectKey, ObjectLease},
    uploads,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Write, sink},
    time::Instant,
};
use treeharbor_core::ObjectId;
use treeharbor_storage::ManagedStore;

const RETAIN: &str = "github.archive.retain.v1";

fn retain_key(spec: &ArchiveArtifactSpec) -> Result<String> {
    spec.validate()?;
    let suffix = spec
        .observation_key
        .strip_prefix("archive-observed-")
        .ok_or(Error::Invalid("archive observation key"))?;
    Ok(format!("archive-retained-{suffix}"))
}

fn operation(
    auth: &Authorization,
    epoch: i64,
    upload: Uuid,
    spec: &ArchiveArtifactSpec,
) -> Result<(String, Request)> {
    uuid(upload)?;
    let key = retain_key(spec)?;
    let command = Command {
        key: key.clone(),
        epoch,
    };
    Ok((
        key,
        request(auth, &command, RETAIN, json!([1, upload, spec]))?,
    ))
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    version: u8,
    state: String,
    key: String,
    tenant: String,
    repository: Uuid,
    epoch: i64,
    binding: Uuid,
    upload: Uuid,
    generation: i64,
    specification: ArchiveArtifactSpec,
}

#[derive(PartialEq, Eq)]
struct Objects {
    backend: String,
    generations: BTreeMap<ObjectId, i64>,
    root_hold: Uuid,
}

async fn objects(
    tx: &Transaction<'_>,
    scope: &Scope,
    upload: Uuid,
    backend: &str,
    spec: &ArchiveArtifactSpec,
) -> Result<Objects> {
    let root = spec.validate()?;
    let rows = tx
        .query(
            r#"
        SELECT i.digest,i.generation,i.hold_token,i.length,i.kind
        FROM th_source_upload_items i
        JOIN th_objects o ON o.tenant=i.tenant AND o.digest=i.digest AND o.generation=i.generation
        WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.complete
            AND o.state='live' AND o.backend=$4 AND o.kind=i.kind AND o.length=i.length
        LIMIT $5
        "#,
            &[
                &scope.tenant(),
                &scope.repository(),
                &upload,
                &backend,
                &(spec.object_count + 1),
            ],
        )
        .await?;
    if rows.len() as i64 != spec.object_count {
        return Err(Error::UploadMembership);
    }
    let mut root_hold = None;
    let mut encoded_bytes = 0i64;
    let mut generations = BTreeMap::new();
    for row in rows {
        let id = uploads::oid(row.get("digest"))?;
        let kind: i16 = row.get("kind");
        if id == root && kind == 2 {
            root_hold = Some(row.get("hold_token"));
        } else if id == root || kind != 1 {
            return Err(Error::UploadMembership);
        }
        encoded_bytes += row.get::<_, i64>("length");
        generations.insert(id, row.get("generation"));
    }
    if encoded_bytes != spec.encoded_bytes {
        return Err(Error::UploadMembership);
    }
    Ok(Objects {
        backend: backend.into(),
        generations,
        root_hold: root_hold.ok_or(Error::UploadMembership)?,
    })
}

async fn active_objects(
    tx: &Transaction<'_>,
    auth: &Authorization,
    upload: Uuid,
    spec: &ArchiveArtifactSpec,
) -> Result<Objects> {
    let scope = auth.scope();
    let session = uploads::active(tx, scope, auth.principal(), upload).await?;
    let ordinary: bool = tx.query_one(
        "SELECT github_import IS NULL FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3",
        &[&scope.tenant(), &scope.repository(), &upload],
    ).await?.get(0);
    if !ordinary
        || session.get::<_, i64>("max_objects") != spec.object_count
        || session.get::<_, i64>("max_bytes") != spec.encoded_bytes
    {
        return Err(Error::UploadState);
    }
    objects(tx, scope, upload, session.get("backend"), spec).await
}

pub enum ArchiveRetentionWork {
    Replay(Value),
    Verify(Box<ArchiveRetention>),
}

pub struct ArchiveRetention {
    key: String,
    upload: Uuid,
    specification: ArchiveArtifactSpec,
    record: ArchiveObservationReceipt,
    objects: Objects,
}

pub struct VerifiedArchiveRetention(Box<ArchiveRetention>);

impl ArchiveRetention {
    pub fn verify(
        self: Box<Self>,
        store: &ManagedStore,
        deadline: Instant,
    ) -> Result<VerifiedArchiveRetention> {
        read_artifact(
            store,
            &self.specification,
            &self.record,
            &self.objects,
            deadline,
            sink(),
        )?;
        Ok(VerifiedArchiveRetention(self))
    }
}

fn read_artifact(
    store: &ManagedStore,
    specification: &ArchiveArtifactSpec,
    record: &ArchiveObservationReceipt,
    objects: &Objects,
    deadline: Instant,
    output: impl Write,
) -> Result<Vec<u8>> {
    if store.backend() != objects.backend || store.tenant() != record.request.tenant {
        return Err(Error::ArchiveArtifactMismatch);
    }
    check_archive_artifact(
        &ArchiveStore {
            store,
            generations: &objects.generations,
        },
        specification,
        record,
        deadline,
        output,
    )
}

pub struct ArchiveStored {
    receipt: Receipt,
    record: ArchiveObservationReceipt,
    objects: Objects,
}

impl ArchiveStored {
    /// Only use a private temporary output. Publish it after successful verification.
    pub fn write(
        &self,
        store: &ManagedStore,
        deadline: Instant,
        bundle_output: impl Write,
    ) -> Result<Vec<u8>> {
        read_artifact(
            store,
            &self.receipt.specification,
            &self.record,
            &self.objects,
            deadline,
            bundle_output,
        )
    }
}

impl Metadata {
    pub async fn prepare_archive_retention(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        upload: Uuid,
        spec: &ArchiveArtifactSpec,
    ) -> Result<ArchiveRetentionWork> {
        let (key, request) = operation(auth, epoch, upload, spec)?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(ArchiveRetentionWork::Replay(value)),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let record =
            archive_observation::recorded(tx, auth.scope(), epoch, &spec.observation_key).await?;
        let objects = active_objects(tx, auth, upload, spec).await?;
        mutation.tx.commit().await?;
        Ok(ArchiveRetentionWork::Verify(Box::new(ArchiveRetention {
            key,
            upload,
            specification: spec.clone(),
            record,
            objects,
        })))
    }

    pub async fn record_archive_retention(
        &mut self,
        auth: &Authorization,
        proof: VerifiedArchiveRetention,
    ) -> Result<Value> {
        let verified = proof.0;
        let upload = verified.upload;
        let epoch = verified.record.request.epoch;
        let spec = &verified.specification;
        let (key, request) = operation(auth, epoch, upload, spec)?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let current =
            archive_observation::recorded(tx, scope, epoch, &spec.observation_key).await?;
        let objects = active_objects(tx, auth, upload, spec).await?;
        if verified.key != key || current != verified.record || objects != verified.objects {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let id = spec.validate()?;
        let generation = *objects
            .generations
            .get(&id)
            .ok_or(Error::UploadMembership)?;
        let lease = ObjectLease {
            key: ObjectKey {
                tenant: scope.tenant().to_owned(),
                backend: objects.backend,
                id,
                generation,
            },
            token: objects.root_hold,
            upload: false,
        };
        mutation.pin_kind(&key, &lease, 2).await?;
        let changed = tx.execute(r#"
            UPDATE th_source_uploads SET state='completed'
            WHERE tenant=$1 AND repository=$2 AND upload=$3 AND state='active' AND expires_at>clock_timestamp()
            "#, &[&scope.tenant(), &scope.repository(), &upload]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        mutation
            .complete(
                json!(Receipt {
                    version: 1,
                    state: "retained".into(),
                    key,
                    tenant: scope.tenant().into(),
                    repository: scope.repository(),
                    epoch,
                    binding: current.request.binding,
                    upload,
                    generation,
                    specification: spec.clone(),
                }),
                vec![],
            )
            .await
    }

    /// This historical data remains recoverable after a binding is revoked or
    /// the repository's authority epoch changes. Current owner permission applies.
    pub async fn open_retained_archive(
        &mut self,
        auth: &Authorization,
        key: &str,
    ) -> Result<ArchiveStored> {
        let scope = auth.scope();
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, scope, false).await?;
        auth.check_transaction(&tx, scope, auth.principal(), Permission::RepositoryWrite)
            .await?;
        let value = archive_proof::operation_receipt(&tx, scope, key, RETAIN).await?;
        let receipt: Receipt = serde_json::from_value(value).map_err(|_| Error::SchemaMismatch)?;
        let spec = &receipt.specification;
        if receipt.version != 1
            || receipt.state != "retained"
            || receipt.key != key
            || receipt.tenant != scope.tenant()
            || receipt.repository != scope.repository()
            || retain_key(spec)? != key
        {
            return Err(Error::SchemaMismatch);
        }
        let record = archive_observation::historical(&tx, scope, &spec.observation_key).await?;
        if record.request.binding != receipt.binding || record.request.epoch != receipt.epoch {
            return Err(Error::SchemaMismatch);
        }
        let root = spec.validate()?;
        let row = tx.query_opt(r#"
            SELECT u.backend FROM th_source_uploads u
            JOIN th_object_pins p ON p.tenant=u.tenant AND p.repository=u.repository
            JOIN th_objects o ON o.tenant=p.tenant AND o.digest=p.digest AND o.generation=p.generation
            WHERE u.tenant=$1 AND u.repository=$2 AND u.upload=$3 AND u.state='completed'
                AND u.authority='connected' AND u.epoch=$4 AND u.github_import IS NULL
                AND u.max_objects=$5 AND u.max_bytes=$6
                AND p.holder=$7 AND p.digest=$8 AND p.generation=$9 AND o.state='live'
                AND o.kind=2 AND o.backend=u.backend
            "#, &[&scope.tenant(), &scope.repository(), &receipt.upload, &receipt.epoch,
                  &spec.object_count, &spec.encoded_bytes, &key, &root.as_bytes().as_slice(), &receipt.generation])
            .await?.ok_or(Error::UploadMembership)?;
        let objects = objects(&tx, scope, receipt.upload, row.get(0), spec).await?;
        if objects.generations.get(&root) != Some(&receipt.generation) {
            return Err(Error::UploadMembership);
        }
        tx.commit().await?;
        Ok(ArchiveStored {
            receipt,
            record,
            objects,
        })
    }
}
