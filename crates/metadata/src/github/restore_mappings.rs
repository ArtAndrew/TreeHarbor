//! A mapping receipt requires an opaque proof from the actual Rust projector.
use super::*;
use treeharbor_core::{ObjectId, RevisionRef};
use treeharbor_git_bridge::VerifiedMapping;
const KIND: &str = "github.restore.mapping.v1";

pub(crate) struct RestoreSource {
    pub root: ObjectId,
    pub snapshot: ObjectId,
    pub holder: String,
    pub decision_sha256: String,
    pub generation: i64,
}
fn object_id(bytes: Vec<u8>) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        bytes.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
/// Caller holds the repository lock; root retention comes from the durable
/// decision, never from an arbitrary supplied object ID or tenant-wide locator.
pub(crate) async fn source_context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    decision: &str,
) -> Result<RestoreSource> {
    crate::identifier(decision, 128)?;
    context(tx, auth, epoch, Permission::RepositoryWrite).await?;
    let s = auth.scope();
    let receipt:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind='github.restore.decision.v1'",
        &[&s.tenant(),&s.repository(),&decision]).await?.ok_or(Error::NotFound)?.get(0);
    let spec: RestoreDecisionSpec = serde_json::from_value(receipt["specification"].clone())
        .map_err(|_| Error::SchemaMismatch)?;
    let ctx = restore::retained_context(
        tx,
        auth,
        epoch,
        spec.binding,
        &spec.manifest_sha256,
        Permission::RepositoryWrite,
    )
    .await?;
    if restore_digest(&ctx)? != spec.context_sha256 {
        return Err(Error::RestoreContextChanged);
    }
    let RestoreDisposition::PreserveSource {
        native_revision, ..
    } = spec.disposition
    else {
        return Err(Error::Invalid("source preservation decision required"));
    };
    let RevisionRef::Native(root) = native_revision.parse().map_err(|_| Error::SchemaMismatch)?
    else {
        return Err(Error::SchemaMismatch);
    };
    let pin = &receipt["source_preservation"];
    let holder = pin["holder"]
        .as_str()
        .ok_or(Error::SchemaMismatch)?
        .to_owned();
    let generation = pin["generation"].as_i64().ok_or(Error::SchemaMismatch)?;
    let digest = root.as_bytes().to_vec();
    let valid:bool=tx.query_one("SELECT EXISTS(SELECT 1 FROM th_object_pins p JOIN th_objects o USING(tenant,digest,generation) WHERE p.tenant=$1 AND p.repository=$2 AND p.holder=$3 AND p.digest=$4 AND p.generation=$5 AND o.kind=5 AND o.state='live')",
        &[&s.tenant(),&s.repository(),&holder,&digest,&generation]).await?.get(0);
    if !valid {
        return Err(Error::ObjectNotLive);
    }
    // A native commit has exactly one Snapshot-typed child. The complete graph
    // and actual encoded commit are independently checked by the read/projector.
    let snapshots=tx.query("SELECT e.child FROM th_object_edges e JOIN th_objects o ON o.tenant=e.tenant AND o.digest=e.child AND o.generation=e.child_generation WHERE e.tenant=$1 AND e.parent=$2 AND e.parent_generation=$3 AND o.kind=3 AND o.state='live' LIMIT 2",
        &[&s.tenant(),&digest,&generation]).await?;
    if snapshots.len() != 1 {
        return Err(Error::ObjectNotLive);
    }
    Ok(RestoreSource {
        root,
        snapshot: object_id(snapshots[0].get(0))?,
        holder,
        decision_sha256: restore_digest(&receipt)?,
        generation,
    })
}
fn mapping_request(
    auth: &Authorization,
    cmd: &Command,
    decision: &str,
    expected: &str,
) -> Result<Request> {
    crate::identifier(decision, 128)?;
    let revision: RevisionRef = expected
        .parse()
        .map_err(|_| Error::Invalid("Git mapping revision"))?;
    if !matches!(revision, RevisionRef::Git(_)) || revision.to_string() != expected {
        return Err(Error::Invalid("Git mapping revision"));
    }
    request(auth, cmd, KIND, json!([1, decision, expected]))
}
impl Metadata {
    /// Recover a committed receipt before doing object IO. Fresh requests perform
    /// no mutation here; completion still repeats every fence atomically.
    pub async fn github_restore_mapping_replay(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        decision: &str,
        expected: &str,
    ) -> Result<Option<Value>> {
        match self
            .begin(mapping_request(auth, cmd, decision, expected)?)
            .await?
        {
            Begin::Replay(receipt) => Ok(Some(receipt)),
            Begin::Fresh(m) => {
                source_context(&m.tx, auth, cmd.epoch, decision).await?;
                Ok(None)
            }
        }
    }
    pub async fn record_github_restore_mapping(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        decision: &str,
        expected: &str,
        proof: &VerifiedMapping,
    ) -> Result<Value> {
        let m = match self
            .begin(mapping_request(auth, cmd, decision, expected)?)
            .await?
        {
            Begin::Replay(receipt) => return Ok(receipt),
            Begin::Fresh(m) => m,
        };
        let source = source_context(&m.tx, auth, cmd.epoch, decision).await?;
        if proof.native() != source.root || RevisionRef::Git(proof.git()).to_string() != expected {
            return Err(Error::ReceiptMismatch);
        }
        let s = auth.scope();
        let receipt = json!({"version":1,"key":cmd.key,"tenant":s.tenant(),"repository":s.repository(),"epoch":cmd.epoch,
            "decision_key":decision,"decision_sha256":source.decision_sha256,"native_revision":RevisionRef::Native(source.root).to_string(),
            "git_revision":expected,"source_holder":source.holder,"generation":source.generation,
            "source_objects":proof.source_objects(),"source_bytes":proof.source_bytes(),"verifier":"rust-git-projection-v1",
            "git_mapping_verified":true,"host_state_verified":false,"signatures_verified":false,
            "reconciliation_complete":false,"publication_authorized":false});
        m.complete(receipt, vec![]).await
    }
    pub async fn github_restore_mapping(
        &mut self,
        auth: &Authorization,
        key: &str,
    ) -> Result<Value> {
        crate::identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let receipt:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",&[&s.tenant(),&s.repository(),&key,&KIND]).await?.ok_or(Error::NotFound)?.get(0);
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(receipt)
    }
}
