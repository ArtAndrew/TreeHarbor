//! Owner policy fences queue admission and managed check execution/publication.
use super::*;
use crate::integration::CheckProgram;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use treeharbor_core::ObjectId;
use treeharbor_github_contract::{
    RequiredCheck, RuleObservation, check_publication as cp, queue::QueueTarget,
    workflow_definitions as wd,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedRequirement {
    pub program: CheckProgram,
    pub validators: BTreeSet<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdmissionPolicy {
    pub version: u8,
    pub rules: RuleObservation,
    pub managed: Vec<ManagedRequirement>,
    pub workflows: Vec<wd::CheckApproval>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_configuration:
        Option<Box<treeharbor_github_contract::host_configuration::Configuration>>,
}
impl AdmissionPolicy {
    pub fn canonical(&self) -> Result<Vec<u8>> {
        if !matches!(
            (self.version, self.host_configuration.is_some()),
            (1, false) | (2, true)
        ) || self.managed.len() + self.workflows.len() == 0
            || self.managed.len() + self.workflows.len() > 32
            || !self.rules.merge_queue
            || self.rules.count == 0
            || self.rules.count > 100
            || self.rules.sha256.len() != 64
            || !self
                .rules
                .sha256
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(Error::Invalid("admission policy bounds"));
        }
        let mut p = self.clone();
        if let Some(c) = &self.host_configuration {
            let target = &c.visibility.target;
            let analysis = c
                .analyze(target)
                .map_err(|_| Error::Invalid("host configuration"))?;
            if !analysis.configuration_reasons.is_empty()
                || treeharbor_github_contract::host_policy::rules_fingerprint(&c.profile.rules)
                    .map_err(|_| Error::Invalid("host configuration rules"))?
                    != self.rules
            {
                return Err(Error::Invalid("unproven host configuration"));
            }
            p.host_configuration = Some(Box::new(
                serde_json::from_slice(
                    &c.canonical(target)
                        .map_err(|_| Error::Invalid("host configuration"))?,
                )
                .map_err(|_| Error::Invalid("host configuration"))?,
            ));
        }
        let mut programs = Vec::new();
        for m in &self.managed {
            let id = m.program.digest()?;
            if m.validators.is_empty() || m.validators.len() > 16 {
                return Err(Error::Invalid("admission validators"));
            }
            for v in &m.validators {
                crate::identifier(v, 128)?;
            }
            programs.push((id, m.clone()));
        }
        programs.sort_by_key(|(id, _)| *id);
        if programs.windows(2).any(|w| w[0].0 == w[1].0) {
            return Err(Error::Invalid("duplicate admission program"));
        }
        p.managed = programs.into_iter().map(|(_, m)| m).collect();
        if !p.workflows.is_empty() {
            let policy = wd::Policy {
                version: 1,
                checks: p.workflows,
            };
            let bytes = policy
                .canonical()
                .map_err(|_| Error::Invalid("admission workflows"))?;
            p.workflows = serde_json::from_slice::<wd::Policy>(&bytes)
                .map_err(|_| Error::SchemaMismatch)?
                .checks;
        }
        let bytes = if p.version == 2 {
            serde_json::to_vec(
                &serde_json::to_value(&p).map_err(|_| Error::Invalid("admission policy"))?,
            )
        } else {
            serde_json::to_vec(&p)
        }
        .map_err(|_| Error::Invalid("admission policy"))?;
        if bytes.len() > 65536 {
            return Err(Error::Invalid("admission policy size"));
        }
        Ok(bytes)
    }
    pub fn digest(&self) -> Result<ObjectId> {
        let mut h = Sha256::new();
        h.update(match self.version {
            1 => b"treeharbor-connected-admission-policy\0v1\0",
            2 => b"treeharbor-connected-admission-policy\0v2\0",
            _ => return Err(Error::Invalid("admission policy version")),
        });
        h.update(self.canonical()?);
        Ok(ObjectId::from_bytes(h.finalize().into()))
    }
    fn decode(bytes: &[u8], id: ObjectId) -> Result<Self> {
        let p: Self = serde_json::from_slice(bytes).map_err(|_| Error::SchemaMismatch)?;
        if p.canonical()? != bytes || p.digest()? != id {
            return Err(Error::SchemaMismatch);
        }
        Ok(p)
    }
    fn planned_required(
        &self,
        app: u64,
        epoch: i64,
        binding: Uuid,
        base: &str,
        generation: i64,
    ) -> Result<BTreeSet<RequiredCheck>> {
        self.canonical()?;
        if app == 0 || app > i64::MAX as u64 || epoch <= 0 || binding.is_nil() || generation <= 0 {
            return Err(Error::Invalid("admission identity"));
        }
        check_policy::base(base)?;
        let mut checks: BTreeSet<_> = self.workflows.iter().map(|w| w.required.clone()).collect();
        for m in &self.managed {
            let a = cp::AdmissionContext {
                binding,
                generation,
                policy: self.digest()?.to_string(),
            };
            checks.insert(RequiredCheck {
                app_id: app,
                name: cp::context_name(epoch, &m.program.digest()?.to_string(), base, Some(&a))
                    .map_err(|_| Error::Invalid("admission context"))?,
            });
        }
        let names: BTreeSet<_> = checks.iter().map(|c| &c.name).collect();
        if checks.len() != self.managed.len() + self.workflows.len() || names.len() != checks.len()
        {
            return Err(Error::Invalid("duplicate admission context"));
        }
        Ok(checks)
    }
    pub fn required(
        &self,
        app: u64,
        epoch: i64,
        binding: Uuid,
        base: &str,
        generation: i64,
    ) -> Result<BTreeSet<RequiredCheck>> {
        let checks = self.planned_required(app, epoch, binding, base, generation)?;
        if let Some(c) = &self.host_configuration
            && (c.visibility.target.base_ref != base
                || c.analyze(&c.visibility.target)
                    .map_err(|_| Error::Invalid("host configuration"))?
                    .required
                    != checks)
        {
            return Err(Error::Invalid("host configuration required checks"));
        }
        Ok(checks)
    }
    fn bound(&self, upstream: &Upstream, base: &str) -> Result<()> {
        if let Some(c) = &self.host_configuration {
            let t = &c.visibility.target;
            if t.repository_id != upstream.repository_id
                || t.repository_name != format!("{}/{}", upstream.owner, upstream.name)
                || t.base_ref != base
            {
                return Err(Error::Invalid("host configuration target"));
            }
        }
        Ok(())
    }
    /// Prepare owner-reviewable input from an actual stable inspection. This
    /// changes no stored policy, generation, host rule or approval.
    pub fn with_inspection(
        &self,
        inspection: &treeharbor_github_contract::host_configuration::Inspection,
    ) -> Result<Self> {
        self.canonical()?;
        Self::from_inspection(inspection, self.managed.clone(), self.workflows.clone())
    }
    /// Construct a draft directly from requirements, without caller-made rule hashes.
    pub fn from_inspection(
        inspection: &treeharbor_github_contract::host_configuration::Inspection,
        managed: Vec<ManagedRequirement>,
        workflows: Vec<wd::CheckApproval>,
    ) -> Result<Self> {
        let t = &inspection.before.visibility.target;
        if !inspection
            .reasons(t)
            .map_err(|_| Error::Invalid("host inspection"))?
            .is_empty()
        {
            return Err(Error::Invalid("unproven host inspection"));
        }
        let p = Self {
            version: 2,
            managed,
            workflows,
            rules: treeharbor_github_contract::host_policy::rules_fingerprint(
                &inspection.after.profile.rules,
            )
            .map_err(|_| Error::Invalid("host configuration rules"))?,
            host_configuration: Some(inspection.after.clone()),
        };
        p.canonical()?;
        Ok(p)
    }
    fn permits(&self, program: ObjectId, validator: &str) -> Result<()> {
        if !self
            .managed
            .iter()
            .any(|m| m.program.digest().ok() == Some(program) && m.validators.contains(validator))
        {
            return Err(Error::ExecutionState);
        }
        Ok(())
    }
}
fn oid(bytes: &[u8]) -> Result<ObjectId> {
    Ok(ObjectId::from_bytes(
        bytes.try_into().map_err(|_| Error::SchemaMismatch)?,
    ))
}
async fn head(
    tx: &Transaction<'_>,
    s: &Scope,
    b: Uuid,
    epoch: i64,
    base: &str,
) -> Result<Option<Row>> {
    Ok(tx.query_opt("SELECT generation,policy FROM th_github_admission_refs WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND base_ref=$5",&[&s.tenant(),&s.repository(),&b,&epoch,&base]).await?)
}
async fn definition(
    tx: &Transaction<'_>,
    s: &Scope,
    b: Uuid,
    epoch: i64,
    id: ObjectId,
) -> Result<AdmissionPolicy> {
    let row=tx.query_opt("SELECT definition FROM th_github_admission_policies WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND digest=$5",&[&s.tenant(),&s.repository(),&b,&epoch,&id.as_bytes().as_slice()]).await?.ok_or(Error::NotFound)?;
    AdmissionPolicy::decode(&row.get::<_, Vec<u8>>(0), id)
}
async fn current(
    tx: &Transaction<'_>,
    s: &Scope,
    b: Uuid,
    epoch: i64,
    base: &str,
    pin: Option<&PolicyPin>,
) -> Result<Option<AdmissionPolicy>> {
    let h = head(tx, s, b, epoch, base).await?;
    match (h, pin) {
        (None, None) => Ok(None),
        (Some(h), Some(p)) => {
            let id = p.validate()?;
            if h.get::<_, i64>("generation") != p.generation
                || h.get::<_, Option<Vec<u8>>>("policy").as_deref() != Some(id.as_bytes())
            {
                return Err(Error::HostQueueState);
            }
            Ok(Some(definition(tx, s, b, epoch, id).await?))
        }
        _ => Err(Error::HostQueueState),
    }
}
fn row_pin(row: &Row) -> Result<PolicyPin> {
    Ok(PolicyPin {
        generation: row.get("generation"),
        digest: oid(&row.get::<_, Vec<u8>>("policy"))?.to_string(),
    })
}
/// Historical pin with independent current eligibility; never grants dispatch.
pub(super) async fn pinned_queue(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
) -> Result<Option<(PolicyPin, AdmissionPolicy, bool)>> {
    let t = queue::target(q)?;
    let b = q.get("binding");
    let epoch = q.get("epoch");
    let Some(row)=tx.query_opt("SELECT * FROM th_github_queue_admission WHERE tenant=$1 AND repository=$2 AND queue=$3",&[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue")]).await? else {return Ok(None);};
    if row.get::<_, Uuid>("binding") != b
        || row.get::<_, i64>("epoch") != epoch
        || row.get::<_, String>("base_ref") != t.pull.base_ref
    {
        return Err(Error::SchemaMismatch);
    }
    let pin = row_pin(&row)?;
    let id = pin.validate()?;
    let eligible = head(tx, s, b, epoch, &t.pull.base_ref)
        .await?
        .is_some_and(|h| {
            h.get::<_, i64>("generation") == pin.generation
                && h.get::<_, Option<Vec<u8>>>("policy").as_deref() == Some(id.as_bytes())
        });
    Ok(Some((
        pin,
        definition(tx, s, b, epoch, id).await?,
        eligible,
    )))
}
pub(super) async fn queue_policy(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
) -> Result<Option<(PolicyPin, AdmissionPolicy)>> {
    let pinned = pinned_queue(tx, s, q).await?;
    if let Some((pin, p, current)) = pinned {
        if !current {
            return Err(Error::HostQueueState);
        }
        return Ok(Some((pin, p)));
    }
    let t = queue::target(q)?;
    current(
        tx,
        s,
        q.get("binding"),
        q.get("epoch"),
        &t.pull.base_ref,
        None,
    )
    .await?;
    Ok(None)
}
pub(super) async fn pin_queue(
    tx: &Transaction<'_>,
    s: &Scope,
    q: &Row,
    t: &QueueTarget,
    pin: Option<&PolicyPin>,
) -> Result<()> {
    let b = q.get("binding");
    let epoch = q.get("epoch");
    let policy = current(tx, s, b, epoch, &t.pull.base_ref, pin).await?;
    if let (Some(p), Some(pin)) = (policy, pin) {
        let spec = specification(&binding(tx, s, b).await?)?;
        if p.required(spec.app_id, epoch, b, &t.pull.base_ref, pin.generation)?
            != t.pull.required.iter().cloned().collect()
            || p.rules != t.expected_rules
        {
            return Err(Error::HostQueueState);
        }
        let id = pin.validate()?;
        tx.execute("INSERT INTO th_github_queue_admission(tenant,repository,queue,binding,epoch,base_ref,generation,policy) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",&[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue"),&b,&epoch,&t.pull.base_ref,&pin.generation,&id.as_bytes().as_slice()]).await?;
    }
    Ok(())
}
async fn check_source(tx: &Transaction<'_>, s: &Scope, c: &Row) -> Result<(Uuid, String)> {
    if let Some(q) = c.get::<_, Option<Uuid>>("queue") {
        let q = queue::load(tx, s, q).await?;
        Ok((q.get("binding"), queue::target(&q)?.pull.base_ref))
    } else {
        let p = pulls::load(tx, s, c.get("pull")).await?;
        let t: PullTarget =
            serde_json::from_value(p.get("target")).map_err(|_| Error::SchemaMismatch)?;
        Ok((p.get("binding"), t.base))
    }
}
pub(super) async fn pin_check(tx: &Transaction<'_>, s: &Scope, c: &Row) -> Result<()> {
    let (b, base) = check_source(tx, s, c).await?;
    let epoch = c.get("epoch");
    let pin = if let Some(id) = c.get::<_, Option<Uuid>>("queue") {
        queue_policy(tx, s, &queue::load(tx, s, id).await?)
            .await?
            .map(|(pin, _)| pin)
    } else {
        head(tx, s, b, epoch, &base)
            .await?
            .map(|h| {
                Ok::<PolicyPin, Error>(PolicyPin {
                    generation: h.get("generation"),
                    digest: oid(&h
                        .get::<_, Option<Vec<u8>>>("policy")
                        .ok_or(Error::HostQueueState)?)?
                    .to_string(),
                })
            })
            .transpose()?
    };
    if let Some(p) = current(tx, s, b, epoch, &base, pin.as_ref()).await? {
        p.permits(
            oid(&c.get::<_, Vec<u8>>("program"))?,
            &c.get::<_, String>("validator"),
        )?;
        let pin = pin.ok_or(Error::SchemaMismatch)?;
        let id = pin.validate()?;
        tx.execute("INSERT INTO th_github_check_admission(tenant,repository,check_run,binding,epoch,base_ref,generation,policy) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",&[&s.tenant(),&s.repository(),&c.get::<_,Uuid>("check_run"),&b,&epoch,&base,&pin.generation,&id.as_bytes().as_slice()]).await?;
    }
    Ok(())
}
pub(super) async fn check_policy(
    tx: &Transaction<'_>,
    s: &Scope,
    c: &Row,
) -> Result<Option<(PolicyPin, AdmissionPolicy)>> {
    let (b, base) = check_source(tx, s, c).await?;
    let epoch = c.get("epoch");
    let row=tx.query_opt("SELECT * FROM th_github_check_admission WHERE tenant=$1 AND repository=$2 AND check_run=$3",&[&s.tenant(),&s.repository(),&c.get::<_,Uuid>("check_run")]).await?;
    if row.as_ref().is_some_and(|r| {
        r.get::<_, Uuid>("binding") != b
            || r.get::<_, i64>("epoch") != epoch
            || r.get::<_, String>("base_ref") != base
    }) {
        return Err(Error::SchemaMismatch);
    }
    let pin = row.as_ref().map(row_pin).transpose()?;
    let policy = current(tx, s, b, epoch, &base, pin.as_ref()).await?;
    if let Some(p) = &policy {
        p.permits(
            oid(&c.get::<_, Vec<u8>>("program"))?,
            &c.get::<_, String>("validator"),
        )?;
    }
    if let Some(id) = c.get::<_, Option<Uuid>>("queue") {
        let q = queue_policy(tx, s, &queue::load(tx, s, id).await?)
            .await?
            .map(|(pin, _)| pin);
        if pin != q {
            return Err(Error::HostQueueState);
        }
    }
    Ok(pin.zip(policy))
}
/// Immutable historical execution identity, independent of current policy eligibility.
pub(super) async fn recorded_context(
    tx: &Transaction<'_>,
    s: &Scope,
    c: &Row,
) -> Result<Option<cp::AdmissionContext>> {
    let Some(row)=tx.query_opt("SELECT * FROM th_github_check_admission WHERE tenant=$1 AND repository=$2 AND check_run=$3",&[&s.tenant(),&s.repository(),&c.get::<_,Uuid>("check_run")]).await? else {return Ok(None);};
    let (binding, base) = check_source(tx, s, c).await?;
    if row.get::<_, Uuid>("binding") != binding
        || row.get::<_, i64>("epoch") != c.get::<_, i64>("epoch")
        || row.get::<_, String>("base_ref") != base
    {
        return Err(Error::SchemaMismatch);
    }
    let pin = row_pin(&row)?;
    Ok(Some(cp::AdmissionContext {
        binding,
        generation: pin.generation,
        policy: pin.digest,
    }))
}
pub(super) async fn publication_preflight(
    tx: &Transaction<'_>,
    s: &Scope,
    check_run: Uuid,
    p: &cp::Preflight,
) -> Result<()> {
    let c=tx.query_one("SELECT * FROM th_github_managed_checks WHERE tenant=$1 AND repository=$2 AND check_run=$3",&[&s.tenant(),&s.repository(),&check_run]).await?;
    if let Some((pin, policy)) = check_policy(tx, s, &c).await? {
        let (b, base) = check_source(tx, s, &c).await?;
        let spec = specification(&binding(tx, s, b).await?)?;
        let required = policy.required(spec.app_id, c.get("epoch"), b, &base, pin.generation)?;
        for observed in [&p.policy_before, &p.policy_after] {
            if observed.rules != policy.rules
                || observed
                    .protection
                    .app_bound_checks
                    .iter()
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    != required
            {
                return Err(Error::HostQueueState);
            }
        }
    }
    Ok(())
}
impl Metadata {
    pub async fn set_github_admission_policy(
        &mut self,
        auth: &Authorization,
        c: &Command,
        b: Uuid,
        base: &str,
        generation: i64,
        value: Option<&AdmissionPolicy>,
    ) -> Result<Value> {
        uuid(b)?;
        check_policy::base(base)?;
        if !(0..i64::MAX).contains(&generation) {
            return Err(Error::Invalid("admission generation"));
        }
        let encoded = value.map(AdmissionPolicy::canonical).transpose()?;
        let id = value.map(AdmissionPolicy::digest).transpose()?;
        let m = match self
            .begin(request(
                auth,
                c,
                "github.admission_policy.set.v1",
                json!([1, b, base, generation, id.map(|i| i.to_string())]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(m) => m,
        };
        context(&m.tx, auth, c.epoch, Permission::PolicyWrite).await?;
        let s = auth.scope();
        let spec = projections::active_binding(&m.tx, s, b, c.epoch).await?;
        let old = head(&m.tx, s, b, c.epoch, base).await?;
        if old.as_ref().map_or(0, |r| r.get::<_, i64>("generation")) != generation
            || (old.is_none() && value.is_none())
        {
            return Err(Error::HostQueueState);
        }
        if let Some(p) = value {
            p.bound(&spec.upstream, base)?;
            p.required(spec.app_id, c.epoch, b, base, generation + 1)?;
        }
        let bytes = id.map(|i| i.as_bytes().to_vec());
        if let Some(encoded) = encoded {
            m.tx.execute("INSERT INTO th_github_admission_policies(tenant,repository,binding,epoch,digest,definition,principal) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING",&[&s.tenant(),&s.repository(),&b,&c.epoch,&bytes,&encoded,&auth.principal()]).await?;
            if definition(&m.tx, s, b, c.epoch, id.ok_or(Error::SchemaMismatch)?)
                .await?
                .canonical()?
                != encoded
            {
                return Err(Error::SchemaMismatch);
            }
        }
        let next = generation + 1;
        m.tx.execute("INSERT INTO th_github_admission_refs(tenant,repository,binding,epoch,base_ref,generation,policy) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT(tenant,repository,binding,epoch,base_ref) DO UPDATE SET generation=EXCLUDED.generation,policy=EXCLUDED.policy",&[&s.tenant(),&s.repository(),&b,&c.epoch,&base,&next,&bytes]).await?;
        let required = value
            .map(|p| p.required(spec.app_id, c.epoch, b, base, next))
            .transpose()?;
        let v = json!({"version":1,"binding":b,"epoch":c.epoch,"base_ref":base,"generation":next,"policy":id.map(|i|i.to_string()),"required":required});
        m.write_completion(
            &v,
            &[Event {
                kind: "github.admission_policy.changed".into(),
                payload: v.clone(),
            }],
        )
        .await?;
        context(&m.tx, auth, c.epoch, Permission::PolicyWrite).await?;
        projections::active_binding(&m.tx, s, b, c.epoch).await?;
        m.tx.commit().await?;
        Ok(v)
    }
    /// Pure owner preview: does not approve policy or modify upstream rules.
    pub async fn plan_github_admission_policy(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        b: Uuid,
        base: &str,
        generation: i64,
        p: &AdmissionPolicy,
    ) -> Result<Value> {
        uuid(b)?;
        check_policy::base(base)?;
        if !(0..i64::MAX).contains(&generation) {
            return Err(Error::Invalid("admission generation"));
        }
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::PolicyWrite).await?;
        let spec = projections::active_binding(&tx, s, b, epoch).await?;
        if head(&tx, s, b, epoch, base)
            .await?
            .map_or(0, |r| r.get::<_, i64>("generation"))
            != generation
        {
            return Err(Error::HostQueueState);
        }
        p.bound(&spec.upstream, base)?;
        let required = p.planned_required(spec.app_id, epoch, b, base, generation + 1)?;
        let mut v = json!({"version":1,"binding":b,"epoch":epoch,"base_ref":base,"expected_generation":generation,"generation":generation+1,"policy":p.digest()?.to_string(),"required":required,"rules":p.rules,"approved":false});
        if let Some(c) = &p.host_configuration {
            v["host_configuration"] = json!({
                "digest": c.digest(&c.visibility.target).map_err(|_| Error::Invalid("host configuration"))?,
                "required_matches": c.analyze(&c.visibility.target).map_err(|_| Error::Invalid("host configuration"))?.required == required,
            });
        }
        context(&tx, auth, epoch, Permission::PolicyWrite).await?;
        tx.commit().await?;
        Ok(v)
    }
    pub async fn github_admission_policy(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        b: Uuid,
        base: &str,
    ) -> Result<Value> {
        uuid(b)?;
        check_policy::base(base)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let row = binding(&tx, s, b).await?;
        if row.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let h = head(&tx, s, b, epoch, base).await?;
        let generation = h.as_ref().map_or(0, |r| r.get::<_, i64>("generation"));
        let id = h
            .and_then(|r| r.get::<_, Option<Vec<u8>>>("policy"))
            .as_deref()
            .map(oid)
            .transpose()?;
        let p = if let Some(id) = id {
            Some(definition(&tx, s, b, epoch, id).await?)
        } else {
            None
        };
        let required = p
            .as_ref()
            .map(|p| p.required(specification(&row)?.app_id, epoch, b, base, generation))
            .transpose()?;
        let state = if generation == 0 {
            "unconfigured"
        } else if p.is_some() {
            "active"
        } else {
            "revoked"
        };
        let v = json!({"version":1,"binding":b,"epoch":epoch,"base_ref":base,"generation":generation,"policy":id.map(|i|i.to_string()),"definition":p,"required":required,"state":state});
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn admission_policy_matches_independent_vector_and_rejects_ambiguous_authority() {
        let v: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/connected-admission-policy-v1.json"
        ))
        .unwrap();
        let p: AdmissionPolicy = serde_json::from_value(v["policy"].clone()).unwrap();
        assert_eq!(
            p.canonical().unwrap(),
            v["canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(p.digest().unwrap().to_string(), v["digest"]);
        AdmissionPolicy::decode(&p.canonical().unwrap(), p.digest().unwrap()).unwrap();
        assert!(
            AdmissionPolicy::decode(&p.canonical().unwrap(), ObjectId::from_bytes([0; 32]))
                .is_err()
        );
        let mut reorder = p.clone();
        reorder.managed.reverse();
        reorder.workflows.reverse();
        assert_eq!(reorder.digest().unwrap(), p.digest().unwrap());
        let mut duplicate = p.clone();
        duplicate.managed.push(duplicate.managed[0].clone());
        assert!(duplicate.canonical().is_err());
        let mut bad = p.clone();
        bad.managed[0].validators.clear();
        assert!(bad.canonical().is_err());
        let mut bad = p.clone();
        bad.rules.merge_queue = false;
        assert!(bad.canonical().is_err());
        let mut bad = p.clone();
        bad.workflows[0].expected_context.toolchain = "unbound".into();
        assert!(bad.canonical().is_err());
        let mut bad = p.clone();
        bad.workflows[0].required = p
            .required(17, 1, Uuid::from_bytes([1; 16]), "main", 1)
            .unwrap()
            .into_iter()
            .find(|r| r.name.starts_with("TreeHarbor/"))
            .unwrap();
        assert!(
            bad.required(17, 1, Uuid::from_bytes([1; 16]), "main", 1)
                .is_err()
        );
        assert!(
            p.required(0, 1, Uuid::from_bytes([1; 16]), "main", 1)
                .is_err()
        );
        assert!(
            p.required(17, 0, Uuid::from_bytes([1; 16]), "main", 1)
                .is_err()
        );
        let mut unknown = v["policy"].clone();
        unknown["admission_verified"] = json!(true);
        assert!(serde_json::from_value::<AdmissionPolicy>(unknown).is_err());
    }
}

#[cfg(test)]
mod tests_v2;
