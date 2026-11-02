//! One local review transaction across every retained binding. Never activation.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::RevisionRef;

const MAX_BINDINGS: usize = 256;
const MAX_WORK: usize = 50_000;
const SAMPLE: usize = 8;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RestoreReviewSpec {
    pub version: u8,
    pub manifest_sha256: String,
    /// At most one explicitly selected capture per binding. Omissions are blockers.
    pub inventory_keys: Vec<String>,
}
impl RestoreReviewSpec {
    pub(super) fn validate(&self) -> Result<()> {
        if self.version != 1 || self.inventory_keys.len() > MAX_BINDINGS {
            return Err(Error::Invalid("restore review specification"));
        }
        restore::digest_valid(&self.manifest_sha256)?;
        let mut keys = BTreeSet::new();
        for k in &self.inventory_keys {
            crate::identifier(k, 128)?;
            if !keys.insert(k) {
                return Err(Error::Invalid("duplicate restore inventory selection"));
            }
        }
        Ok(())
    }
}
fn budget(remaining: &mut usize, n: usize) -> Result<()> {
    *remaining = remaining.checked_sub(n).ok_or(Error::RestoreContextLimit)?;
    Ok(())
}
fn summary(values: &[Value]) -> Result<Value> {
    Ok(
        json!({"count":values.len(),"sample":values.iter().take(SAMPLE).collect::<Vec<_>>(),"sample_truncated":values.len()>SAMPLE,"all_sha256":restore_digest(&values)?}),
    )
}
fn subjects(ctx: &RestoreContext) -> Vec<RestoreDecisionSubject> {
    let mut result: Vec<_> = ctx
        .effects
        .iter()
        .map(|e| RestoreDecisionSubject::Retained {
            effect_kind: e.kind.clone(),
            operation: e.id,
        })
        .collect();
    result.extend(
        ctx.dequeues
            .iter()
            .map(|e| RestoreDecisionSubject::RetainedDequeue { operation: e.id }),
    );
    result.extend(
        ctx.recoveries
            .iter()
            .map(|e| RestoreDecisionSubject::RetainedRecovery {
                recovery: e.recovery,
            }),
    );
    result.extend(
        ctx.redeliveries
            .iter()
            .map(|e| RestoreDecisionSubject::RetainedRedelivery {
                recovery: e.recovery,
                provider_id: e.provider_id,
            }),
    );
    result
}
async fn decisions(
    tx: &Transaction<'_>,
    s: &Scope,
    ctx: &RestoreContext,
    remaining: &mut usize,
) -> Result<Value> {
    let required = subjects(ctx);
    budget(remaining, required.len())?;
    let identities: Vec<_> = required.iter().map(restore_digest).collect::<Result<_>>()?;
    let rows=tx.query("SELECT DISTINCT ON (result->>'subject_sha256') result->>'subject_sha256',key,result->'specification',(result->>'sequence')::bigint FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind='github.restore.decision.v1' AND result->>'manifest_sha256'=$3 AND result->>'binding'=$4 AND result->>'subject_sha256'=ANY($5) ORDER BY result->>'subject_sha256',(result->>'sequence')::bigint DESC",&[&s.tenant(),&s.repository(),&ctx.manifest_sha256,&ctx.binding.to_string(),&identities]).await?;
    let latest: BTreeMap<String, (String, Value, i64)> = rows
        .into_iter()
        .map(|r| (r.get(0), (r.get(1), r.get(2), r.get(3))))
        .collect();
    let hash = restore_digest(ctx)?;
    let mut evaluated = Vec::new();
    let mut blockers = Vec::new();
    for (subject, id) in required.iter().zip(identities) {
        let (status, selected) = match latest.get(&id) {
            None => ("missing_decision", Value::Null),
            Some((key, value, sequence)) => {
                let d: RestoreDecisionSpec =
                    serde_json::from_value(value.clone()).map_err(|_| Error::SchemaMismatch)?;
                let status = if d.context_sha256 != hash {
                    "stale_decision"
                } else if matches!(d.disposition, RestoreDisposition::DoNotReplay) {
                    "do_not_replay_recorded"
                } else {
                    "decision_not_final"
                };
                (
                    status,
                    json!({"key":key,"sequence":sequence,"specification_sha256":restore_digest(&d)?}),
                )
            }
        };
        let item = json!({"subject":subject,"classification":status,"decision":selected});
        if status != "do_not_replay_recorded" {
            blockers.push(item.clone());
        }
        evaluated.push(item);
    }
    Ok(
        json!({"required":required.len(),"reviewed":required.len()-blockers.len(),"blockers":summary(&blockers)?,"all_sha256":restore_digest(&evaluated)?,"prior_host_outcomes_verified":false}),
    )
}
async fn retained_sources(
    tx: &Transaction<'_>,
    s: &Scope,
    ctx: &RestoreContext,
) -> Result<(BTreeSet<String>, BTreeSet<String>)> {
    let mut git = BTreeSet::new();
    let mut native = BTreeSet::new();
    for e in &ctx.effects {
        git.extend(e.expected_git.iter().cloned());
        git.extend(e.prepared_git.iter().cloned());
        if let Some(t) = &e.target {
            git.insert(t.git_revision.clone());
        }
        if let Some(hex) = &e.native_object_hex {
            let v = format!("native:sha256:{hex}");
            if !matches!(v.parse::<RevisionRef>(), Ok(RevisionRef::Native(_))) {
                return Err(Error::SchemaMismatch);
            }
            native.insert(v);
        }
        match e.kind {
            RestoreEffectKind::Queue => {
                let row = queue::load(tx, s, e.id).await?;
                let target = queue::target(&row)?;
                target.validate().map_err(|_| Error::SchemaMismatch)?;
                git.insert(target.pull.head_revision);
                git.insert(target.expected_base_revision);
            }
            RestoreEffectKind::Check => {
                let row = check_publications::load(tx, s, e.id).await?;
                let target = check_publications::target(&row)?;
                git.insert(
                    target
                        .source
                        .head()
                        .map_err(|_| Error::SchemaMismatch)?
                        .into(),
                );
                git.insert(target.source.pull().head_revision.clone());
            }
            _ => {}
        }
    }
    Ok((git, native))
}
async fn native_pins(
    tx: &Transaction<'_>,
    s: &Scope,
    ctx: &RestoreContext,
    revisions: &BTreeSet<String>,
) -> Result<BTreeMap<String, Value>> {
    let wanted: Vec<_> = revisions.iter().cloned().collect();
    let rows=tx.query("SELECT DISTINCT ON (d.result#>>'{source_preservation,native_revision}') d.result#>>'{source_preservation,native_revision}',d.key,p.holder,p.generation FROM th_operations d JOIN th_object_pins p ON p.tenant=d.tenant AND p.repository=d.repository AND p.holder=d.result#>>'{source_preservation,holder}' AND p.generation=(d.result#>>'{source_preservation,generation}')::bigint JOIN th_objects o ON o.tenant=p.tenant AND o.digest=p.digest AND o.generation=p.generation WHERE d.tenant=$1 AND d.repository=$2 AND d.kind='github.restore.decision.v1' AND d.result->>'manifest_sha256'=$3 AND d.result->>'binding'=$4 AND (d.result->>'epoch')::bigint=$5 AND d.result#>>'{source_preservation,native_revision}'=ANY($6) AND 'native:sha256:'||encode(p.digest,'hex')=d.result#>>'{source_preservation,native_revision}' AND o.kind=5 AND o.state='live' ORDER BY d.result#>>'{source_preservation,native_revision}',d.key",&[&s.tenant(),&s.repository(),&ctx.manifest_sha256,&ctx.binding.to_string(),&ctx.epoch,&wanted]).await?;
    Ok(rows.into_iter().map(|r|(r.get::<_,String>(0),json!({"decision_key":r.get::<_,String>(1),"holder":r.get::<_,String>(2),"generation":r.get::<_,i64>(3)}))).collect())
}
async fn source_summary(
    tx: &Transaction<'_>,
    s: &Scope,
    ctx: &RestoreContext,
    git: BTreeSet<String>,
    native: BTreeSet<String>,
) -> Result<Value> {
    let wanted: Vec<_> = git.iter().cloned().collect();
    let mappings = restore_inventory::verified_mappings(
        tx,
        s,
        ctx.epoch,
        &ctx.manifest_sha256,
        ctx.binding,
        &wanted,
    )
    .await?;
    let pins = native_pins(tx, s, ctx, &native).await?;
    let missing_git: Vec<_> = git
        .iter()
        .filter(|v| !mappings.contains_key(*v))
        .map(|v| json!(v))
        .collect();
    let missing_native: Vec<_> = native
        .iter()
        .filter(|v| !pins.contains_key(*v))
        .map(|v| json!(v))
        .collect();
    Ok(
        json!({"required_git_revisions":git.len(),"required_native_revisions":native.len(),"missing_git":summary(&missing_git)?,"missing_native":summary(&missing_native)?,
        "all_sha256":restore_digest(&json!([1,git,native,mappings,pins]))?,"physical_storage_scrubbed":false}),
    )
}
impl Metadata {
    pub async fn github_restore_review(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        spec: &RestoreReviewSpec,
    ) -> Result<Value> {
        spec.validate()?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::RepositoryWrite).await?;
        let result = evaluate(&tx, auth, epoch, spec).await?;
        tx.commit().await?;
        Ok(result)
    }
}

// Reused inside the exclusive activation transaction; no cached-pass shortcut.
pub(super) async fn evaluate(
    tx: &Transaction<'_>,
    auth: &Authorization,
    epoch: i64,
    spec: &RestoreReviewSpec,
) -> Result<Value> {
    spec.validate()?;
    context(tx, auth, epoch, Permission::RepositoryWrite).await?;
    let s = auth.scope();
    let rows=tx.query("SELECT binding FROM th_github_bindings WHERE tenant=$1 AND repository=$2 ORDER BY binding LIMIT $3",&[&s.tenant(),&s.repository(),&((MAX_BINDINGS+1) as i64)]).await?;
    if rows.is_empty() {
        return Err(Error::NotFound);
    }
    if rows.len() > MAX_BINDINGS {
        return Err(Error::RestoreContextLimit);
    }
    let bindings: BTreeSet<Uuid> = rows.into_iter().map(|r| r.get(0)).collect();
    let mut inventories = BTreeMap::new();
    for key in &spec.inventory_keys {
        let inv = restore_inventory::inventory(tx, s, key, epoch).await?;
        if inv.manifest_sha256 != spec.manifest_sha256
            || !bindings.contains(&inv.binding)
            || inventories.insert(inv.binding, (key, inv)).is_some()
        {
            return Err(Error::Invalid("restore review inventory selection"));
        }
    }
    let now: i64 = tx
        .query_one("SELECT extract(epoch FROM clock_timestamp())::bigint", &[])
        .await?
        .get(0);
    let mut remaining = MAX_WORK;
    let mut reports = Vec::new();
    let mut incomplete = 0;
    for binding in bindings {
        let ctx = restore::retained_context(
            tx,
            auth,
            epoch,
            binding,
            &spec.manifest_sha256,
            Permission::RepositoryWrite,
        )
        .await?;
        let ctx_sha = restore_digest(&ctx)?;
        let decisions = decisions(tx, s, &ctx, &mut remaining).await?;
        let (mut git, native) = retained_sources(tx, s, &ctx).await?;
        budget(&mut remaining, git.len() + native.len())?;
        let mut inventory_status = "missing";
        let mut inventory_fresh = false;
        let mut inventory_receipt = Value::Null;
        if let Some((key, inv)) = inventories.get(&binding) {
            inventory_status = if inv.context_sha256 != ctx_sha {
                "stale_context"
            } else if !restore_inventory::sealed(tx, s, key).await? {
                "incomplete"
            } else {
                "sealed"
            };
            let finished = inv.finished_at_unix as i64;
            inventory_fresh =
                finished <= now.saturating_add(5) && finished >= now.saturating_sub(120);
            inventory_receipt = json!({"key":key,"specification_sha256":restore_digest(inv)?,"capture_finished_at_unix":inv.finished_at_unix,"token_revoked":inv.token_revoked});
            if inventory_status == "sealed" {
                for index in 0..inv.page_sha256.len() {
                    let page = restore_inventory::page(tx, s, key, index).await?;
                    inv.validate_page(&page)?;
                    budget(&mut remaining, page.items.len())?;
                    for item in page.items {
                        match item {
                            RestoreInventoryItem::Branch { git_revision, .. } => {
                                git.insert(git_revision);
                            }
                            RestoreInventoryItem::Pull {
                                head_revision,
                                base_revision,
                                merge_revision,
                                ..
                            } => {
                                git.insert(head_revision);
                                git.insert(base_revision);
                                git.extend(merge_revision);
                            }
                        }
                    }
                }
            }
        }
        let sources = source_summary(tx, s, &ctx, git, native).await?;
        let complete = inventory_status == "sealed"
            && inventory_fresh
            && decisions["blockers"]["count"] == 0
            && sources["missing_git"]["count"] == 0
            && sources["missing_native"]["count"] == 0;
        if !complete {
            incomplete += 1;
        }
        reports.push(json!({"binding":binding,"context_sha256":ctx_sha,"inventory_status":inventory_status,"inventory_fresh":inventory_fresh,"inventory":inventory_receipt,
                "decisions":decisions,"sources":sources,"local_review_complete":complete}));
    }
    let assessment = json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"epoch":epoch,"manifest_sha256":spec.manifest_sha256,
            "bindings":reports,"incomplete_bindings":incomplete,"local_review_complete":incomplete==0,"external_fencing_verified":false,
            "prior_host_mutations_settled":false,"reconciliation_complete":false,"publication_authorized":false});
    if serde_json::to_vec(&assessment)
        .map_err(|_| Error::SchemaMismatch)?
        .len()
        > 512 * 1024
    {
        return Err(Error::RestoreContextLimit);
    }
    context(tx, auth, epoch, Permission::RepositoryWrite).await?;
    Ok(
        json!({"version":1,"observed_at_unix":now,"assessment_sha256":restore_digest(&assessment)?,"assessment":assessment}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_wire_and_samples_do_not_accept_or_hide_admission() {
        let v: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-review-v1.json"))
                .unwrap();
        let c: WireCommand = serde_json::from_value(v["request"].clone()).unwrap();
        assert_eq!(c.permission(), Permission::RepositoryWrite);
        let WireCommand::RestoreReview { specification, .. } = c else {
            panic!()
        };
        specification.validate().unwrap();
        assert_eq!(
            restore_digest(&specification).unwrap(),
            v["specification_sha256"]
        );
        let mut duplicate = specification.clone();
        duplicate
            .inventory_keys
            .push(duplicate.inventory_keys[0].clone());
        assert!(duplicate.validate().is_err());
        for field in [
            "publication_authorized",
            "local_review_complete",
            "external_fencing_verified",
        ] {
            let mut bad = v["request"].clone();
            bad["specification"][field] = json!(true);
            assert!(serde_json::from_value::<WireCommand>(bad).is_err());
        }
        let mut values: Vec<_> = (0..12).map(|n| json!(n)).collect();
        let a = summary(&values).unwrap();
        values[11] = json!(99);
        let b = summary(&values).unwrap();
        assert_eq!(a["count"], 12);
        assert_eq!(a["sample"], b["sample"]);
        assert_eq!(a["sample_truncated"], true);
        assert_ne!(a["all_sha256"], b["all_sha256"]);
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/restore-context-v2.json"))
                .unwrap();
        let ctx: RestoreContext = serde_json::from_value(fixture["context"].clone()).unwrap();
        let subjects = subjects(&ctx);
        assert_eq!(subjects.len(), 4);
        assert_eq!(
            subjects
                .iter()
                .filter(|s| matches!(s, RestoreDecisionSubject::RetainedDequeue { .. }))
                .count(),
            1
        );
        assert_eq!(
            subjects
                .iter()
                .filter(|s| matches!(s, RestoreDecisionSubject::RetainedRecovery { .. }))
                .count(),
            1
        );
        assert_eq!(
            subjects
                .iter()
                .filter(|s| matches!(s, RestoreDecisionSubject::RetainedRedelivery { .. }))
                .count(),
            2
        );
        let mut left = 1;
        budget(&mut left, 1).unwrap();
        assert!(budget(&mut left, 1).is_err());
    }
}
