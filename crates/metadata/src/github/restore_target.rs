//! Prospective discovery configuration when no GitHub binding survived the backup.
//! A revoked row is a permanently unusable configuration, not a claimed old session.
use super::*;
use serde::{Deserialize, Serialize};
const KIND: &str = "github.restore.target.v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreTargetSpec {
    pub version: u8,
    pub manifest_sha256: String,
    pub target: Uuid,
    pub binding: BindingSpec,
    pub evidence_sha256: String,
    pub prospective_target_confirmed: bool,
}
impl RestoreTargetSpec {
    pub fn validate(&self) -> Result<()> {
        uuid(self.target)?;
        self.binding.validate()?;
        restore::digest_valid(&self.manifest_sha256)?;
        restore::digest_valid(&self.evidence_sha256)?;
        if self.version != 1 || !self.prospective_target_confirmed {
            return Err(Error::Invalid("prospective restore target confirmation"));
        }
        Ok(())
    }
}
impl Metadata {
    pub async fn register_github_restore_target(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreTargetSpec,
    ) -> Result<Value> {
        spec.validate()?;
        let req = request(auth, cmd, KIND, json!([1, spec]))?;
        let m = match self.begin(req).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let (_, database) = restore::quarantine_receipt(&m.tx, s, &spec.manifest_sha256).await?;
        let quarantine = restore_reconnect::quarantine_scope(&m.tx, s)
            .await?
            .ok_or(Error::HostRecoveryRequired)?;
        if !restore_reconciliation_required(&m.tx, s).await? {
            return Err(Error::HostRecoveryRequired);
        }
        if m.tx
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM th_github_bindings WHERE tenant=$1 AND repository=$2)",
                &[&s.tenant(), &s.repository()],
            )
            .await?
            .get::<_, bool>(0)
        {
            return Err(Error::Invalid(
                "restore target requires no retained bindings",
            ));
        }
        restore_reconnect::bridge(&m.tx, s, &spec.binding).await?;
        // Never create an active interval or a configured event. The existing
        // immutable revoked-state trigger also prevents reusing this target ID.
        m.tx.execute("INSERT INTO th_github_bindings(tenant,repository,binding,epoch,bridge,specification,state,principal,revoked_at) VALUES($1,$2,$3,$4,$5,$6,'revoked',$7,clock_timestamp())",&[&s.tenant(),&s.repository(),&spec.target,&cmd.epoch,&spec.binding.bridge,&json!(spec.binding),&auth.principal()]).await?;
        m.complete(json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"epoch":cmd.epoch,"key":cmd.key,"specification":spec,
            "restored_database":database,"quarantine_sha256":quarantine,"outcome":"prospective_recovery_target_registered","binding":spec.target,
            "binding_state":"revoked","historical_connection_asserted":false,"remote_history_empty_asserted":false,"publication_authorized":false,"reconciliation_complete":false}),vec![]).await
    }
    pub async fn github_restore_target(
        &mut self,
        auth: &Authorization,
        key: &str,
    ) -> Result<Value> {
        crate::identifier(key, 128)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        auth.check_transaction(&tx, s, auth.principal(), Permission::RepositoryWrite)
            .await?;
        let v:Value=tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",&[&s.tenant(),&s.repository(),&key,&KIND]).await?.ok_or(Error::NotFound)?.get(0);
        tx.commit().await?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_target_is_explicit_configuration_not_supplied_recovery_success() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-target-v1.json"))
                .unwrap();
        let cmd: WireCommand = serde_json::from_value(v["request"].clone()).unwrap();
        assert_eq!(cmd.permission(), Permission::RepositoryWrite);
        let WireCommand::RestoreTargetRegister {
            specification: spec,
            ..
        } = cmd
        else {
            panic!()
        };
        spec.validate().unwrap();
        assert_eq!(restore_digest(&spec).unwrap(), v["specification_sha256"]);
        let mut bad = (*spec).clone();
        bad.prospective_target_confirmed = false;
        assert!(bad.validate().is_err());
        for field in [
            "publication_authorized",
            "historical_connection_asserted",
            "remote_history_empty_asserted",
            "state",
        ] {
            let mut bad = v["request"].clone();
            bad["specification"][field] = json!(true);
            assert!(serde_json::from_value::<WireCommand>(bad).is_err());
        }
    }
}
