//! Operator-assisted new binding admission. External assertions are not server proof.
use super::*;
use serde::{Deserialize, Serialize};

const PLAN: &str = "github.restore.reconnect.plan.v1";
const PROBE: &str = "github.restore.reconnect.probe.v1";
const CANCEL: &str = "github.restore.reconnect.cancel.v1";
const ACTIVATE: &str = "github.restore.reconnect.activate.v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreReconnectSpec {
    pub version: u8,
    pub manifest_sha256: String,
    pub source_binding: Uuid,
    pub new_binding: Uuid,
    pub binding: BindingSpec,
    pub ttl_seconds: u32,
    pub evidence_sha256: String,
    pub old_writers_and_runtimes_fenced: bool,
    pub old_credentials_cannot_mint: bool,
    pub old_tokens_revoked_or_expired: bool,
    pub prior_host_mutations_settled: bool,
    pub host_writes_controlled: bool,
    pub host_policy_reviewed: bool,
}
impl RestoreReconnectSpec {
    pub fn validate(&self) -> Result<()> {
        uuid(self.source_binding)?;
        uuid(self.new_binding)?;
        self.binding.validate()?;
        restore::digest_valid(&self.manifest_sha256)?;
        restore::digest_valid(&self.evidence_sha256)?;
        if self.version != 1
            || self.source_binding == self.new_binding
            || !(30..=3600).contains(&self.ttl_seconds)
            || !self.old_writers_and_runtimes_fenced
            || !self.old_credentials_cannot_mint
            || !self.old_tokens_revoked_or_expired
            || !self.prior_host_mutations_settled
            || !self.host_writes_controlled
            || !self.host_policy_reviewed
        {
            return Err(Error::Invalid("restore reconnect operator assertions"));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreReconnectProbe {
    pub version: u8,
    pub plan_key: String,
    pub plan_sha256: String,
    pub started_at_unix: u64,
    pub finished_at_unix: u64,
    pub observation: Observation,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreActivateSpec {
    pub version: u8,
    pub plan_key: String,
    pub probe_key: String,
    pub review: RestoreReviewSpec,
    pub assessment_sha256: String,
    pub external_assertions_still_valid: bool,
}

async fn clock(tx: &Transaction<'_>) -> Result<i64> {
    Ok(tx
        .query_one(
            "SELECT floor(extract(epoch FROM clock_timestamp()))::bigint",
            &[],
        )
        .await?
        .get(0))
}
// Include the whole quarantine set and staging identity. Never erase journals to
// clear a fence; a new restore invalidates every old completion receipt.
pub(super) async fn quarantine_scope(tx: &Transaction<'_>, s: &Scope) -> Result<Option<String>> {
    let rows=tx.query("SELECT key,principal,encode(request_digest,'hex'),result FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind='restore.quarantine.v1' ORDER BY key LIMIT 257",&[&s.tenant(),&s.repository()]).await?;
    if rows.is_empty() {
        return Ok(None);
    }
    if rows.len() > 256 {
        return Err(Error::RestoreContextLimit);
    }
    let identity=tx.query_one("SELECT current_database(),current_schema(),epoch FROM th_repositories WHERE tenant=$1 AND repository=$2",&[&s.tenant(),&s.repository()]).await?;
    let journals: Vec<_> = rows
        .into_iter()
        .map(|r| {
            json!([
                r.get::<_, String>(0),
                r.get::<_, String>(1),
                r.get::<_, String>(2),
                r.get::<_, Value>(3)
            ])
        })
        .collect();
    Ok(Some(restore_digest(&json!([
        1,
        s.tenant(),
        s.repository(),
        identity.get::<_, String>(0),
        identity.get::<_, String>(1),
        identity.get::<_, i64>(2),
        journals
    ]))?))
}
pub(super) async fn reconciliation_required(tx: &Transaction<'_>, s: &Scope) -> Result<bool> {
    let Some(hash) = quarantine_scope(tx, s).await? else {
        return Ok(false);
    };
    Ok(!tx.query_one("SELECT EXISTS(SELECT 1 FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind=$3 AND result->>'quarantine_sha256'=$4)",&[&s.tenant(),&s.repository(),&ACTIVATE,&hash]).await?.get::<_,bool>(0))
}
async fn bridge(tx: &Transaction<'_>, s: &Scope, spec: &BindingSpec) -> Result<()> {
    let row=tx.query_opt("SELECT p.kind,p.enabled,a.permissions FROM th_principals p JOIN th_repository_access a USING(tenant,principal) WHERE p.tenant=$1 AND p.principal=$2 AND a.repository=$3 FOR SHARE OF p,a",&[&s.tenant(),&spec.bridge,&s.repository()]).await?.ok_or(Error::Forbidden)?;
    if row.get::<_, String>(0) != "bridge"
        || !row.get::<_, bool>(1)
        || !row
            .get::<_, Vec<String>>(2)
            .iter()
            .any(|p| p == "host.read")
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}
async fn available(tx: &Transaction<'_>, s: &Scope, id: Uuid) -> Result<()> {
    if tx.query_opt("SELECT binding FROM th_github_bindings WHERE tenant=$1 AND repository=$2 AND (binding=$3 OR state='active') LIMIT 1",&[&s.tenant(),&s.repository(),&id]).await?.is_some() {return Err(Error::HostBindingState)}
    Ok(())
}
async fn contexts(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    spec: &RestoreReconnectSpec,
    permission: Permission,
) -> Result<Value> {
    // The owner authorizes all old contexts. A prospective bridge must not need
    // access to prior bridges' credentials to report its new connection probe.
    let s = auth.scope();
    let rows=tx.query("SELECT binding FROM th_github_bindings WHERE tenant=$1 AND repository=$2 ORDER BY binding LIMIT 257",&[&s.tenant(),&s.repository()]).await?;
    if rows.is_empty() {
        return Err(Error::HostRecoveryRequired);
    }
    if rows.len() > 256 {
        return Err(Error::RestoreContextLimit);
    }
    let mut selected = false;
    let mut result = Vec::new();
    for row in rows {
        let id: Uuid = row.get(0);
        let ctx = restore::retained_context(tx, auth, epoch, id, &spec.manifest_sha256, permission)
            .await?;
        if id == spec.source_binding {
            selected = true;
            if ctx.specification.app_id != spec.binding.app_id
                || ctx.specification.client_id != spec.binding.client_id
                || ctx.specification.upstream != spec.binding.upstream
            {
                return Err(Error::Invalid(
                    "restore reconnect upstream migration unsupported",
                ));
            }
        }
        result.push(json!({"binding":id,"context_sha256":restore_digest(&ctx)?}));
    }
    if !selected {
        return Err(Error::NotFound);
    }
    Ok(json!(result))
}
async fn receipt(tx: &Transaction<'_>, s: &Scope, key: &str, kind: &str) -> Result<Value> {
    crate::identifier(key, 128)?;
    Ok(tx.query_opt("SELECT result FROM th_operations WHERE tenant=$1 AND repository=$2 AND key=$3 AND kind=$4",&[&s.tenant(),&s.repository(),&key,&kind]).await?.ok_or(Error::NotFound)?.get(0))
}
fn plan_spec(p: &Value) -> Result<RestoreReconnectSpec> {
    let spec: RestoreReconnectSpec =
        serde_json::from_value(p["specification"].clone()).map_err(|_| Error::SchemaMismatch)?;
    spec.validate().map_err(|_| Error::SchemaMismatch)?;
    Ok(spec)
}
async fn live_plan(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    key: &str,
    permission: Permission,
) -> Result<(Value, RestoreReconnectSpec)> {
    context(tx, auth, epoch, permission).await?;
    let s = auth.scope();
    let p = receipt(tx, s, key, PLAN).await?;
    let spec = plan_spec(&p)?;
    if p["epoch"] != epoch
        || p["expires_at_unix"].as_i64().ok_or(Error::SchemaMismatch)? <= clock(tx).await?
        || p["quarantine_sha256"].as_str() != quarantine_scope(tx, s).await?.as_deref()
    {
        return Err(Error::Fenced);
    }
    if tx.query_one("SELECT EXISTS(SELECT 1 FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind=ANY($3) AND result->>'plan_key'=$4)",&[&s.tenant(),&s.repository(),&vec![CANCEL,ACTIVATE],&key]).await?.get::<_,bool>(0) {return Err(Error::Fenced)}
    bridge(tx, s, &spec.binding).await?;
    available(tx, s, spec.new_binding).await?;
    if permission == Permission::HostRead {
        if auth.principal() != spec.binding.bridge {
            return Err(Error::Forbidden);
        }
        // Do not impersonate the owner when authorizing a prospective bridge.
        // Each retained context is rechecked under owner authority at activation.
    } else if contexts(tx, auth, epoch, &spec, permission).await? != p["contexts"] {
        return Err(Error::RestoreContextChanged);
    }
    Ok((p, spec))
}
fn fresh_probe(
    p: &Value,
    spec: &RestoreReconnectSpec,
    probe: &RestoreReconnectProbe,
    now: i64,
) -> Result<()> {
    crate::identifier(&probe.plan_key, 128)?;
    restore::digest_valid(&probe.plan_sha256)?;
    probe.observation.validate(&spec.binding)?;
    let started =
        i64::try_from(probe.started_at_unix).map_err(|_| Error::Invalid("probe clock"))?;
    let finished =
        i64::try_from(probe.finished_at_unix).map_err(|_| Error::Invalid("probe clock"))?;
    if probe.version != 1
        || probe.plan_key != p["key"]
        || probe.plan_sha256 != restore_digest(p)?
        || started
            < p["observe_after_unix"]
                .as_i64()
                .ok_or(Error::SchemaMismatch)?
        || finished < started
        || finished - started > 120
        || finished > now + 5
        || finished < now - 120
        || probe.observation.observed_at_unix < probe.started_at_unix
        || probe.observation.observed_at_unix > probe.finished_at_unix
        || probe.observation.archived
        || probe.observation.disabled
    {
        return Err(Error::Invalid("fresh restore reconnect probe required"));
    }
    Ok(())
}
impl Metadata {
    pub async fn plan_github_restore_reconnect(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreReconnectSpec,
    ) -> Result<Value> {
        spec.validate()?;
        let req = request(auth, cmd, PLAN, json!([1, spec]))?;
        let m = match self.begin(req).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let s = auth.scope();
        let quarantine = quarantine_scope(&m.tx, s)
            .await?
            .ok_or(Error::HostRecoveryRequired)?;
        if !reconciliation_required(&m.tx, s).await? {
            return Err(Error::Invalid("restore already reconnected"));
        }
        available(&m.tx, s, spec.new_binding).await?;
        bridge(&m.tx, s, &spec.binding).await?;
        let old = contexts(&m.tx, auth, cmd.epoch, spec, Permission::RepositoryWrite).await?;
        let now = clock(&m.tx).await?;
        let value = json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"epoch":cmd.epoch,"key":cmd.key,"specification":spec,
            "quarantine_sha256":quarantine,"contexts":old,"created_at_unix":now,"observe_after_unix":now+1,"expires_at_unix":now+i64::from(spec.ttl_seconds),
            "operator_assertions_recorded":true,"external_fencing_verified":false,"publication_authorized":false});
        m.complete(value, vec![]).await
    }
    pub async fn github_restore_reconnect_context(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        let (p, _) = live_plan(&tx, auth, epoch, key, Permission::HostRead).await?;
        tx.commit().await?;
        Ok(p)
    }
    pub async fn record_github_restore_reconnect_probe(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreReconnectProbe,
    ) -> Result<Value> {
        let req = request(auth, cmd, PROBE, json!([1, spec]))?;
        let m = match self.begin(req).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (p, plan) =
            live_plan(&m.tx, auth, cmd.epoch, &spec.plan_key, Permission::HostRead).await?;
        fresh_probe(&p, &plan, spec, clock(&m.tx).await?)?;
        m.complete(json!({"version":1,"tenant":auth.scope().tenant(),"repository":auth.scope().repository(),"epoch":cmd.epoch,"key":cmd.key,"plan_key":spec.plan_key,"specification":spec,"external_fencing_verified":false,"publication_authorized":false}),vec![]).await
    }
    pub async fn cancel_github_restore_reconnect(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        key: &str,
    ) -> Result<Value> {
        crate::identifier(key, 128)?;
        let req = request(auth, cmd, CANCEL, json!([1, key]))?;
        let m = match self.begin(req).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, cmd.epoch, Permission::RepositoryWrite).await?;
        let p = receipt(&m.tx, auth.scope(), key, PLAN).await?;
        if p["epoch"] != cmd.epoch {
            return Err(Error::Fenced);
        }
        if m.tx.query_one("SELECT EXISTS(SELECT 1 FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind=$3 AND result->>'plan_key'=$4)",&[&auth.scope().tenant(),&auth.scope().repository(),&ACTIVATE,&key]).await?.get::<_,bool>(0) {return Err(Error::Invalid("activated plan cannot be cancelled; revoke the binding"))}
        m.complete(json!({"version":1,"plan_key":key,"outcome":"cancelled","publication_authorized":false}),vec![]).await
    }
    pub async fn github_restore_reconnect_status(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        key: &str,
    ) -> Result<Value> {
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        repository_lock(&tx, auth.scope(), false).await?;
        context(&tx, auth, epoch, Permission::RepositoryWrite).await?;
        let p = receipt(&tx, auth.scope(), key, PLAN).await?;
        let spec = plan_spec(&p)?;
        let rows=tx.query("SELECT key,kind,result FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind=ANY($3) AND result->>'plan_key'=$4 ORDER BY key LIMIT 257",&[&auth.scope().tenant(),&auth.scope().repository(),&vec![CANCEL,ACTIVATE,PROBE],&key]).await?;
        if rows.len() > 256 {
            return Err(Error::RestoreContextLimit);
        }
        let records:Vec<_>=rows.into_iter().map(|r|json!({"key":r.get::<_,String>(0),"kind":r.get::<_,String>(1),"receipt":r.get::<_,Value>(2)})).collect();
        let current=tx.query_opt("SELECT state FROM th_github_bindings WHERE tenant=$1 AND repository=$2 AND binding=$3",&[&auth.scope().tenant(),&auth.scope().repository(),&spec.new_binding]).await?.map(|r|r.get::<_,String>(0));
        let v = json!({"version":1,"plan":p,"records":records,"current_binding_state":current,"plan_unexpired":clock(&tx).await?<p["expires_at_unix"].as_i64().ok_or(Error::SchemaMismatch)?,"reconciliation_required":reconciliation_required(&tx,auth.scope()).await?,"external_fencing_verified":false});
        tx.commit().await?;
        Ok(v)
    }
    pub async fn activate_github_restore_reconnect(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        spec: &RestoreActivateSpec,
    ) -> Result<Value> {
        crate::identifier(&spec.plan_key, 128)?;
        crate::identifier(&spec.probe_key, 128)?;
        restore::digest_valid(&spec.assessment_sha256)?;
        spec.review.validate()?;
        if spec.version != 1 || !spec.external_assertions_still_valid {
            return Err(Error::Invalid("restore activation assertions"));
        }
        let req = request(auth, cmd, ACTIVATE, json!([1, spec]))?;
        let m = match self.begin(req).await? {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        let (p, plan) = live_plan(
            &m.tx,
            auth,
            cmd.epoch,
            &spec.plan_key,
            Permission::RepositoryWrite,
        )
        .await?;
        if spec.review.manifest_sha256 != plan.manifest_sha256 {
            return Err(Error::Invalid("restore activation manifest"));
        }
        let proof = receipt(&m.tx, auth.scope(), &spec.probe_key, PROBE).await?;
        let probe: RestoreReconnectProbe = serde_json::from_value(proof["specification"].clone())
            .map_err(|_| Error::SchemaMismatch)?;
        if proof["epoch"] != cmd.epoch {
            return Err(Error::Fenced);
        }
        fresh_probe(&p, &plan, &probe, clock(&m.tx).await?)?;
        let review = restore_review::evaluate(&m.tx, auth, cmd.epoch, &spec.review).await?;
        if review["assessment_sha256"] != spec.assessment_sha256
            || review["assessment"]["local_review_complete"] != true
        {
            return Err(Error::HostRecoveryRequired);
        }
        let mut oldest_capture = u64::MAX;
        for key in &spec.review.inventory_keys {
            let inv = restore_inventory::inventory(&m.tx, auth.scope(), key, cmd.epoch).await?;
            oldest_capture = oldest_capture.min(inv.finished_at_unix);
            if inv.started_at_unix
                < p["observe_after_unix"]
                    .as_u64()
                    .ok_or(Error::SchemaMismatch)?
                || !inv.token_revoked
            {
                return Err(Error::HostRecoveryRequired);
            }
            if inv.binding == plan.source_binding
                && (inv.repository_node != probe.observation.repository_node_id
                    || inv.default_branch != probe.observation.default_branch)
            {
                return Err(Error::RestoreContextChanged);
            }
        }
        // A review near the deadline must not inherit freshness from its start.
        let final_now = clock(&m.tx).await?;
        if final_now >= p["expires_at_unix"].as_i64().ok_or(Error::SchemaMismatch)?
            || oldest_capture < final_now.saturating_sub(120) as u64
        {
            return Err(Error::Fenced);
        }
        fresh_probe(&p, &plan, &probe, final_now)?;
        // All facts above are evaluated under begin()'s exclusive repository lock.
        // Existing policy/check/event/operation rows retain their old binding IDs.
        let s = auth.scope();
        m.tx.execute("INSERT INTO th_github_bindings(tenant,repository,binding,epoch,bridge,specification,state,principal) VALUES($1,$2,$3,$4,$5,$6,'active',$7)",&[&s.tenant(),&s.repository(),&plan.new_binding,&cmd.epoch,&plan.binding.bridge,&json!(plan.binding),&auth.principal()]).await?;
        let result = json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"epoch":cmd.epoch,"key":cmd.key,"plan_key":spec.plan_key,"binding":plan.new_binding,
            "quarantine_sha256":p["quarantine_sha256"],"plan_sha256":restore_digest(&p)?,"probe_key":spec.probe_key,"assessment_sha256":spec.assessment_sha256,
            "outcome":"configured_after_operator_assisted_restore","operator_assertions_recorded":true,"external_fencing_verified":false,"old_operations_replayed":false,"validation_authorized":false,"merge_authorized":false});
        m.complete(
            result,
            vec![Event {
                kind: "github.binding.configured".into(),
                payload: json!({"binding":plan.new_binding,"epoch":cmd.epoch}),
            }],
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn restore_reconnect_wire_requires_explicit_assertions_and_exact_probe() {
        let v: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/restore-reconnect-v1.json"
        ))
        .unwrap();
        let wire: WireCommand = serde_json::from_value(v["request"].clone()).unwrap();
        assert_eq!(wire.permission(), Permission::RepositoryWrite);
        let WireCommand::RestoreReconnectPlan {
            specification: s, ..
        } = wire
        else {
            panic!()
        };
        s.validate().unwrap();
        assert_eq!(restore_digest(&s).unwrap(), v["specification_sha256"]);
        for field in [
            "old_writers_and_runtimes_fenced",
            "old_credentials_cannot_mint",
            "old_tokens_revoked_or_expired",
            "prior_host_mutations_settled",
            "host_writes_controlled",
            "host_policy_reviewed",
        ] {
            let mut bad = json!(s);
            bad[field] = json!(false);
            assert!(
                serde_json::from_value::<RestoreReconnectSpec>(bad)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
            let mut absent = json!(s);
            absent.as_object_mut().unwrap().remove(field);
            assert!(serde_json::from_value::<RestoreReconnectSpec>(absent).is_err());
        }
        for ttl in [0, 29, 3601, u32::MAX] {
            let mut bad = (*s).clone();
            bad.ttl_seconds = ttl;
            assert!(bad.validate().is_err());
        }
        let mut bad = v["request"].clone();
        bad["specification"]["external_fencing_verified"] = json!(true);
        assert!(serde_json::from_value::<WireCommand>(bad).is_err());
        let plan = json!({"key":"plan","observe_after_unix":100});
        let mut p = RestoreReconnectProbe {
            version: 1,
            plan_key: "plan".into(),
            plan_sha256: restore_digest(&plan).unwrap(),
            started_at_unix: 100,
            finished_at_unix: 105,
            observation: Observation {
                version: 1,
                api_version: "2026-03-10".into(),
                observed_at_unix: 102,
                app_id: s.binding.app_id,
                binding: s.binding.upstream.clone(),
                repository_node_id: "R_test".into(),
                owner_type: "Organization".into(),
                private: false,
                default_branch: "main".into(),
                archived: false,
                disabled: false,
                read_connection_verified: true,
                probe_token_revoked: true,
                queue_eligibility: "public_organization_eligible".into(),
                queue_configuration_verified: false,
                publication_ready: false,
            },
        };
        fresh_probe(&plan, &s, &p, 105).unwrap();
        assert!(fresh_probe(&plan, &s, &p, 226).is_err());
        assert!(fresh_probe(&plan, &s, &p, 99).is_err());
        p.started_at_unix = 99;
        assert!(fresh_probe(&plan, &s, &p, 105).is_err());
    }
}
