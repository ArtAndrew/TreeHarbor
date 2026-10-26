//! Bounded, read-only evaluation of retained candidate checks. No merge authority.
use super::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use treeharbor_core::ObjectId;
use treeharbor_github_contract::{
    RequiredCheck, check_publication as cp, group_checks as gc,
    publication as publication_contract,
    queue::{QueueObservation, QueueTarget},
};

async fn report(
    tx: &Transaction<'_>,
    s: &Scope,
    id: Uuid,
    sequence: i64,
    target: &QueueTarget,
) -> Result<QueueObservation> {
    if !(1..=4096).contains(&sequence) {
        return Err(Error::Invalid("evidence sequence"));
    }
    let row = tx.query_opt("SELECT report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4", &[&s.tenant(), &s.repository(), &id, &sequence]).await?.ok_or(Error::NotFound)?;
    let r: QueueObservation =
        serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
    r.validate(target).map_err(|_| Error::SchemaMismatch)?;
    Ok(r)
}
fn matches<'a>(snapshot: &'a gc::Snapshot, required: &RequiredCheck) -> Vec<&'a gc::Run> {
    snapshot
        .suites
        .iter()
        .filter(|s| s.app_id == Some(required.app_id))
        .flat_map(|s| s.runs.iter().filter(|r| r.name == required.name))
        .collect()
}
fn digest(evidence: &Value) -> Result<String> {
    let mut h = Sha256::new();
    h.update(b"treeharbor-connected-check-set\0v1\0");
    h.update(serde_json::to_vec(evidence).map_err(|_| Error::SchemaMismatch)?);
    Ok(ObjectId::from_bytes(h.finalize().into()).to_string())
}
fn eligible_error(e: &Error) -> bool {
    matches!(
        e,
        Error::Forbidden | Error::Fenced | Error::HostQueueState | Error::ExecutionState
    )
}

struct Evaluation<'a> {
    scope: &'a Scope,
    queue: Uuid,
    binding: Uuid,
    epoch: i64,
    app_id: u64,
    target: &'a QueueTarget,
    selected: &'a publication_contract::Target,
    checks: &'a gc::GroupCheckReport,
    policy: &'a AdmissionPolicy,
    admission: cp::AdmissionContext,
    required: BTreeSet<RequiredCheck>,
}
impl Evaluation<'_> {
    async fn workflow(
        &self,
        tx: &Transaction<'_>,
        q: &Row,
        sequence: i64,
        w: &treeharbor_github_contract::workflow_definitions::CheckApproval,
    ) -> Result<Value> {
        let s = self.scope;
        let mut v = json!({"kind":"workflow","required":w.required,"verified":false,
            "attempt_linked":false,"workflow_file_matches_policy":false,"reasons":["workflow_context_unproven"]});
        let runs = matches(&self.checks.checks.before, &w.required);
        let Some(run) = runs.first().filter(|_| runs.len() == 1) else {
            v["reasons"] = json!([
                "workflow_context_unproven",
                if runs.is_empty() {
                    "missing_host_check"
                } else {
                    "ambiguous_host_check"
                }
            ]);
            return Ok(v);
        };
        let row = tx.query_opt("SELECT sequence,report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report#>>'{workflow_definition,target,check,check_sequence}'=$4 AND report#>>'{workflow_definition,target,check,check,id}'=$5 ORDER BY sequence DESC LIMIT 1", &[&s.tenant(), &s.repository(), &self.queue, &sequence.to_string(), &run.id]).await?;
        if let Some(row) = row {
            let r: QueueObservation =
                serde_json::from_value(row.get("report")).map_err(|_| Error::SchemaMismatch)?;
            r.validate(self.target).map_err(|_| Error::SchemaMismatch)?;
            let d = r.workflow_definition.ok_or(Error::SchemaMismatch)?;
            let a = workflow_definitions::validate_report(tx, s, q, &d).await?;
            v["definition_sequence"] = json!(row.get::<_, i64>("sequence"));
            v["attempt_linked"] = json!(
                a.target.check_sequence == sequence
                    && a.target.check == **run
                    && a.target.workflow.event == "merge_group"
                    && a.current_attempt_matches()
            );
            v["workflow_file_matches_policy"] =
                json!(d.workflow_file_matches_requirement(self.target, w));
        }
        // A root-file match or provider attempt cannot prove the declared
        // environment, toolchain and transitive dependency inputs.
        Ok(v)
    }
    async fn managed(&self, tx: &Transaction<'_>, m: &ManagedRequirement) -> Result<(Value, bool)> {
        let s = self.scope;
        let program = m.program.digest()?;
        let required = RequiredCheck {
            app_id: self.app_id,
            name: cp::context_name(
                self.epoch,
                &program.to_string(),
                &self.target.pull.base_ref,
                Some(&self.admission),
            )
            .map_err(|_| Error::SchemaMismatch)?,
        };
        let mut v = json!({"kind":"managed","required":required,"verified":false,"reasons":[]});
        let mut reasons = Vec::new();
        let runs = matches(&self.checks.checks.before, &required);
        if runs.len() != 1 {
            reasons.push(if runs.is_empty() {
                "missing_host_check"
            } else {
                "ambiguous_host_check"
            });
            v["reasons"] = json!(reasons);
            return Ok((v, false));
        }
        let run = runs[0];
        if run.status != "COMPLETED" || run.conclusion.as_deref() != Some("SUCCESS") {
            reasons.push("host_check_not_successful");
        }
        // A receipt is selected by the opaque authenticated node ID, never by
        // context name alone. Explicit queue/membership filters exclude PR checks.
        let rows = tx.query("SELECT p.* FROM th_github_check_publications p JOIN th_github_managed_checks c USING(tenant,repository,check_run) WHERE p.tenant=$1 AND p.repository=$2 AND c.queue=$3 AND c.membership_sequence=$4 AND p.binding=$5 AND p.epoch=$6 AND p.program=$7 AND p.latest_report#>>'{check,node_id}'=$8 ORDER BY p.publication LIMIT 2", &[&s.tenant(), &s.repository(), &self.queue, &self.selected.membership_sequence, &self.binding, &self.epoch, &program.as_bytes().as_slice(), &run.id]).await?;
        if rows.len() != 1 {
            reasons.push(if rows.is_empty() {
                "publication_unlinked"
            } else {
                "ambiguous_publication"
            });
            v["reasons"] = json!(reasons);
            return Ok((v, false));
        }
        let row = &rows[0];
        let target = check_publications::target(row)?;
        let observed: cp::Report =
            serde_json::from_value(row.get("latest_report")).map_err(|_| Error::SchemaMismatch)?;
        observed
            .validate(&target)
            .map_err(|_| Error::SchemaMismatch)?;
        let expected_source = cp::Source::Group {
            queue: Box::new(self.target.clone()),
            target: Box::new(self.selected.clone()),
        };
        if row.get::<_, String>("state") != "observed"
            || observed.version != 2
            || observed.outcome != cp::Outcome::Observed
            || target.source != expected_source
            || target.admission.as_ref() != Some(&self.admission)
            || target.conclusion != cp::Conclusion::Success
        {
            reasons.push("publication_not_exact_success");
        }
        let check = observed.check.as_ref().ok_or(Error::SchemaMismatch)?;
        if check.node_id.as_deref() != Some(run.id.as_str())
            || check.name != required.name
            || check.app_id != required.app_id
            || check.head_revision != self.checks.checks.subject.revision
            || check.status != "completed"
            || check.conclusion != cp::Conclusion::Success
        {
            reasons.push("host_identity_mismatch");
        }
        let retained: Option<cp::Preflight> = row
            .get::<_, Option<Value>>("preflight")
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::SchemaMismatch)?;
        if observed.preflight != retained
            || retained.as_ref().is_none_or(|p| {
                !p.eligible(&target)
                    || [&p.policy_before, &p.policy_after].iter().any(|o| {
                        o.rules != self.policy.rules
                            || o.protection
                                .app_bound_checks
                                .iter()
                                .cloned()
                                .collect::<BTreeSet<_>>()
                                != self.required
                    })
            })
        {
            reasons.push("reviewed_policy_unproven");
        }
        // Verify the final receipt is the retained immutable report, not only the
        // publication's convenient latest-report projection.
        let receipt_sequence: i64 = row.get("report_sequence");
        let receipt = tx.query_opt("SELECT report,attempt,reporter,token FROM th_github_check_publication_reports WHERE tenant=$1 AND repository=$2 AND publication=$3 AND sequence=$4", &[&s.tenant(), &s.repository(), &target.publication, &receipt_sequence]).await?.ok_or(Error::SchemaMismatch)?;
        if receipt.get::<_, Value>("report") != json!(observed)
            || Some(receipt.get::<_, Uuid>("attempt")) != row.get::<_, Option<Uuid>>("attempt")
        {
            return Err(Error::SchemaMismatch);
        }
        let check_run: Uuid = row.get("check_run");
        match managed_checks::publication::verify_recorded_group(tx, s, check_run, &target).await {
            Ok(()) => (),
            Err(e) if eligible_error(&e) => reasons.push("collected_execution_unproven"),
            Err(e) => return Err(e),
        }
        let validator: String = tx.query_one("SELECT validator FROM th_github_managed_checks WHERE tenant=$1 AND repository=$2 AND check_run=$3", &[&s.tenant(), &s.repository(), &check_run]).await?.get(0);
        if !m.validators.contains(&validator) {
            reasons.push("validator_not_in_policy");
        }
        let mut current =
            match managed_checks::publication::derive(tx, s, check_run, target.publication).await {
                Ok((b, current)) => b == self.binding && current == target,
                Err(e) if eligible_error(&e) => false,
                Err(e) => return Err(e),
            };
        let pending = tx.query_opt("SELECT 1 FROM th_github_check_publications WHERE tenant=$1 AND repository=$2 AND binding=$3 AND epoch=$4 AND head=$5 AND program=$6 AND context_name=$7 AND state IN ('requested','attempting','uncertain') LIMIT 1", &[&s.tenant(), &s.repository(), &self.binding, &self.epoch, &self.checks.checks.subject.revision, &program.as_bytes().as_slice(), &required.name]).await?.is_some();
        current &=
            !pending && check_publications::retained_approval_current(tx, s, row, &receipt).await?;
        v["check_run"] = json!(check_run);
        v["publication"] = json!(target.publication);
        v["receipt_sequence"] = json!(receipt_sequence);
        v["host_check"] = json!({"id":check.id,"node_id":check.node_id});
        v["evidence"] = json!(target.evidence);
        v["verified"] = json!(reasons.is_empty());
        v["reasons"] = json!(reasons);
        Ok((v, current))
    }
}

impl Metadata {
    pub async fn github_queue_admission_evidence(
        &mut self,
        auth: &Authorization,
        epoch: i64,
        id: Uuid,
        check_sequence: i64,
        publication_sequence: Option<i64>,
    ) -> Result<Value> {
        uuid(id)?;
        let tx = self.client.transaction().await?;
        settings(&tx).await?;
        let s = auth.scope();
        repository_lock(&tx, s, false).await?;
        context(&tx, auth, epoch, Permission::Read).await?;
        let q = queue::load(&tx, s, id).await?;
        if q.get::<_, i64>("epoch") != epoch {
            return Err(Error::Fenced);
        }
        let t = queue::target(&q)?;
        let r = report(&tx, s, id, check_sequence, &t).await?;
        let checks = r.group_checks.ok_or(Error::Invalid("check report"))?;
        let g = group_checks::validate_report(&tx, s, &q, &checks).await?;
        let view = match checks.target.side {
            gc::Side::Before => &g.before,
            gc::Side::After => &g.after,
        };
        let selected = publication_contract::Target {
            membership_sequence: checks.target.membership_sequence,
            member: view.selected(&t).ok_or(Error::SchemaMismatch)?.clone(),
        };
        let candidate = selected.candidate().map_err(|_| Error::SchemaMismatch)?;
        let binding_id: Uuid = q.get("binding");
        let b = binding(&tx, s, binding_id).await?;
        let spec = specification(&b)?;
        let pinned = admission_policy::pinned_queue(&tx, s, &q).await?;
        let mut reasons = Vec::new();
        let mut requirements = Vec::new();
        if !g.consistent(&t) {
            reasons.push("membership_inconsistent");
        }
        if !checks.checks.consistent(t.pull.repository_id) {
            reasons.push("check_snapshot_inconsistent");
        }
        let mut current = b.get::<_, String>("state") == "active"
            && b.get::<_, i64>("epoch") == epoch
            && !q.get::<_, bool>("cancel_requested")
            && queue::current(&tx, s, &q).await?;
        let latest: Option<QueueObservation> = q
            .get::<_, Option<Value>>("latest_observation")
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| Error::SchemaMismatch)?;
        current &= latest
            .and_then(|v| v.group_capture)
            .is_some_and(|v| v.consistent(&t) && v.before.selected(&t) == Some(&selected.member));
        let latest_checks: Option<i64> = tx.query_one("SELECT max(sequence) FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report->'group_checks' IS NOT NULL AND report->'group_checks'<>'null'::jsonb", &[&s.tenant(), &s.repository(), &id]).await?.get(0);
        current &= latest_checks == Some(check_sequence);
        match check_policy::fresh(&tx, s, &q).await {
            Ok(()) => (),
            Err(e) if eligible_error(&e) => current = false,
            Err(e) => return Err(e),
        }
        let policy = if let Some((pin, p, active)) = pinned {
            current &= active;
            let required = p.required(
                spec.app_id,
                epoch,
                binding_id,
                &t.pull.base_ref,
                pin.generation,
            )?;
            if required != t.pull.required.iter().cloned().collect() || p.rules != t.expected_rules
            {
                return Err(Error::SchemaMismatch);
            }
            let e = Evaluation {
                scope: s,
                queue: id,
                binding: binding_id,
                epoch,
                app_id: spec.app_id,
                target: &t,
                selected: &selected,
                checks: &checks,
                policy: &p,
                admission: cp::AdmissionContext {
                    binding: binding_id,
                    generation: pin.generation,
                    policy: pin.digest.clone(),
                },
                required,
            };
            for m in &p.managed {
                let (requirement, eligible) = e.managed(&tx, m).await?;
                requirements.push(requirement);
                current &= eligible;
            }
            for w in &p.workflows {
                requirements.push(e.workflow(&tx, &q, check_sequence, w).await?);
            }
            Some(pin)
        } else {
            reasons.push("admission_policy_unconfigured");
            current = false;
            None
        };
        let complete = reasons.is_empty()
            && !requirements.is_empty()
            && requirements.iter().all(|r| r["verified"] == true);
        let publication = if let Some(sequence) = publication_sequence {
            let r = report(&tx, s, id, sequence, &t).await?;
            let p = r.publication.ok_or(Error::Invalid("publication report"))?;
            publication::validate_report(&tx, s, &q, &p).await?;
            let exact = p.target == selected && p.exact_candidate_published(&t);
            Some(
                json!({"sequence":sequence,"exact_candidate_published":exact,"checked_candidate_published":exact && complete}),
            )
        } else {
            None
        };
        let evidence = json!({"version":1,"tenant":s.tenant(),"repository":s.repository(),"queue":id,"binding":binding_id,"epoch":epoch,
            "policy":policy,"check_sequence":check_sequence,"membership_sequence":selected.membership_sequence,"candidate":candidate,
            "requirements":requirements,"reasons":reasons,"complete":complete,"publication":publication});
        let result = json!({"version":1,"evidence_digest":digest(&evidence)?,"evidence":evidence,"current_locally":current,
            "checks_verified":complete && current,"host_policy_verified":false,"admission_verified":false,"merge_verified":false});
        context(&tx, auth, epoch, Permission::Read).await?;
        tx.commit().await?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn check_set_encoding_matches_independent_vector_and_binds_every_source() {
        let v: Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/connected-check-set-v1.json"
        ))
        .unwrap();
        let evidence = &v["evidence"];
        assert_eq!(
            serde_json::to_vec(evidence).unwrap(),
            v["canonical"].as_str().unwrap().as_bytes()
        );
        assert_eq!(digest(evidence).unwrap(), v["digest"]);
        for path in [
            "/tenant",
            "/repository",
            "/queue",
            "/binding",
            "/epoch",
            "/policy/generation",
            "/policy/digest",
            "/check_sequence",
            "/membership_sequence",
            "/candidate/commit/revision",
            "/candidate/commit/tree",
            "/candidate/parents/0/revision",
            "/requirements/0/publication",
            "/requirements/0/host_check/id",
            "/requirements/0/host_check/node_id",
            "/requirements/0/evidence",
            "/requirements/0/receipt_sequence",
            "/complete",
            "/publication",
        ] {
            let mut changed = evidence.clone();
            *changed.pointer_mut(path).unwrap() = json!("changed");
            assert_ne!(digest(&changed).unwrap(), v["digest"], "{path}");
        }
        let mut reversed = evidence.clone();
        reversed["candidate"]["parents"]
            .as_array_mut()
            .unwrap()
            .reverse();
        assert_ne!(digest(&reversed).unwrap(), v["digest"]);
    }
}
