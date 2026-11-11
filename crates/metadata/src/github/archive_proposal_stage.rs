//! Pin verified proposal inputs for a later authority handoff.
use super::{
    archive_observation,
    archive_proof::ArchiveStore,
    archive_proposals::{VerifiedProposals, verify_proposals},
    archive_retention::{ArchiveStored, retained_archive_in},
    *,
};
use crate::uploads;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, time::Instant};
use treeharbor_core::{FRAME_HEADER, MAX_PAYLOAD, Object, ObjectId, ObjectStore};
use treeharbor_git_bridge::Limits;
use treeharbor_storage::ManagedStore;

const STAGE: &str = "github.archive.proposals.stage.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveProposalStageSpec {
    pub version: u8,
    pub retained_key: String,
    pub manifest_object: String,
    pub manifest_sha256: String,
    /// Includes the uploaded manifest blob.
    pub object_count: i64,
    /// Total canonical frame bytes, including the manifest blob.
    pub encoded_bytes: i64,
}

impl ArchiveProposalStageSpec {
    fn validate(&self) -> Result<ObjectId> {
        let invalid = || Error::Invalid("archive proposal stage");
        let canonical = |s: &str| -> Result<ObjectId> {
            let id: ObjectId = s.parse().map_err(|_| invalid())?;
            if id.to_string() != s {
                return Err(invalid());
            }
            Ok(id)
        };
        let key = self
            .retained_key
            .strip_prefix("archive-retained-")
            .ok_or_else(invalid)?;
        canonical(&format!("sha256:{key}"))?;
        canonical(&self.manifest_sha256)?;
        let limits = Limits::default();
        if self.version != 1
            || !(1..=limits.objects as i64 + 1).contains(&self.object_count)
            || !(FRAME_HEADER as i64
                ..=limits.source_bytes as i64 + (MAX_PAYLOAD + FRAME_HEADER) as i64)
                .contains(&self.encoded_bytes)
        {
            return Err(invalid());
        }
        canonical(&self.manifest_object)
    }

    pub fn key(&self) -> Result<String> {
        self.validate()?;
        Ok(self
            .retained_key
            .replacen("archive-retained-", "archive-proposals-", 1))
    }
}

fn operation(
    auth: &Authorization,
    epoch: i64,
    upload: Uuid,
    spec: &ArchiveProposalStageSpec,
) -> Result<(String, Request)> {
    uuid(upload)?;
    let key = spec.key()?;
    let command = Command {
        key: key.clone(),
        epoch,
    };
    Ok((
        key,
        request(auth, &command, STAGE, json!([1, upload, spec]))?,
    ))
}

#[derive(PartialEq, Eq)]
struct Objects {
    backend: String,
    generations: BTreeMap<ObjectId, i64>,
    manifest_length: i64,
}

async fn active_objects(
    tx: &Transaction<'_>,
    auth: &Authorization,
    upload: Uuid,
    spec: &ArchiveProposalStageSpec,
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
    let backend: String = session.get("backend");
    let rows = tx
        .query(
            r#"
        SELECT i.digest,i.generation,i.length,i.kind
        FROM th_source_upload_items i
        JOIN th_objects o ON o.tenant=i.tenant AND o.digest=i.digest AND o.generation=i.generation
        JOIN th_object_holds h ON h.tenant=i.tenant AND h.token=i.hold_token
            AND h.digest=i.digest AND h.generation=i.generation
        WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.complete
            AND o.state='live' AND o.backend=$4 AND o.kind=i.kind AND o.length=i.length
            AND h.expires_at>clock_timestamp()
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
    let manifest = spec.validate()?;
    let mut manifest_length = None;
    let mut bytes = 0i64;
    let mut generations = BTreeMap::new();
    for row in rows {
        let id = uploads::oid(row.get("digest"))?;
        let length: i64 = row.get("length");
        if id == manifest && row.get::<_, i16>("kind") == 1 {
            manifest_length = Some(length);
        }
        bytes += length;
        generations.insert(id, row.get("generation"));
    }
    if bytes != spec.encoded_bytes {
        return Err(Error::UploadMembership);
    }
    Ok(Objects {
        backend,
        generations,
        manifest_length: manifest_length.ok_or(Error::UploadMembership)?,
    })
}

async fn checked_archive(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
    retained_key: &str,
) -> Result<ArchiveStored> {
    let archive =
        retained_archive_in(tx, scope, retained_key)
            .await
            .map_err(|error| match error {
                Error::NotFound => Error::ArchiveArtifactMismatch,
                other => other,
            })?;
    let current = archive_observation::recorded(
        tx,
        scope,
        epoch,
        &archive.observation().request.observation_key,
    )
    .await?;
    if current != *archive.observation() {
        return Err(Error::ArchiveArtifactMismatch);
    }
    Ok(archive)
}

pub enum ArchiveProposalStageWork {
    Replay(Value),
    Verify(Box<ArchiveProposalStage>),
}

pub struct ArchiveProposalStage {
    upload: Uuid,
    specification: ArchiveProposalStageSpec,
    archive: ArchiveStored,
    objects: Objects,
}

pub struct VerifiedArchiveProposalStage {
    stage: Box<ArchiveProposalStage>,
    manifest: ObjectId,
    proposals: VerifiedProposals,
}

impl ArchiveProposalStage {
    pub fn verify(
        self: Box<Self>,
        store: &ManagedStore,
        deadline: Instant,
    ) -> Result<VerifiedArchiveProposalStage> {
        if store.backend() != self.objects.backend
            || store.tenant() != self.archive.observation().request.tenant
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let manifest = self.specification.validate()?;
        let source = ArchiveStore {
            store,
            generations: &self.objects.generations,
        };
        let Object::Blob(bytes) = source.get(manifest)? else {
            return Err(Error::ArchiveArtifactMismatch);
        };
        if archive_proposals_fingerprint(&bytes).to_string() != self.specification.manifest_sha256
            || (FRAME_HEADER + bytes.len()) as i64 != self.objects.manifest_length
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let decoded: ArchiveProposalManifest =
            serde_json::from_slice(&bytes).map_err(|_| Error::ArchiveArtifactMismatch)?;
        if decoded.encoded_bytes
            != (self.specification.encoded_bytes - self.objects.manifest_length) as u64
        {
            return Err(Error::ArchiveArtifactMismatch);
        }
        let archive = self.archive.manifest(store, deadline)?;
        let uploaded = self
            .objects
            .generations
            .keys()
            .filter(|id| **id != manifest)
            .copied()
            .collect();
        let mut proposals = verify_proposals(
            &source,
            decoded,
            &archive,
            self.archive.fingerprint(),
            &uploaded,
            deadline,
        )
        .map_err(|error| match error {
            Error::Objects(treeharbor_core::Error::TooLarge) => Error::UploadLimit,
            other => other,
        })?;
        proposals.roots.insert(manifest);
        Ok(VerifiedArchiveProposalStage {
            stage: self,
            manifest,
            proposals,
        })
    }
}

// Lock the current catalogue rows before creating root pins, in digest order.
// The already verified source-upload holds protect their reachable children.
async fn pin_roots(
    tx: &Transaction<'_>,
    scope: &Scope,
    key: &str,
    proof: &VerifiedArchiveProposalStage,
) -> Result<()> {
    let mut digests = Vec::with_capacity(proof.proposals.roots.len());
    let mut generations = Vec::with_capacity(digests.capacity());
    let mut holders = Vec::with_capacity(digests.capacity());
    for id in &proof.proposals.roots {
        let mut hash = Sha256::new();
        hash.update(b"treeharbor-archive-proposal-pin\0v1\0");
        hash.update(key.as_bytes());
        hash.update(id.as_bytes());
        let holder = ObjectId::from_bytes(hash.finalize().into()).hex();
        holders.push(format!("archive-proposal-{holder}"));
        digests.push(id.as_bytes().to_vec());
        generations.push(
            *proof
                .stage
                .objects
                .generations
                .get(id)
                .ok_or(Error::UploadMembership)?,
        );
    }
    let pinned = tx
        .execute(
            r#"
        WITH roots AS MATERIALIZED (
            SELECT o.digest,o.generation,r.holder
            FROM unnest($3::bytea[],$4::bigint[],$5::text[]) AS r(digest,generation,holder)
            JOIN th_objects o ON o.tenant=$1 AND o.digest=r.digest AND o.generation=r.generation
            WHERE o.state='live' AND o.backend=$6 ORDER BY o.digest FOR UPDATE OF o
        )
        INSERT INTO th_object_pins (tenant,repository,holder,digest,generation)
        SELECT $1,$2,holder,digest,generation FROM roots
        "#,
            &[
                &scope.tenant(),
                &scope.repository(),
                &digests,
                &generations,
                &holders,
                &proof.stage.objects.backend,
            ],
        )
        .await?;
    if pinned != digests.len() as u64 {
        return Err(Error::UploadMembership);
    }
    tx.execute(
        "UPDATE th_objects SET orphaned_at=NULL WHERE tenant=$1 AND digest=ANY($2::bytea[])",
        &[&scope.tenant(), &digests],
    )
    .await?;
    Ok(())
}

impl Metadata {
    pub async fn prepare_archive_proposal_stage(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        upload: Uuid,
        spec: &ArchiveProposalStageSpec,
    ) -> Result<ArchiveProposalStageWork> {
        let (_, request) = operation(auth, epoch, upload, spec)?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(ArchiveProposalStageWork::Replay(value)),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let archive = checked_archive(tx, auth.scope(), epoch, &spec.retained_key).await?;
        let objects = active_objects(tx, auth, upload, spec).await?;
        mutation.tx.commit().await?;
        Ok(ArchiveProposalStageWork::Verify(Box::new(
            ArchiveProposalStage {
                upload,
                specification: spec.clone(),
                archive,
                objects,
            },
        )))
    }

    pub async fn record_archive_proposal_stage(
        &mut self,
        auth: &Authorization,
        proof: VerifiedArchiveProposalStage,
    ) -> Result<Value> {
        let stage = &proof.stage;
        let spec = &stage.specification;
        let epoch = stage.archive.observation().request.epoch;
        let (key, request) = operation(auth, epoch, stage.upload, spec)?;
        let mutation = match self.begin(request).await? {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let archive = checked_archive(tx, scope, epoch, &spec.retained_key).await?;
        let objects = active_objects(tx, auth, stage.upload, spec).await?;
        if archive != stage.archive || objects != stage.objects {
            return Err(Error::ArchiveArtifactMismatch);
        }
        pin_roots(tx, scope, &key, &proof).await?;
        let changed = tx.execute(r#"
            UPDATE th_source_uploads SET state='completed'
            WHERE tenant=$1 AND repository=$2 AND upload=$3 AND state='active' AND expires_at>clock_timestamp()
            "#, &[&scope.tenant(), &scope.repository(), &stage.upload]).await?;
        if changed != 1 {
            return Err(Error::UploadState);
        }
        mutation
            .complete(
                json!({
                    "version":1,"state":"staged","authority":"connected","key":key,
                    "tenant":scope.tenant(),"repository":scope.repository(),"epoch":epoch,
                    "binding":archive.observation().request.binding,"upload":stage.upload,
                    "specification":spec,"manifest_generation":objects.generations[&proof.manifest],
                    "open_pulls":proof.proposals.count,"ready":proof.proposals.ready,
                    "needs_resolution":proof.proposals.count-proof.proposals.ready,
                    "root_count":proof.proposals.roots.len(),
                }),
                vec![],
            )
            .await
    }
}
