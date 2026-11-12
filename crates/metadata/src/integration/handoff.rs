//! Freeze native writes and retain the exact input for a reverse authority transfer.
mod projection;
use super::{capture_refs, ref_name, request, *};
use crate::identifier;
pub(crate) use projection::verified_projection;
pub use projection::{HandoffProjectionSpec, HandoffProjectionWork};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const FREEZE: &str = "native.handoff.freeze.v1";
const RESUME: &str = "native.handoff.resume.v1";

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffRef {
    pub name: String,
    pub head: String,
    pub snapshot: String,
    pub generation: i64,
    pub epoch: i64,
    pub policy: String,
}

impl From<RefState> for HandoffRef {
    fn from(value: RefState) -> Self {
        Self {
            name: value.name,
            head: RevisionRef::Native(value.head).to_string(),
            snapshot: value.snapshot.to_string(),
            generation: value.generation,
            epoch: value.epoch,
            policy: value.policy.to_string(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FrozenSnapshot {
    pub version: u8,
    pub tenant: String,
    pub repository: Uuid,
    pub authority: String,
    pub epoch: i64,
    pub freeze_key: String,
    pub refs: Vec<HandoffRef>,
}

impl FrozenSnapshot {
    fn fingerprint(&self) -> Result<String> {
        let encoded = serde_json::to_vec(self).map_err(|_| Error::NativeHandoffMismatch)?;
        let mut digest = Sha256::new();
        digest.update(b"treeharbor-native-handoff\0v1\0");
        digest.update(encoded);
        Ok(ObjectId::from_bytes(digest.finalize().into()).to_string())
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffFreeze {
    pub version: u8,
    pub state: String,
    pub snapshot_sha256: String,
    pub snapshot: FrozenSnapshot,
}

impl HandoffFreeze {
    pub fn validate(&self, scope: &Scope, epoch: i64, key: &str) -> Result<()> {
        let snapshot = &self.snapshot;
        if self.version != 1
            || self.state != "frozen"
            || snapshot.version != 1
            || snapshot.tenant != scope.tenant()
            || snapshot.repository != scope.repository()
            || snapshot.authority != "standalone"
            || snapshot.epoch != epoch
            || epoch <= 0
            || snapshot.freeze_key != key
            || self.snapshot_sha256 != snapshot.fingerprint()?
            || snapshot.refs.is_empty()
            || snapshot.refs.len() > MAX_REF_SNAPSHOT
            || snapshot
                .refs
                .windows(2)
                .any(|refs| refs[0].name >= refs[1].name)
        {
            return Err(Error::NativeHandoffMismatch);
        }
        identifier(key, 128)?;
        for tip in &snapshot.refs {
            ref_name(&tip.name)?;
            if tip.epoch != epoch
                || tip.generation <= 0
                || !matches!(tip.head.parse(), Ok(rev @ RevisionRef::Native(_)) if rev.to_string()==tip.head)
                || !matches!(tip.snapshot.parse::<ObjectId>(), Ok(id) if id.to_string()==tip.snapshot)
                || !matches!(tip.policy.parse::<ObjectId>(), Ok(id) if id.to_string()==tip.policy)
            {
                return Err(Error::NativeHandoffMismatch);
            }
        }
        Ok(())
    }
}

pub(crate) async fn quiescent(tx: &Transaction<'_>, scope: &Scope) -> Result<()> {
    let busy: bool = tx.query_one(r#"
        SELECT EXISTS(SELECT 1 FROM th_workspaces WHERE tenant=$1 AND repository=$2 AND state<>'deleted')
          OR EXISTS(SELECT 1 FROM th_agent_runs WHERE tenant=$1 AND repository=$2
                    AND state IN ('requested','launching','running','stopping'))
          OR EXISTS(SELECT 1 FROM th_managed_runs WHERE tenant=$1 AND repository=$2 AND NOT removed)
          OR EXISTS(SELECT 1 FROM th_remote_tasks WHERE tenant=$1 AND repository=$2 AND state IN ('queued','leased'))
          OR EXISTS(SELECT 1 FROM th_native_queue WHERE tenant=$1 AND repository=$2 AND state IN ('preparing','checking'))
          OR EXISTS(SELECT 1 FROM th_outbox WHERE tenant=$1 AND repository=$2 AND state='running' AND lease_until>clock_timestamp())
        "#, &[&scope.tenant(), &scope.repository()]).await?.get(0);
    if busy {
        return Err(Error::NativeHandoffBusy);
    }
    Ok(())
}

pub(crate) async fn retire_frozen_work(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
) -> Result<()> {
    tx.execute(
        r#"
        UPDATE th_native_queue SET state='stale',reason='authority_epoch_changed'
        WHERE tenant=$1 AND repository=$2 AND epoch=$3 AND state='queued'
        "#,
        &[&scope.tenant(), &scope.repository(), &epoch],
    )
    .await?;
    tx.execute(r#"
        UPDATE th_outbox SET state='done',worker=NULL,lease_until=NULL,completed_at=clock_timestamp()
        WHERE tenant=$1 AND repository=$2 AND state<>'done'
        "#, &[&scope.tenant(), &scope.repository()]).await?;
    Ok(())
}

pub(crate) async fn current(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
    key: &str,
) -> Result<HandoffFreeze> {
    identifier(key, 128)?;
    let row = tx
        .query_one(
            "SELECT authority,state,epoch FROM th_repositories WHERE tenant=$1 AND repository=$2",
            &[&scope.tenant(), &scope.repository()],
        )
        .await?;
    if row.get::<_, &str>(0) != "standalone"
        || row.get::<_, &str>(1) != "frozen"
        || row.get::<_, i64>(2) != epoch
    {
        return Err(Error::Fenced);
    }
    let row = tx.query_opt(
        "SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",
        &[&scope.tenant(), &scope.repository(), &key, &FREEZE],
    ).await?.ok_or(Error::NativeHandoffMismatch)?;
    let freeze: HandoffFreeze =
        serde_json::from_value(row.get(0)).map_err(|_| Error::SchemaMismatch)?;
    freeze.validate(scope, epoch, key)?;
    Ok(freeze)
}

async fn check_refs(tx: &Transaction<'_>, scope: &Scope, freeze: &HandoffFreeze) -> Result<()> {
    let refs: Vec<_> = capture_refs(tx, scope, freeze.snapshot.epoch)
        .await?
        .into_iter()
        .map(HandoffRef::from)
        .collect();
    if refs != freeze.snapshot.refs {
        return Err(Error::NativeHandoffMismatch);
    }
    Ok(())
}

pub(crate) async fn export_context(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    key: &str,
    head: ObjectId,
) -> Result<(ObjectId, ObjectId, String)> {
    let scope = auth.scope();
    auth.check_transaction(tx, scope, auth.principal(), Permission::Read)
        .await?;
    let freeze = current(tx, scope, epoch, key).await?;
    let revision = RevisionRef::Native(head).to_string();
    let tip = freeze
        .snapshot
        .refs
        .iter()
        .find(|tip| tip.head == revision)
        .ok_or(Error::NativeHandoffMismatch)?;
    let snapshot: ObjectId = tip.snapshot.parse()?;
    let policy: ObjectId = tip.policy.parse()?;
    let row = tx.query_opt(r#"
        SELECT r.pin_holder FROM th_native_refs r JOIN th_bases b ON (b.tenant,b.repository)=(r.tenant,r.repository)
        WHERE r.tenant=$1 AND r.repository=$2 AND r.name=$3 AND r.head=$4 AND r.generation=$5
            AND r.epoch=$6 AND r.policy=$7 AND b.revision=$8 AND b.snapshot=$9
        "#, &[&scope.tenant(), &scope.repository(), &tip.name, &head.as_bytes().as_slice(), &tip.generation,
            &epoch, &policy.as_bytes().as_slice(), &revision, &snapshot.as_bytes().as_slice()])
        .await?.ok_or(Error::NativeHandoffMismatch)?;
    Ok((head, snapshot, row.get(0)))
}

impl Metadata {
    pub async fn freeze_native_handoff(
        &mut self,
        auth: &Authorization,
        command: &Command,
    ) -> Result<Value> {
        if command.epoch == i64::MAX {
            return Err(Error::Invalid("handoff epoch"));
        }
        let mutation = match self
            .begin(request(auth, command, FREEZE, json!([1]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        quiescent(tx, scope).await?;
        let refs = capture_refs(tx, scope, command.epoch)
            .await?
            .into_iter()
            .map(HandoffRef::from)
            .collect();
        let snapshot = FrozenSnapshot {
            version: 1,
            tenant: scope.tenant().into(),
            repository: scope.repository(),
            authority: "standalone".into(),
            epoch: command.epoch,
            freeze_key: command.key.clone(),
            refs,
        };
        let freeze = HandoffFreeze {
            version: 1,
            state: "frozen".into(),
            snapshot_sha256: snapshot.fingerprint()?,
            snapshot,
        };
        freeze.validate(scope, command.epoch, &command.key)?;
        let changed = tx.execute(r#"
            UPDATE th_repositories SET state='frozen'
            WHERE tenant=$1 AND repository=$2 AND authority='standalone' AND state='active' AND epoch=$3
            "#, &[&scope.tenant(), &scope.repository(), &command.epoch]).await?;
        if changed != 1 {
            return Err(Error::Fenced);
        }
        mutation.complete(json!(freeze), vec![]).await
    }

    pub async fn frozen_native_snapshot(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        read_authorized(&tx, auth).await?;
        let freeze = current(&tx, auth.scope(), epoch, key).await?;
        check_refs(&tx, auth.scope(), &freeze).await?;
        read_authorized(&tx, auth).await?;
        tx.commit().await?;
        Ok(json!(freeze))
    }

    /// Abort a transfer whose external destination is still fenced. Advance
    /// the epoch so a previously stopped writer cannot reuse its old grant.
    pub async fn resume_native_handoff(
        &mut self,
        auth: &Authorization,
        command: &Command,
        freeze_key: &str,
        target_stays_fenced: bool,
    ) -> Result<Value> {
        identifier(freeze_key, 128)?;
        let next = command
            .epoch
            .checked_add(1)
            .filter(|_| target_stays_fenced)
            .ok_or(Error::Invalid("handoff resume"))?;
        let mutation = match self
            .begin_frozen(request(auth, command, RESUME, json!([1, freeze_key]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let freeze = current(tx, scope, command.epoch, freeze_key).await?;
        check_refs(tx, scope, &freeze).await?;
        quiescent(tx, scope).await?;
        let changed = tx
            .execute(
                r#"
            UPDATE th_native_refs SET epoch=$4 WHERE tenant=$1 AND repository=$2 AND epoch=$3
            "#,
                &[&scope.tenant(), &scope.repository(), &command.epoch, &next],
            )
            .await?;
        if changed != freeze.snapshot.refs.len() as u64 {
            return Err(Error::NativeHandoffMismatch);
        }
        retire_frozen_work(tx, scope, command.epoch).await?;
        let changed = tx.execute(r#"
            UPDATE th_repositories SET state='active',epoch=$4
            WHERE tenant=$1 AND repository=$2 AND authority='standalone' AND state='frozen' AND epoch=$3
            "#, &[&scope.tenant(), &scope.repository(), &command.epoch, &next]).await?;
        if changed != 1 {
            return Err(Error::Fenced);
        }
        mutation.complete(json!({"version":1,"state":"active","authority":"standalone","previous_epoch":command.epoch,
            "epoch":next,"freeze_key":freeze_key,"snapshot_sha256":freeze.snapshot_sha256}), vec![]).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frozen_snapshot_matches_v1_vector() {
        let raw = include_bytes!("../../tests/fixtures/native-handoff-freeze-v1.json");
        let freeze: HandoffFreeze = serde_json::from_slice(raw).unwrap();
        let scope = Scope::new(
            "fixture",
            "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap(),
        )
        .unwrap();
        freeze.validate(&scope, 4, "fixture-native-freeze").unwrap();
        assert_eq!(
            freeze.snapshot_sha256,
            "sha256:5726d4c852496b1824745e79bca134d3b145532c07e833e85240d7147d72a380"
        );
        assert_eq!(json!(freeze), serde_json::from_slice::<Value>(raw).unwrap());
        let mut modified: HandoffFreeze = serde_json::from_slice(raw).unwrap();
        modified.snapshot.refs[1].generation += 1;
        assert!(matches!(
            modified.validate(&scope, 4, "fixture-native-freeze"),
            Err(Error::NativeHandoffMismatch)
        ));
    }
}
