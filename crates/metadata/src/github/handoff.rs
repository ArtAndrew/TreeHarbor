//! Activate Connected only after a trusted bridge verifies the archived destination.
use super::{
    archive_observation::{CLOCK_SKEW_SECONDS, FRESH_SECONDS, REQUEST_SECONDS, clock},
    archive_proof::operation_receipt,
    restore_reconnect::{available, bridge},
    *,
};
use crate::{
    identifier,
    integration::{
        HandoffProjectionSpec,
        handoff::{quiescent, retire_frozen_work, verified_projection},
        ref_name,
    },
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use treeharbor_core::ObjectId;

const PLAN: &str = "github.handoff.plan.v1";
const OBSERVE: &str = "github.handoff.observe.v1";
const ACTIVATE: &str = "github.handoff.activate.v1";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffTargetSpec {
    pub version: u8,
    pub projection_key: String,
    pub binding: Uuid,
    pub specification: BindingSpec,
    pub repository_node_id: String,
    pub default_branch: String,
}

impl HandoffTargetSpec {
    fn validate(&self) -> Result<()> {
        identifier(&self.projection_key, 128)?;
        uuid(self.binding)?;
        self.specification.validate()?;
        ref_name(&self.default_branch)?;
        if self.version != 1
            || self.repository_node_id.is_empty()
            || self.repository_node_id.len() > 256
            || !self
                .repository_node_id
                .bytes()
                .all(|b| b.is_ascii_graphic())
        {
            return Err(Error::Invalid("handoff target"));
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct HandoffTargetPlan {
    pub version: u8,
    pub tenant: String,
    pub repository: Uuid,
    pub key: String,
    pub epoch: i64,
    pub freeze_key: String,
    pub snapshot_sha256: String,
    pub target: HandoffTargetSpec,
    pub observe_after_unix: u64,
    pub expires_at_unix: u64,
    pub observation_key: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObservationReceipt {
    version: u8,
    state: String,
    plan: HandoffTargetPlan,
    observation: ArchiveObservation,
}

fn observation_key(plan_key: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-github-handoff-observation\0v1\0");
    hash.update(plan_key.as_bytes());
    let digest = ObjectId::from_bytes(hash.finalize().into());
    format!("handoff-observed-{}", digest.hex())
}

fn request(auth: &Authorization, command: &Command, kind: &str, data: Value) -> Result<Request> {
    auth.context(command.key.clone(), Authority::Standalone, command.epoch)
        .request(kind, data)
}

async fn target_available(
    tx: &Transaction<'_>,
    scope: &Scope,
    spec: &HandoffTargetSpec,
) -> Result<()> {
    if restore_reconciliation_required(tx, scope).await? {
        return Err(Error::HostRecoveryRequired);
    }
    bridge(tx, scope, &spec.specification).await?;
    available(tx, scope, spec.binding).await
}

async fn live_plan(
    tx: &Transaction<'_>,
    scope: &Scope,
    epoch: i64,
    key: &str,
) -> Result<(HandoffTargetPlan, HandoffProjectionSpec)> {
    let plan: HandoffTargetPlan =
        serde_json::from_value(operation_receipt(tx, scope, key, PLAN).await?)
            .map_err(|_| Error::SchemaMismatch)?;
    if plan.version != 1
        || plan.tenant != scope.tenant()
        || plan.repository != scope.repository()
        || plan.key != key
        || plan.observation_key != observation_key(key)
    {
        return Err(Error::SchemaMismatch);
    }
    if plan.epoch != epoch {
        return Err(Error::Fenced);
    }
    let projection = verified_projection(tx, scope, epoch, &plan.target.projection_key).await?;
    if plan.freeze_key != projection.freeze_key
        || plan.snapshot_sha256 != projection.snapshot_sha256
        || clock(tx).await? >= plan.expires_at_unix
    {
        return Err(Error::NativeHandoffMismatch);
    }
    Ok((plan, projection))
}

fn fresh(plan: &HandoffTargetPlan, observation: &ArchiveObservation, now: u64) -> bool {
    now < plan.expires_at_unix
        && observation.started_at_unix >= plan.observe_after_unix
        && observation.finished_at_unix >= observation.started_at_unix
        && observation.finished_at_unix - observation.started_at_unix <= FRESH_SECONDS
        && observation.finished_at_unix <= now + CLOCK_SKEW_SECONDS
        && observation.finished_at_unix >= now.saturating_sub(FRESH_SECONDS)
}

fn check_observation(
    plan: &HandoffTargetPlan,
    projection: &HandoffProjectionSpec,
    observation: &ArchiveObservation,
    now: u64,
) -> Result<()> {
    observation.tags.validate(b"tags")?;
    observation.open_pulls.validate(b"open-pulls")?;
    let target = &plan.target;
    if observation.version != 1
        || observation.request_key != plan.key
        || observation.binding != target.binding
        || observation.app_id != target.specification.app_id
        || observation.upstream != target.specification.upstream
        || observation.repository_node_id != target.repository_node_id
        || observation.default_branch != target.default_branch
        || !observation.token_revoked
        || observation.heads != projection.refs
        || !fresh(plan, observation, now)
    {
        return Err(Error::ArchiveObservationMismatch);
    }
    Ok(())
}

async fn bridge_plan(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    key: &str,
) -> Result<(HandoffTargetPlan, HandoffProjectionSpec)> {
    let scope = auth.scope();
    auth.check_transaction(tx, scope, auth.principal(), Permission::HostRead)
        .await?;
    let (plan, projection) = live_plan(tx, scope, epoch, key).await?;
    if plan.target.specification.bridge != auth.principal() {
        return Err(Error::Forbidden);
    }
    target_available(tx, scope, &plan.target).await?;
    Ok((plan, projection))
}

impl Metadata {
    pub async fn begin_github_handoff(
        &mut self,
        auth: &Authorization,
        command: &Command,
        spec: &HandoffTargetSpec,
    ) -> Result<Value> {
        spec.validate()?;
        if command.epoch == i64::MAX {
            return Err(Error::Invalid("handoff epoch"));
        }
        let mutation = match self
            .begin_frozen(request(auth, command, PLAN, json!([1, spec]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let projection =
            verified_projection(tx, scope, command.epoch, &spec.projection_key).await?;
        if !projection
            .refs
            .contains_key(&format!("refs/heads/{}", spec.default_branch))
        {
            return Err(Error::NativeHandoffMismatch);
        }
        target_available(tx, scope, spec).await?;
        let now = clock(tx).await?;
        let plan = HandoffTargetPlan {
            version: 1,
            tenant: scope.tenant().into(),
            repository: scope.repository(),
            key: command.key.clone(),
            epoch: command.epoch,
            freeze_key: projection.freeze_key,
            snapshot_sha256: projection.snapshot_sha256,
            target: spec.clone(),
            observe_after_unix: now + 1,
            expires_at_unix: now + REQUEST_SECONDS,
            observation_key: observation_key(&command.key),
        };
        mutation.complete(json!(plan), vec![]).await
    }

    pub async fn github_handoff_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let (plan, _) = bridge_plan(&tx, auth, epoch, key).await?;
        tx.commit().await?;
        Ok(json!(plan))
    }

    pub async fn record_github_handoff_observation(
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
            .begin_frozen(request(auth, &command, OBSERVE, json!([1, observation]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let (plan, projection) = bridge_plan(tx, auth, epoch, &observation.request_key).await?;
        check_observation(&plan, &projection, observation, clock(tx).await?)?;
        let result = json!({"version":1,"state":"observed","plan":plan,"observation":observation});
        mutation.write_completion(&result, &[]).await?;
        if !fresh(&plan, observation, clock(&mutation.tx).await?) {
            return Err(Error::ArchiveObservationMismatch);
        }
        mutation.tx.commit().await?;
        Ok(result)
    }

    pub async fn activate_github_handoff(
        &mut self,
        auth: &Authorization,
        command: &Command,
        plan_key: &str,
        keep_target_archived: bool,
    ) -> Result<Value> {
        identifier(plan_key, 128)?;
        let next = command
            .epoch
            .checked_add(1)
            .filter(|_| keep_target_archived)
            .ok_or(Error::Invalid("handoff activation"))?;
        let mutation = match self
            .begin_frozen(request(auth, command, ACTIVATE, json!([1, plan_key]))?)
            .await?
        {
            Begin::Replay(value) => return Ok(value),
            Begin::Fresh(mutation) => mutation,
        };
        let tx = mutation.transaction();
        let scope = auth.scope();
        let (plan, projection) = live_plan(tx, scope, command.epoch, plan_key).await?;
        let receipt: ObservationReceipt = serde_json::from_value(
            operation_receipt(tx, scope, &plan.observation_key, OBSERVE).await?,
        )
        .map_err(|_| Error::SchemaMismatch)?;
        if receipt.version != 1 || receipt.state != "observed" || receipt.plan != plan {
            return Err(Error::ArchiveObservationMismatch);
        }
        let observation = &receipt.observation;
        check_observation(&plan, &projection, observation, clock(tx).await?)?;
        quiescent(tx, scope).await?;
        let target = &plan.target;
        target_available(tx, scope, target).await?;
        tx.execute(r#"
            INSERT INTO th_github_bindings(tenant,repository,binding,epoch,bridge,specification,state,principal)
            VALUES ($1,$2,$3,$4,$5,$6,'active',$7)
            "#, &[&scope.tenant(), &scope.repository(), &target.binding, &next, &target.specification.bridge,
                &json!(target.specification), &auth.principal()]).await?;
        // Native refs and their pins remain as historical records. No native
        // validation or review is copied into the new GitHub binding.
        retire_frozen_work(tx, scope, command.epoch).await?;
        let changed = tx.execute(r#"
            UPDATE th_repositories SET authority='connected',state='active',epoch=$4
            WHERE tenant=$1 AND repository=$2 AND authority='standalone' AND state='frozen' AND epoch=$3
            "#, &[&scope.tenant(), &scope.repository(), &command.epoch, &next]).await?;
        if changed != 1 {
            return Err(Error::Fenced);
        }
        let result = json!({
            "version":1,"state":"activated","authority":"connected","tenant":scope.tenant(),
            "repository":scope.repository(),"key":command.key,"previous_epoch":command.epoch,"epoch":next,
            "binding":target.binding,"plan_key":plan.key,"observation_key":plan.observation_key,
            "snapshot_sha256":plan.snapshot_sha256,"target_archived":true,"publication_ready":false,
        });
        mutation
            .write_completion(
                &result,
                &[Event {
                    kind: "github.binding.configured".into(),
                    payload: json!({"binding":target.binding,"epoch":next}),
                }],
            )
            .await?;
        if !fresh(&plan, observation, clock(&mutation.tx).await?) {
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
    fn github_handoff_matches_v1_wire_and_freshness() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/github-handoff-target-v1.json"
        ))
        .unwrap();
        let command =
            |name: &str| -> WireCommand { serde_json::from_value(fixture[name].clone()).unwrap() };
        assert_eq!(command("plan").permission(), Permission::RepositoryWrite);
        assert_eq!(command("context").permission(), Permission::HostRead);
        assert_eq!(command("observe").permission(), Permission::HostRead);
        assert_eq!(
            command("activate").permission(),
            Permission::RepositoryWrite
        );
        let WireCommand::HandoffPlan { key, epoch, target } = command("plan") else {
            panic!("expected handoff plan")
        };
        let WireCommand::HandoffObserve { observation, .. } = command("observe") else {
            panic!("expected handoff observation")
        };
        target.validate().unwrap();
        let projection = HandoffProjectionSpec {
            version: 1,
            freeze_key: "fixture-freeze".into(),
            snapshot_sha256: ObjectId::from_bytes([7; 32]).to_string(),
            refs: observation.heads.clone(),
        };
        projection.validate().unwrap();
        let plan = HandoffTargetPlan {
            version: 1,
            tenant: "tenant".into(),
            repository: Uuid::from_u128(1),
            epoch,
            freeze_key: projection.freeze_key.clone(),
            snapshot_sha256: projection.snapshot_sha256.clone(),
            target: *target,
            observe_after_unix: 2000000001,
            expires_at_unix: 2000000180,
            observation_key: observation_key(&key),
            key,
        };
        assert_eq!(json!(plan.observation_key), fixture["observation_key"]);
        check_observation(&plan, &projection, &observation, 2000000003).unwrap();
        assert!(matches!(
            check_observation(&plan, &projection, &observation, 2000000123),
            Err(Error::ArchiveObservationMismatch)
        ));
        assert!(!fresh(&plan, &observation, plan.expires_at_unix));
    }
}
