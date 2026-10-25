use super::candidates::{candidate, fresh};
use super::*;
use std::collections::BTreeSet;
use treeharbor_validation::{self as validation, ContextDigest, Receipt};

pub(crate) async fn trusted(
    tx: &Transaction<'_>,
    s: &Scope,
    principal: &str,
    kind: &str,
    permission: &str,
    token: Option<Uuid>,
) -> Result<bool> {
    // Principal disable is global and does not take the repository lock. Hold
    // the exact evidence principals through publication, rather than relying on
    // an MVCC existence read that may outlive a completed disable transaction.
    let allowed=tx.query_opt(r"SELECT p.principal FROM th_principals p JOIN th_repository_access a USING(tenant,principal)
        WHERE p.tenant=$1 AND a.repository=$2 AND p.principal=$3 AND p.kind=$4 AND p.enabled AND $5=ANY(a.permissions)
        AND ($6::uuid IS NULL OR EXISTS(SELECT 1 FROM th_access_tokens t WHERE t.tenant=p.tenant AND t.repository=a.repository AND t.principal=p.principal AND t.token=$6 AND t.revoked_at IS NULL AND $5=ANY(t.permissions)))
        FOR SHARE OF p,a",
        &[&s.tenant,&s.repository,&principal,&kind,&permission,&token]).await?.is_some();
    // Expiry limits future submissions; explicit credential revocation also
    // invalidates pending evidence. Repository/principal grants remain current.
    Ok(allowed)
}
fn outcome(value: &str) -> Result<validation::Outcome> {
    Ok(match value {
        "passed" => validation::Outcome::Passed,
        "failed" => validation::Outcome::Failed,
        "error" => validation::Outcome::Error,
        "cancelled" => validation::Outcome::Cancelled,
        _ => return Err(Error::Invalid("validation outcome")),
    })
}
pub(super) async fn current_run(
    tx: &Transaction<'_>,
    s: &Scope,
    run: Uuid,
) -> Result<(Uuid, String, String, Vec<u8>)> {
    let row=tx.query_opt("SELECT r.candidate,r.name,r.validator,r.context FROM th_native_check_runs r JOIN th_native_current_checks c USING(tenant,repository,candidate,name,run) WHERE r.tenant=$1 AND r.repository=$2 AND r.run=$3 AND NOT r.revoked", &[&s.tenant,&s.repository,&run]).await?.ok_or(Error::IntegrationState)?;
    Ok((row.get(0), row.get(1), row.get(2), row.get(3)))
}

impl Metadata {
    pub async fn assign_native_check(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
        name: &str,
        run: Uuid,
        validator: &str,
    ) -> Result<Value> {
        crate::identifier(name, 128)?;
        crate::identifier(validator, 128)?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.check.assign.v1",
                json!([
                    1,
                    candidate_id.to_string(),
                    name,
                    run.to_string(),
                    validator
                ]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let c = candidate(&tx.tx, s, candidate_id).await?;
        if c.state != "checking" {
            return Err(Error::IntegrationState);
        }
        fresh(&tx.tx, s, &c).await?;
        let policy = load_policy(&tx.tx, s, c.policy.as_bytes()).await?;
        let check = policy.check(name, &run.to_string())?;
        if !check.trusted_validators.contains(validator)
            || !trusted(&tx.tx, s, validator, "validator", "validation.report", None).await?
        {
            return Err(Error::Forbidden);
        }
        let context = validation::context_digest(&c.validation(s)?, &check);
        let inserted=tx.tx.execute("INSERT INTO th_native_check_runs(tenant,repository,candidate,name,run,validator,context) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING", &[&s.tenant,&s.repository,&candidate_id,&name,&run,&validator,&context.as_bytes().as_slice()]).await?;
        if inserted != 1 {
            return Err(Error::IntegrationState);
        }
        tx.tx.execute("INSERT INTO th_native_current_checks(tenant,repository,candidate,name,run) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant,repository,candidate,name) DO UPDATE SET run=EXCLUDED.run", &[&s.tenant,&s.repository,&candidate_id,&name,&run]).await?;
        let result = json!({"candidate":candidate_id.to_string(),"check":name,"run":run.to_string(),"validator":validator,"context":ObjectId::from_bytes(*context.as_bytes()).to_string(),"revision":RevisionRef::Native(c.revision.ok_or(Error::IntegrationState)?).to_string()});
        tx.complete(
            result.clone(),
            vec![Event {
                kind: "native.validation.run".into(),
                payload: result,
            }],
        )
        .await
    }

    pub async fn report_native_validation(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        run: Uuid,
        receipt: Uuid,
        result: &str,
        evidence: ObjectId,
    ) -> Result<Value> {
        outcome(result)?;
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.validation.report.v1",
                json!([
                    1,
                    run.to_string(),
                    receipt.to_string(),
                    result,
                    evidence.to_string()
                ]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let (candidate_id, name, validator, context) = current_run(&tx.tx, s, run).await?;
        if validator != auth.principal() {
            return Err(Error::Forbidden);
        }
        let c = candidate(&tx.tx, s, candidate_id).await?;
        if c.state != "checking" {
            return Err(Error::IntegrationState);
        }
        fresh(&tx.tx, s, &c).await?;
        let policy = load_policy(&tx.tx, s, c.policy.as_bytes()).await?;
        let check = policy.check(&name, &run.to_string())?;
        if super::program::known_program(&tx.tx, s, &check.definition_digest).await? {
            return Err(Error::Forbidden);
        }
        tx.tx.execute("INSERT INTO th_native_validations(tenant,repository,receipt,run,principal,token,context,outcome,evidence) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) ON CONFLICT DO NOTHING", &[&s.tenant,&s.repository,&receipt,&run,&auth.principal(),&auth.token_id(),&context,&result,&bytes(evidence)]).await?;
        let old=tx.tx.query_one("SELECT run,principal,token,context,outcome,evidence FROM th_native_validations WHERE tenant=$1 AND repository=$2 AND receipt=$3", &[&s.tenant,&s.repository,&receipt]).await?;
        if old.get::<_, Uuid>(0) != run
            || old.get::<_, String>(1) != auth.principal()
            || old.get::<_, Uuid>(2) != auth.token_id()
            || old.get::<_, Vec<u8>>(3) != context
            || old.get::<_, String>(4) != result
            || old.get::<_, Vec<u8>>(5) != bytes(evidence)
        {
            return Err(Error::IdempotencyConflict);
        }
        tx.complete(json!({"receipt":receipt.to_string(),"run":run.to_string(),"candidate":candidate_id.to_string(),"outcome":result}),vec![]).await
    }

    pub async fn revoke_native_validation(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        run: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.validation.revoke.v1",
                json!([1, run.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let row=tx.tx.query_opt("SELECT validator FROM th_native_check_runs WHERE tenant=$1 AND repository=$2 AND run=$3", &[&s.tenant,&s.repository,&run]).await?.ok_or(Error::NotFound)?;
        if row.get::<_, String>(0) != auth.principal() {
            return Err(Error::Forbidden);
        }
        tx.tx.execute("UPDATE th_native_check_runs SET revoked=true WHERE tenant=$1 AND repository=$2 AND run=$3", &[&s.tenant,&s.repository,&run]).await?;
        tx.complete(json!({"run":run.to_string(),"revoked":true}), vec![])
            .await
    }

    pub async fn record_native_review(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
        review: Uuid,
        decision: &str,
    ) -> Result<Value> {
        if !["approve", "reject"].contains(&decision) {
            return Err(Error::Invalid("review decision"));
        }
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.review.record.v1",
                json!([1, candidate_id.to_string(), review.to_string(), decision]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let c = candidate(&tx.tx, s, candidate_id).await?;
        if c.state != "checking" {
            return Err(Error::IntegrationState);
        }
        fresh(&tx.tx, s, &c).await?;
        let policy = load_policy(&tx.tx, s, c.policy.as_bytes()).await?;
        if c.owner == auth.principal() || !policy.reviewers.contains(auth.principal()) {
            return Err(Error::Forbidden);
        }
        tx.tx.execute("INSERT INTO th_native_reviews(tenant,repository,review,candidate,principal,token,decision) VALUES($1,$2,$3,$4,$5,$6,$7) ON CONFLICT DO NOTHING", &[&s.tenant,&s.repository,&review,&candidate_id,&auth.principal(),&auth.token_id(),&decision]).await?;
        let old=tx.tx.query_one("SELECT candidate,principal,token,decision,revoked FROM th_native_reviews WHERE tenant=$1 AND repository=$2 AND review=$3", &[&s.tenant,&s.repository,&review]).await?;
        if old.get::<_, Uuid>(0) != candidate_id
            || old.get::<_, String>(1) != auth.principal()
            || old.get::<_, Uuid>(2) != auth.token_id()
            || old.get::<_, String>(3) != decision
        {
            return Err(Error::IdempotencyConflict);
        }
        tx.complete(json!({"review":review.to_string(),"candidate":candidate_id.to_string(),"decision":decision,"revoked":old.get::<_,bool>(4)}),vec![]).await
    }

    pub async fn revoke_native_review(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        review: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.review.revoke.v1",
                json!([1, review.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let row=tx.tx.query_opt("SELECT principal FROM th_native_reviews WHERE tenant=$1 AND repository=$2 AND review=$3", &[&s.tenant,&s.repository,&review]).await?.ok_or(Error::NotFound)?;
        if row.get::<_, String>(0) != auth.principal() {
            return Err(Error::Forbidden);
        }
        tx.tx.execute("UPDATE th_native_reviews SET revoked=true WHERE tenant=$1 AND repository=$2 AND review=$3", &[&s.tenant,&s.repository,&review]).await?;
        tx.complete(json!({"review":review.to_string(),"revoked":true}), vec![])
            .await
    }

    pub async fn land_native_candidate(
        &mut self,
        auth: &Authorization,
        cmd: &Command,
        candidate_id: Uuid,
    ) -> Result<Value> {
        let tx = match self
            .begin(request(
                auth,
                cmd,
                "native.candidate.land.v1",
                json!([1, candidate_id.to_string()]),
            )?)
            .await?
        {
            Begin::Replay(v) => return Ok(v),
            Begin::Fresh(tx) => tx,
        };
        let s = auth.scope();
        let c = candidate(&tx.tx, s, candidate_id).await?;
        if c.state == "landed" {
            return tx.complete(c.json(), vec![]).await;
        }
        if c.state != "checking" {
            return Err(Error::IntegrationState);
        }
        let current = fresh(&tx.tx, s, &c).await?;
        let policy = load_policy(&tx.tx, s, c.policy.as_bytes()).await?;
        let subject = c.validation(s)?;
        let mut checks = Vec::new();
        let mut receipts = Vec::new();
        for definition in &policy.checks {
            let row=tx.tx.query_opt("SELECT run FROM th_native_current_checks WHERE tenant=$1 AND repository=$2 AND candidate=$3 AND name=$4", &[&s.tenant,&s.repository,&candidate_id,&definition.name]).await?.ok_or(Error::AdmissionBlocked)?;
            let run: Uuid = row.get(0);
            let (_, name, validator, context) = current_run(&tx.tx, s, run).await.map_err(|e| {
                if matches!(e, Error::IntegrationState) {
                    Error::AdmissionBlocked
                } else {
                    e
                }
            })?;
            let check = policy.check(&name, &run.to_string())?;
            let expected = validation::context_digest(&subject, &check);
            if context != expected.as_bytes() || !check.trusted_validators.contains(&validator) {
                return Err(Error::AdmissionBlocked);
            }
            let rows=tx.tx.query("SELECT receipt,principal,token,context,outcome FROM th_native_validations WHERE tenant=$1 AND repository=$2 AND run=$3 LIMIT 2", &[&s.tenant,&s.repository,&run]).await?;
            // Competing definitive receipts block even when both say passed.
            if rows.len() != 1 {
                return Err(Error::AdmissionBlocked);
            }
            let row = &rows[0];
            let principal: String = row.get(1);
            if principal != validator
                || !trusted(
                    &tx.tx,
                    s,
                    &principal,
                    "validator",
                    "validation.report",
                    Some(row.get(2)),
                )
                .await?
            {
                return Err(Error::AdmissionBlocked);
            }
            if super::program::known_program(&tx.tx, s, &check.definition_digest).await? {
                super::execution::admission(&tx.tx, s, run, row.get(0)).await?;
            }
            receipts.push(Receipt {
                id: row.get::<_, Uuid>(0).to_string(),
                context: ContextDigest::from_bytes(
                    row.get::<_, Vec<u8>>(3)
                        .try_into()
                        .map_err(|_| Error::SchemaMismatch)?,
                ),
                validator: principal,
                outcome: outcome(&row.get::<_, String>(4))?,
            });
            checks.push(check);
        }
        let target = validation::CurrentTarget {
            repository_scope: subject.repository_scope.clone(),
            authority_epoch: current.epoch as u64,
            target_ref: current.name.clone(),
            head: RevisionRef::Native(current.head),
            generation: current.generation as u64,
            policy_digest: *current.policy.as_bytes(),
            active: true,
        };
        if validation::evaluate_checks(&subject, &target, &checks, &receipts, &BTreeSet::new())
            != validation::CheckDecision::Satisfied
        {
            return Err(Error::AdmissionBlocked);
        }
        let reviews=tx.tx.query("SELECT principal,token,decision FROM th_native_reviews WHERE tenant=$1 AND repository=$2 AND candidate=$3 AND NOT revoked LIMIT 4097", &[&s.tenant,&s.repository,&candidate_id]).await?;
        if reviews.len() > 4096 {
            return Err(Error::AdmissionBlocked);
        }
        let mut approved = BTreeSet::new();
        for review in reviews {
            let reviewer: String = review.get(0);
            if reviewer == c.owner
                || !policy.reviewers.contains(&reviewer)
                || !trusted(&tx.tx, s, &reviewer, "human", "review", Some(review.get(1))).await?
            {
                continue;
            }
            if review.get::<_, String>(2) == "reject" {
                return Err(Error::AdmissionBlocked);
            }
            approved.insert(reviewer);
        }
        if approved.len() < usize::from(policy.approvals) {
            return Err(Error::AdmissionBlocked);
        }
        let revision = c.revision.ok_or(Error::IntegrationState)?;
        let pin = format!("native-published:{}", revision.hex());
        tx.copy_root_pin(c.pin.as_deref().ok_or(Error::IntegrationState)?, &pin)
            .await?;
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(Error::Invalid("ref generation"))?;
        let changed=tx.tx.execute("UPDATE th_native_refs SET head=$4,generation=$5,pin_holder=$6 WHERE tenant=$1 AND repository=$2 AND name=$3 AND head=$7 AND generation=$8 AND policy=$9 AND epoch=$10", &[&s.tenant,&s.repository,&c.target,&bytes(revision),&generation,&pin,&bytes(current.head),&current.generation,&bytes(current.policy),&c.epoch]).await?;
        if changed != 1 {
            return Err(Error::StaleCandidate);
        }
        tx.ref_update(
            &c.target,
            generation,
            Some(current.head),
            revision,
            current.policy,
            Some(candidate_id),
            "land",
        )
        .await?;
        tx.tx.execute("UPDATE th_native_candidates SET state='landed' WHERE tenant=$1 AND repository=$2 AND candidate=$3", &[&s.tenant,&s.repository,&candidate_id]).await?;
        tx.tx.execute("UPDATE th_native_queue SET state='landed' WHERE tenant=$1 AND repository=$2 AND entry=$3", &[&s.tenant,&s.repository,&c.entry]).await?;
        let result = candidate(&tx.tx, s, candidate_id).await?.json();
        tx.complete(result,vec![Event{kind:"native.ref.published".into(),payload:json!({"target":c.target,"revision":RevisionRef::Native(revision).to_string(),"generation":generation,"candidate":candidate_id.to_string()})}]).await
    }
}
