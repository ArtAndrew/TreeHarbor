//! Bind a fresh read of the archived host to a verified source receipt.
use super::{
    archive_proof::{StageReceipt, operation_receipt, verified_stage},
    *,
};
use crate::identifier;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use treeharbor_core::ObjectId;

const BEGIN: &str = "github.archive.observe.begin.v1";
const RECORD: &str = "github.archive.observe.record.v1";
const REQUEST_SECONDS: u64 = 180;
const FRESH_SECONDS: u64 = 120;
const CLOCK_SKEW_SECONDS: u64 = 5;
const MAX_INVENTORY_ITEMS: usize = 5000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveInventoryDigest {
    pub count: usize,
    pub sha256: String,
}

impl ArchiveInventoryDigest {
    pub fn tags(items: &[impl Serialize]) -> Result<Self> {
        Self::new(b"tags", items)
    }

    pub fn open_pulls(items: &[impl Serialize]) -> Result<Self> {
        Self::new(b"open-pulls", items)
    }

    fn new(set: &[u8], items: &[impl Serialize]) -> Result<Self> {
        if items.len() > MAX_INVENTORY_ITEMS {
            return Err(Error::ArchiveObservationMismatch);
        }
        let bytes = serde_json::to_vec(items).map_err(|_| Error::ArchiveObservationMismatch)?;
        let mut hash = Sha256::new();
        hash.update(b"treeharbor-archive-inventory\0v1\0");
        hash.update(set);
        hash.update(b"\0");
        hash.update(bytes);
        Ok(Self {
            count: items.len(),
            sha256: ObjectId::from_bytes(hash.finalize().into()).to_string(),
        })
    }

    fn validate(&self, set: &[u8]) -> Result<()> {
        let digest: ObjectId = self
            .sha256
            .parse()
            .map_err(|_| Error::ArchiveObservationMismatch)?;
        if digest.to_string() != self.sha256
            || self.count > MAX_INVENTORY_ITEMS
            || (self.count == 0 && *self != Self::new(set, &[] as &[Value])?)
        {
            return Err(Error::ArchiveObservationMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveObservation {
    pub version: u8,
    pub request_key: String,
    pub binding: Uuid,
    pub app_id: u64,
    pub upstream: Upstream,
    pub repository_node_id: String,
    pub default_branch: String,
    pub started_at_unix: u64,
    pub finished_at_unix: u64,
    pub token_revoked: bool,
    pub heads: BTreeMap<String, String>,
    pub tags: ArchiveInventoryDigest,
    pub open_pulls: ArchiveInventoryDigest,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArchiveObservationRequest {
    pub version: u8,
    pub tenant: String,
    pub repository: Uuid,
    pub epoch: i64,
    pub key: String,
    pub proof_key: String,
    pub stage_key: String,
    pub binding: Uuid,
    pub upload: Uuid,
    pub spool_fingerprint: String,
    pub observe_after_unix: u64,
    pub expires_at_unix: u64,
    pub observation_key: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchiveObservationContext {
    pub request: ArchiveObservationRequest,
    pub specification: BindingSpec,
}

async fn clock(tx: &Transaction<'_>) -> Result<u64> {
    let unix: i64 = tx
        .query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint",
            &[],
        )
        .await?
        .get(0);
    u64::try_from(unix).map_err(|_| Error::SchemaMismatch)
}

fn observation_key(request_key: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-archive-observation\0v1\0");
    hash.update(request_key.as_bytes());
    let digest = ObjectId::from_bytes(hash.finalize().into());
    format!("archive-observed-{}", digest.hex())
}

async fn live_request(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    key: &str,
) -> Result<(ArchiveObservationContext, StageReceipt)> {
    let scope = auth.scope();
    let value = operation_receipt(tx, scope, key, BEGIN).await?;
    let request: ArchiveObservationRequest =
        serde_json::from_value(value).map_err(|_| Error::SchemaMismatch)?;
    if request.version != 1
        || request.tenant != scope.tenant()
        || request.repository != scope.repository()
        || request.key != key
        || request.observation_key != observation_key(key)
    {
        return Err(Error::SchemaMismatch);
    }
    if request.epoch != epoch {
        return Err(Error::Fenced);
    }
    if clock(tx).await? >= request.expires_at_unix {
        return Err(Error::ArchiveObservationMismatch);
    }
    let (_, specification) = assigned(tx, auth, epoch, request.binding).await?;
    let (stage_key, source) = verified_stage(tx, scope, epoch, &request.proof_key).await?;
    if request.stage_key != stage_key
        || request.binding != source.binding
        || request.upload != source.upload
        || request.spool_fingerprint != source.specification.spool_fingerprint
    {
        return Err(Error::ArchiveObservationMismatch);
    }
    Ok((
        ArchiveObservationContext {
            request,
            specification,
        },
        source,
    ))
}

fn check_observation(
    observation: &ArchiveObservation,
    context: &ArchiveObservationContext,
    source: &StageReceipt,
    now: u64,
) -> Result<()> {
    identifier(&observation.repository_node_id, 256)?;
    observation.tags.validate(b"tags")?;
    observation.open_pulls.validate(b"open-pulls")?;
    let request = &context.request;
    let stage = &source.specification;
    if observation.version != 1
        || observation.request_key != request.key
        || observation.binding != request.binding
        || observation.app_id != context.specification.app_id
        || observation.upstream != context.specification.upstream
        || observation.default_branch != stage.default_branch
        || !observation.token_revoked
        || observation.started_at_unix < request.observe_after_unix
        || observation.finished_at_unix < observation.started_at_unix
        || observation.finished_at_unix - observation.started_at_unix > FRESH_SECONDS
        || observation.finished_at_unix > now + CLOCK_SKEW_SECONDS
        || observation.finished_at_unix < now.saturating_sub(FRESH_SECONDS)
        || now >= request.expires_at_unix
        || observation.heads.len() != stage.branches.len()
        || stage
            .branches
            .iter()
            .any(|(name, tip)| observation.heads.get(name) != Some(&tip.git_revision))
    {
        return Err(Error::ArchiveObservationMismatch);
    }
    Ok(())
}

impl Metadata {
    pub async fn begin_archive_observation(
        &mut self,
        auth: &Authorization,
        command: &Command,
        proof_key: &str,
    ) -> Result<Value> {
        identifier(proof_key, 128)?;
        let mutation = match self
            .begin(request(auth, command, BEGIN, json!([1, proof_key]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let (stage_key, source) = verified_stage(tx, scope, command.epoch, proof_key).await?;
        let now = clock(tx).await?;
        let result = ArchiveObservationRequest {
            version: 1,
            tenant: scope.tenant().to_owned(),
            repository: scope.repository(),
            epoch: command.epoch,
            key: command.key.clone(),
            proof_key: proof_key.to_owned(),
            stage_key,
            binding: source.binding,
            upload: source.upload,
            spool_fingerprint: source.specification.spool_fingerprint,
            observe_after_unix: now,
            expires_at_unix: now + REQUEST_SECONDS,
            observation_key: observation_key(&command.key),
        };
        mutation.complete(json!(result), vec![]).await
    }

    pub async fn archive_observation_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        request_key: &str,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let (context, _) = live_request(&tx, auth, epoch, request_key).await?;
        if clock(&tx).await? >= context.request.expires_at_unix {
            return Err(Error::ArchiveObservationMismatch);
        }
        tx.commit().await?;
        Ok(json!(context))
    }

    pub async fn record_archive_observation(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        observation: &ArchiveObservation,
    ) -> Result<Value> {
        identifier(&observation.request_key, 128)?;
        let command = Command {
            key: observation_key(&observation.request_key),
            epoch,
        };
        let mutation = match self
            .begin(request(auth, &command, RECORD, json!([1, observation]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let (context, source) = live_request(tx, auth, epoch, &observation.request_key).await?;
        check_observation(observation, &context, &source, clock(tx).await?)?;
        let result = json!({"version":1,"state":"observed","request":context.request,"observation":observation});
        mutation.write_completion(&result, &[]).await?;
        let now = clock(&mutation.tx).await?;
        if now >= context.request.expires_at_unix
            || observation.finished_at_unix < now.saturating_sub(FRESH_SECONDS)
        {
            return Err(Error::ArchiveObservationMismatch);
        }
        mutation.tx.commit().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_observation_wire_matches_v1_digest_and_receipt_vectors() {
        let fixture = include_str!("../../tests/fixtures/github-archive-observation-v1.json");
        let command: WireCommand = serde_json::from_str(fixture).unwrap();
        assert_eq!(command.permission(), Permission::HostRead);
        let WireCommand::ArchiveObserveRecord { epoch, observation } = command else {
            panic!("expected archive observation");
        };
        assert_eq!(epoch, 4);
        assert_eq!(
            observation_key(&observation.request_key),
            "archive-observed-54e83d0cf66c527a37c42a196365a1f73b3900ca7aa931164d1eb9fa569e271f"
        );
        assert_eq!(
            observation.tags,
            ArchiveInventoryDigest::tags(&[] as &[Value]).unwrap()
        );
        assert_eq!(
            observation.open_pulls,
            ArchiveInventoryDigest::open_pulls(&[] as &[Value]).unwrap()
        );
        assert!(observation.tags.validate(b"open-pulls").is_err());
        let mut value: Value = serde_json::from_str(fixture).unwrap();
        value["observation"]["permission"] = json!("write");
        assert!(serde_json::from_value::<WireCommand>(value).is_err());
    }
}
