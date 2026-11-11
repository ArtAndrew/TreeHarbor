//! Verify exact Git identities from the permanently pinned, stored source set.
use super::{archive_stage::check_archive_binding, *};
use crate::{identifier, uploads};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, time::Instant};
use treeharbor_core::{Object, ObjectId, ObjectStore, RevisionRef};
use treeharbor_git_bridge::{self as git, Limits};
use treeharbor_storage::ManagedStore;

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StageReceipt {
    version: u8,
    tenant: String,
    repository: Uuid,
    authority: String,
    epoch: i64,
    binding: Uuid,
    upload: Uuid,
    state: String,
    specification: ArchiveStageSpec,
}

async fn stage(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
    stage_key: &str,
) -> Result<StageReceipt> {
    let row = tx
        .query_opt(
            "SELECT result FROM th_operations
             WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind='github.archive.stage.v1'",
            &[&scope.tenant(), &scope.repository(), &stage_key],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let receipt: StageReceipt =
        serde_json::from_value(row.get(0)).map_err(|_| Error::SchemaMismatch)?;
    if receipt.version != 1
        || receipt.tenant != scope.tenant()
        || receipt.repository != scope.repository()
        || receipt.authority != Authority::Connected.as_str()
        || receipt.state != "staged"
        || receipt.binding.is_nil()
        || receipt.upload.is_nil()
    {
        return Err(Error::SchemaMismatch);
    }
    if receipt.epoch != epoch {
        return Err(Error::Fenced);
    }
    check_archive_binding(tx, scope, receipt.binding, epoch, &receipt.specification).await?;
    Ok(receipt)
}

async fn retained_backend(
    tx: &Transaction<'_>,
    scope: &Scope,
    receipt: &StageReceipt,
) -> Result<String> {
    let spec = &receipt.specification;
    let tenant = scope.tenant();
    let repository = scope.repository();
    let row = tx
        .query_opt(
            "SELECT backend FROM th_source_uploads WHERE tenant=$1 AND repository=$2 AND upload=$3
         AND state='completed' AND authority='connected' AND epoch=$4 AND github_import IS NULL
         AND max_objects=$5 AND reserved_objects=$5 AND max_bytes=$6 AND reserved_bytes=$6",
            &[
                &tenant,
                &repository,
                &receipt.upload,
                &receipt.epoch,
                &spec.object_count,
                &spec.encoded_bytes,
            ],
        )
        .await?
        .ok_or(Error::UploadState)?;
    let backend: String = row.get(0);
    let roots = spec.roots().map_err(|_| Error::SchemaMismatch)?;
    let revisions: Vec<_> = roots
        .keys()
        .map(|id| RevisionRef::Native(*id).to_string())
        .collect();
    let pins = tx.query(
        r#"
        SELECT b.revision,b.snapshot,p.digest
        FROM th_bases b
        JOIN th_object_pins p ON p.tenant=b.tenant AND p.repository=b.repository AND p.holder=b.pin_holder
        JOIN th_source_upload_items i ON i.tenant=p.tenant AND i.repository=p.repository
            AND i.digest=p.digest AND i.generation=p.generation
        JOIN th_objects o ON o.tenant=p.tenant AND o.digest=p.digest AND o.generation=p.generation
        WHERE b.tenant=$1 AND b.repository=$2 AND b.revision=ANY($3::text[])
            AND i.upload=$4 AND i.complete AND i.kind=5 AND o.state='live' AND o.backend=$5
        "#,
        &[&tenant, &repository, &revisions, &receipt.upload, &backend],
    ).await?;
    if pins.len() != roots.len() {
        return Err(Error::UploadMembership);
    }
    for row in pins {
        let id = uploads::oid(row.get("digest"))?;
        let root = roots.get(&id).ok_or(Error::UploadMembership)?;
        if row.get::<_, &str>("revision") != RevisionRef::Native(id).to_string()
            || row.get::<_, Vec<u8>>("snapshot") != root.snapshot.as_bytes()
        {
            return Err(Error::UploadMembership);
        }
    }
    Ok(backend)
}

pub enum ArchiveProofWork {
    Replay(Value),
    Verify(Box<RetainedArchive>),
}

pub struct RetainedArchive {
    stage_key: String,
    receipt: StageReceipt,
    backend: String,
    generations: BTreeMap<ObjectId, i64>,
}

/// This value is produced only from verified objects read on the server.
pub struct VerifiedArchive {
    stage_key: String,
    receipt: StageReceipt,
    backend: String,
}

// The exact permanent base pins protect the whole closure while projecting.
// The adapter never resolves a digest outside this scoped upload's generations.
struct ArchiveStore<'a> {
    store: &'a ManagedStore,
    generations: &'a BTreeMap<ObjectId, i64>,
}

impl ObjectStore for ArchiveStore<'_> {
    fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
        let generation = self
            .generations
            .get(&id)
            .ok_or(treeharbor_core::Error::MissingObject(id))?;
        self.store.get(*generation, id)
    }

    fn put(&self, _object: &Object) -> treeharbor_core::Result<ObjectId> {
        Err(treeharbor_core::Error::InvalidObject(
            "read-only archived source",
        ))
    }
}

impl RetainedArchive {
    pub fn verify(
        self,
        store: &ManagedStore,
        scratch: &Path,
        deadline: Instant,
    ) -> git::Result<VerifiedArchive> {
        if store.backend() != self.backend || store.tenant() != self.receipt.tenant {
            return Err(git::Error::InvalidProjection);
        }
        let store = ArchiveStore {
            store,
            generations: &self.generations,
        };
        let spec = &self.receipt.specification;
        let mut expected = BTreeMap::new();
        for (name, tip) in &spec.branches {
            if Instant::now() >= deadline {
                return Err(git::Error::Limit);
            }
            let RevisionRef::Native(native) = tip.native_revision.parse()? else {
                return Err(git::Error::InvalidProjection);
            };
            let RevisionRef::Git(git_oid) = tip.git_revision.parse()? else {
                return Err(git::Error::InvalidProjection);
            };
            let snapshot: ObjectId = tip.snapshot.parse()?;
            if !matches!(store.get(native)?, Object::NativeCommit(commit) if commit.snapshot == snapshot)
            {
                return Err(git::Error::InvalidProjection);
            }
            expected.insert(name.clone(), (native, git_oid));
        }
        let timeout = deadline.saturating_duration_since(Instant::now());
        if timeout.is_zero() {
            return Err(git::Error::Limit);
        }
        let proof = git::verify_branch_mappings(
            &store,
            expected,
            scratch,
            Limits {
                timeout,
                ..Limits::default()
            },
        )?;
        if proof.source_objects() != spec.object_count as usize
            || proof.source_bytes() != spec.encoded_bytes as u64
        {
            return Err(git::Error::InvalidProjection);
        }
        Ok(VerifiedArchive {
            stage_key: self.stage_key,
            receipt: self.receipt,
            backend: self.backend,
        })
    }
}

fn proof_request(auth: &Authorization, command: &Command, stage_key: &str) -> Result<Request> {
    identifier(stage_key, 128)?;
    request(
        auth,
        command,
        "github.archive.proof.v1",
        json!([1, stage_key]),
    )
}

impl Metadata {
    pub async fn prepare_archive_proof(
        &mut self,
        auth: &Authorization,
        command: &Command,
        stage_key: &str,
    ) -> Result<ArchiveProofWork> {
        let mutation = match self.begin(proof_request(auth, command, stage_key)?).await? {
            Begin::Replay(value) => return Ok(ArchiveProofWork::Replay(value)),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let receipt = stage(tx, scope, command.epoch, stage_key).await?;
        let backend = retained_backend(tx, scope, &receipt).await?;
        let spec = &receipt.specification;
        let limits = Limits::default();
        if spec.object_count as usize > limits.objects
            || spec.encoded_bytes as u64 > limits.source_bytes
        {
            return Err(Error::UploadLimit);
        }
        let rows = tx.query(
            r#"
            SELECT i.digest,i.generation FROM th_source_upload_items i
            JOIN th_objects o ON o.tenant=i.tenant AND o.digest=i.digest AND o.generation=i.generation
            WHERE i.tenant=$1 AND i.repository=$2 AND i.upload=$3 AND i.complete
                AND o.state='live' AND o.backend=$4 AND o.kind=i.kind AND o.length=i.length
            LIMIT $5
            "#,
            &[&scope.tenant(), &scope.repository(), &receipt.upload, &backend, &(spec.object_count + 1)],
        ).await?;
        if rows.len() != spec.object_count as usize {
            return Err(Error::UploadMembership);
        }
        let generations = rows
            .into_iter()
            .map(|row| Ok((uploads::oid(row.get(0))?, row.get(1))))
            .collect::<Result<_>>()?;
        mutation.tx.commit().await?;
        Ok(ArchiveProofWork::Verify(Box::new(RetainedArchive {
            stage_key: stage_key.to_owned(),
            receipt,
            backend,
            generations,
        })))
    }

    pub async fn record_archive_proof(
        &mut self,
        auth: &Authorization,
        command: &Command,
        proof: VerifiedArchive,
    ) -> Result<Value> {
        let mutation = match self
            .begin(proof_request(auth, command, &proof.stage_key)?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let current = stage(tx, scope, command.epoch, &proof.stage_key).await?;
        if current != proof.receipt || retained_backend(tx, scope, &current).await? != proof.backend
        {
            return Err(Error::ReceiptMismatch);
        }
        mutation
            .complete(
                json!({
                    "version":1,"tenant":current.tenant,"repository":current.repository,
                    "authority":"connected","epoch":current.epoch,"binding":current.binding,
                    "upload":current.upload,"stage_key":proof.stage_key,"state":"verified",
                    "spool_fingerprint":current.specification.spool_fingerprint,
                    "object_count":current.specification.object_count,
                    "encoded_bytes":current.specification.encoded_bytes,
                }),
                vec![],
            )
            .await
    }
}
