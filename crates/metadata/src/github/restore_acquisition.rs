//! Current owner authorization to recover source, separate from active host imports.
use super::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreAcquisitionContext {
    pub version: u8,
    pub tenant: String,
    pub repository: Uuid,
    pub epoch: i64,
    pub decision_key: String,
    pub decision_sha256: String,
    pub decision: RestoreDecisionSpec,
    pub binding_specification: BindingSpec,
    pub publication_authorized: bool,
}
impl Metadata {
    pub async fn github_restore_acquisition_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
    ) -> Result<Value> {
        crate::identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::RepositoryWrite).await?;
        let receipt: Value = tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind='github.restore.decision.v1'", &[&s.tenant(), &s.repository(), &key]).await?.ok_or(Error::NotFound)?.get(0);
        let spec: RestoreDecisionSpec = serde_json::from_value(receipt["specification"].clone())
            .map_err(|_| Error::SchemaMismatch)?;
        if spec.version != 3
            || !matches!(spec.disposition, RestoreDisposition::AcquireSource { .. })
        {
            return Err(Error::Invalid("source acquisition decision required"));
        }
        if receipt["epoch"] != epoch
            || receipt["tenant"] != s.tenant()
            || receipt["repository"] != json!(s.repository())
            || receipt["key"] != key
        {
            return Err(Error::Fenced);
        }
        let ctx = restore::retained_context(
            &tx,
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
        let subject = restore_digest(&spec.subject)?;
        let latest: String = tx.query_one("SELECT key FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind='github.restore.decision.v1' AND result->>'manifest_sha256'=$3 AND result->>'binding'=$4 AND result->>'subject_sha256'=$5 ORDER BY (result->>'sequence')::bigint DESC LIMIT 1", &[&s.tenant(), &s.repository(), &spec.manifest_sha256, &spec.binding.to_string(), &subject]).await?.get(0);
        if latest != key {
            return Err(Error::RestoreDecisionConflict);
        }
        let result = RestoreAcquisitionContext {
            version: 1,
            tenant: s.tenant().into(),
            repository: s.repository(),
            epoch,
            decision_key: key.into(),
            decision_sha256: restore_digest(&receipt)?,
            decision: spec,
            binding_specification: ctx.specification,
            publication_authorized: false,
        };
        context(&tx, auth, epoch, Permission::RepositoryWrite).await?;
        tx.commit().await?;
        Ok(json!(result))
    }
}
