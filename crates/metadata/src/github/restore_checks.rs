//! Exact host check observations do not restore validation/publication authority.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_github_contract::{
    check_publication::Target,
    restore_check::{CheckIdentity, RestoreCheckReport},
};
const KIND: &str = "github.restore.check.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreCheckContext {
    pub version: u8,
    pub context: RestoreContext,
    pub context_sha256: String,
    pub operation: Uuid,
    pub identity: CheckIdentity,
    pub target: Target,
    pub publication_authorized: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestoreCheckEvidence {
    pub version: u8,
    pub binding: Uuid,
    pub manifest_sha256: String,
    pub context_sha256: String,
    pub operation: Uuid,
    pub identity: CheckIdentity,
    pub report: RestoreCheckReport,
}
impl RestoreCheckEvidence {
    fn validate(&self) -> Result<()> {
        uuid(self.binding)?;
        uuid(self.operation)?;
        self.identity
            .validate()
            .map_err(|_| Error::Invalid("restore check identity"))?;
        restore::digest_valid(&self.manifest_sha256)?;
        restore::digest_valid(&self.context_sha256)?;
        if self.version != 1 {
            return Err(Error::Invalid("restore check evidence"));
        }
        Ok(())
    }
}
async fn retained(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    binding: Uuid,
    manifest: &str,
    operation: Uuid,
    identity: &CheckIdentity,
) -> Result<RestoreCheckContext> {
    uuid(operation)?;
    identity
        .validate()
        .map_err(|_| Error::Invalid("restore check identity"))?;
    let ctx =
        restore::retained_context(tx, auth, epoch, binding, manifest, Permission::HostRead).await?;
    if !ctx
        .effects
        .iter()
        .any(|e| e.kind == RestoreEffectKind::Check && e.id == operation)
    {
        return Err(Error::NotFound);
    }
    let row = check_publications::load(tx, auth.scope(), operation).await?;
    if row.get::<_, Uuid>("binding") != binding || row.get::<_, i64>("epoch") != epoch {
        return Err(Error::Fenced);
    }
    let target = check_publications::target(&row)?;
    if target.app_id != ctx.specification.app_id
        || target.source.pull().repository_id != ctx.specification.upstream.repository_id
    {
        return Err(Error::SchemaMismatch);
    }
    if let Some(value) = row.get::<_, Option<Value>>("latest_report") {
        let report: treeharbor_github_contract::check_publication::Report =
            serde_json::from_value(value).map_err(|_| Error::SchemaMismatch)?;
        report
            .validate(&target)
            .map_err(|_| Error::SchemaMismatch)?;
        if let Some(check) = report.check
            && (check.id != identity.id
                || check
                    .node_id
                    .as_ref()
                    .is_some_and(|n| *n != identity.node_id))
        {
            return Err(Error::HostPublicationState);
        }
    }
    Ok(RestoreCheckContext {
        version: 1,
        context_sha256: restore_digest(&ctx)?,
        context: ctx,
        operation,
        identity: identity.clone(),
        target,
        publication_authorized: false,
    })
}
impl Metadata {
    pub async fn github_restore_check_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        binding: Uuid,
        manifest: &str,
        operation: Uuid,
        identity: &CheckIdentity,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let result = retained(&tx, auth, epoch, binding, manifest, operation, identity).await?;
        context(&tx, auth, epoch, Permission::HostRead).await?;
        tx.commit().await?;
        Ok(json!(result))
    }
    pub async fn record_github_restore_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreCheckEvidence,
    ) -> Result<Value> {
        spec.validate()?;
        let m = match self
            .begin(request(auth, cmd, KIND, json!([1, spec]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let ctx = retained(
            &m.tx,
            auth,
            cmd.epoch,
            spec.binding,
            &spec.manifest_sha256,
            spec.operation,
            &spec.identity,
        )
        .await?;
        if ctx.context_sha256 != spec.context_sha256 {
            return Err(Error::RestoreContextChanged);
        }
        let r = &spec.report;
        let classification = r
            .classification(&ctx.target, &spec.identity)
            .map_err(|_| Error::Invalid("restore check report"))?;
        if r.installation_id != ctx.context.specification.upstream.installation_id {
            return Err(Error::HostPublicationState);
        }
        let now: i64 =
            m.tx.query_one(
                "SELECT (extract(epoch FROM clock_timestamp())*1000)::bigint",
                &[],
            )
            .await?
            .get(0);
        if r.finished_at_unix_ms > now.saturating_add(5000)
            || r.finished_at_unix_ms < now.saturating_sub(120_000)
        {
            return Err(Error::Invalid("restore check capture time"));
        }
        let receipt = json!({"version":1,"key":cmd.key,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),"epoch":cmd.epoch,
            "recorded_by":auth.principal(),"specification":spec,"retained_target":ctx.target,"capture_consistent":r.before==r.after,
            "classification":classification,"metadata_matches":classification=="matches_retained_target",
            "validation_authorized":false,"source_mapping_verified":false,"reconciliation_complete":false,"publication_authorized":false});
        m.complete(receipt, vec![]).await
    }
    pub async fn github_restore_check(&mut self, auth: &Authorization, key: &str) -> Result<Value> {
        crate::identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        let value:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",&[&s.tenant(),&s.repository(),&key,&KIND]).await?.ok_or(Error::NotFound)?.get(0);
        auth.check_transaction(&tx, s, auth.principal(), Permission::Read)
            .await?;
        tx.commit().await?;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_check_wire_preserves_identity_and_rejects_supplied_validation() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-check-v1.json"))
                .unwrap();
        let wire: WireCommand = serde_json::from_value(v["request"].clone()).unwrap();
        assert_eq!(wire.permission(), Permission::HostRead);
        let WireCommand::RestoreCheckRecord { specification, .. } = wire else {
            panic!()
        };
        specification.validate().unwrap();
        assert_eq!(json!(specification), v["request"]["specification"]);
        assert_eq!(
            restore_digest(&specification).unwrap(),
            v["specification_sha256"]
        );
        for field in [
            "metadata_matches",
            "validation_authorized",
            "publication_authorized",
        ] {
            let mut bad = v["request"].clone();
            bad["specification"][field] = json!(true);
            assert!(serde_json::from_value::<WireCommand>(bad).is_err());
        }
    }
}
