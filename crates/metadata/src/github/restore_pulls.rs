//! Trusted bridge observations of an exact retained PR intent, not admission.
use super::*;
use serde::{Deserialize, Serialize};
const KIND: &str = "github.restore.pull.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestorePullReport {
    pub version: u8,
    pub app_id: u64,
    pub installation_id: u64,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: PullSnapshot,
    pub after: PullSnapshot,
    pub token_revoked: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RestorePullEvidence {
    pub version: u8,
    pub binding: Uuid,
    pub manifest_sha256: String,
    pub context_sha256: String,
    pub operation: Uuid,
    pub identity: PullIdentity,
    pub report: RestorePullReport,
}
impl RestorePullEvidence {
    fn validate(&self) -> Result<()> {
        uuid(self.binding)?;
        uuid(self.operation)?;
        self.identity.validate()?;
        restore::digest_valid(&self.manifest_sha256)?;
        restore::digest_valid(&self.context_sha256)?;
        let r = &self.report;
        if self.version != 1
            || r.version != 1
            || r.started_at_unix_ms <= 0
            || r.finished_at_unix_ms < r.started_at_unix_ms
            || r.finished_at_unix_ms - r.started_at_unix_ms > 120_000
            || r.before.identity != self.identity
            || r.after.identity != self.identity
        {
            return Err(Error::Invalid("restore pull evidence"));
        }
        Ok(())
    }
}
impl Metadata {
    pub async fn record_github_restore_pull(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestorePullEvidence,
    ) -> Result<Value> {
        spec.validate()?;
        let m = match self
            .begin(request(auth, cmd, KIND, json!([1, spec]))?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let ctx = restore::retained_context(
            &m.tx,
            auth,
            cmd.epoch,
            spec.binding,
            &spec.manifest_sha256,
            Permission::HostRead,
        )
        .await?;
        if restore_digest(&ctx)? != spec.context_sha256 {
            return Err(Error::RestoreContextChanged);
        }
        let effect = ctx
            .effects
            .iter()
            .find(|e| e.kind == RestoreEffectKind::Pull && e.id == spec.operation)
            .ok_or(Error::NotFound)?;
        let target = effect.target.as_ref().ok_or(Error::SchemaMismatch)?;
        if effect
            .pull_number
            .is_some_and(|n| n as u64 != spec.identity.number)
            || effect
                .pull_node
                .as_ref()
                .is_some_and(|n| *n != spec.identity.node_id)
        {
            return Err(Error::HostPullState);
        }
        let r = &spec.report;
        if r.app_id != ctx.specification.app_id
            || r.installation_id != ctx.specification.upstream.installation_id
        {
            return Err(Error::HostPullState);
        }
        r.before.validate(target, &ctx.specification)?;
        r.after.validate(target, &ctx.specification)?;
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
            return Err(Error::Invalid("restore pull capture time"));
        }
        let consistent = r.before == r.after;
        let matches = consistent && target.matches(&r.after);
        let receipt = json!({"version":1,"key":cmd.key,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),
            "epoch":cmd.epoch,"recorded_by":auth.principal(),"specification":spec,"retained_target":target,
            "capture_consistent":consistent,"metadata_and_marker_match":matches,
            "classification":if !consistent {"changed_during_capture"} else if matches {"matches_retained_target"} else {"differs_from_retained_target"},
            "source_mapping_verified":false,"reconciliation_complete":false,"publication_authorized":false});
        m.complete(receipt, vec![]).await
    }
    pub async fn github_restore_pull(&mut self, auth: &Authorization, key: &str) -> Result<Value> {
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
    fn restore_pull_wire_retains_complete_capture_and_rejects_supplied_success() {
        let vector: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-pull-v1.json"))
                .unwrap();
        let wire: WireCommand = serde_json::from_value(vector["request"].clone()).unwrap();
        assert_eq!(wire.permission(), Permission::HostRead);
        let WireCommand::RestorePullRecord { specification, .. } = wire else {
            panic!()
        };
        specification.validate().unwrap();
        assert_eq!(json!(specification), vector["request"]["specification"]);
        assert_eq!(
            restore_digest(&specification).unwrap(),
            vector["specification_sha256"]
        );
        for field in [
            "metadata_and_marker_match",
            "publication_authorized",
            "source_mapping_verified",
        ] {
            let mut forged = vector["request"].clone();
            forged["specification"][field] = json!(true);
            assert!(serde_json::from_value::<WireCommand>(forged).is_err());
        }
        let mut invalid = *specification;
        invalid.report.after.identity.number += 1;
        assert!(invalid.validate().is_err());
    }
}
