//! A selected check may publish before its peers; this is not merge admission.
use super::*;
use treeharbor_github_contract::{check_dispatch::PolicyProof, check_publication as cp};

fn preflight_digest(p: &cp::Preflight) -> Result<String> {
    treeharbor_github_contract::check_dispatch::preflight_digest(p)
        .map_err(|_| Error::SchemaMismatch)
}

pub(in crate::github) async fn retained(tx: &Transaction<'_>, s: &Scope, r: &Row) -> Result<Value> {
    let id: Uuid = r.get("publication");
    let mut v = json!({"version":1,"publication":id,"recorded":false,"proof":null,"digest":null,"merge_verified":false});
    if let Some(row) = tx.query_opt("SELECT * FROM th_github_check_dispatch_policy WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&id]).await? {
        let proof: PolicyProof = serde_json::from_value(row.get("proof")).map_err(|_| Error::SchemaMismatch)?;
        let digest: ObjectId = proof.digest().map_err(|_| Error::SchemaMismatch)?.parse()?;
        let p: cp::Preflight = serde_json::from_value(r.get::<_,Option<Value>>("preflight").ok_or(Error::SchemaMismatch)?).map_err(|_|Error::SchemaMismatch)?;
        if proof.tenant != s.tenant() || proof.repository != s.repository()
            || proof.target != super::super::check_publications::target(r)?
            || proof.check_run != r.get::<_,Uuid>("check_run")
            || proof.queue != row.get::<_,Uuid>("queue")
            || proof.host_policy_sequence != row.get::<_,i64>("host_policy_sequence")
            || proof.collection_key != row.get::<_,String>("collection_key")
            || proof.preflight_digest != preflight_digest(&p)?
            || row.get::<_,Vec<u8>>("digest") != digest.as_bytes() {
            return Err(Error::SchemaMismatch);
        }
        v["recorded"] = json!(true);
        v["proof"] = json!(proof);
        v["digest"] = json!(digest.to_string());
    }
    Ok(v)
}

async fn evaluate(
    tx: &Transaction<'_>,
    s: &Scope,
    publication: &Row,
    preflight: &cp::Preflight,
) -> Result<Option<PolicyProof>> {
    let t = super::super::check_publications::target(publication)?;
    let cp::Source::Group {
        queue: expected,
        target: selected,
    } = &t.source
    else {
        return Ok(None);
    };
    if t.conclusion != cp::Conclusion::Success {
        return Ok(None);
    }
    let c = check(tx, s, publication.get("check_run")).await?;
    let Some((_, policy)) = admission_policy::check_policy(tx, s, &c).await? else {
        return Ok(None);
    };
    let Some(configuration) = policy.host_configuration else {
        return Ok(None);
    };
    let e = execution(tx, s, c.get("check_run"))
        .await?
        .ok_or(Error::ExecutionState)?;
    // derive checks the selected immutable program, validator, runtime and owner
    // evidence. It deliberately does not require future provider check receipts.
    let (_, derived) = super::publication::derive(tx, s, c.get("check_run"), t.publication).await?;
    if derived != t || !preflight.eligible(&t) {
        return Err(Error::HostPublicationState);
    }
    let q = queue::load(tx, s, c.get("queue")).await?;
    if queue::target(&q)? != **expected {
        return Err(Error::HostQueueState);
    }
    let collection = tx.query_opt("SELECT key,(extract(epoch from committed_at)*1000)::bigint AS at FROM th_operations WHERE tenant=$1 AND repository=$2 AND kind='github.managed_check.collect.v1' AND result->>'check_run'=$3 ORDER BY committed_at,key LIMIT 1", &[&s.tenant(),&s.repository(),&c.get::<_,Uuid>("check_run").to_string()]).await?.ok_or(Error::ExecutionState)?;
    let sequence: Option<i64> = tx.query_one("SELECT max(sequence) FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND report->'host_policy' IS NOT NULL AND report->'host_policy'<>'null'::jsonb", &[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue")]).await?.get(0);
    let sequence = sequence.ok_or(Error::HostQueueState)?;
    let (_, verified) =
        host_policy::current::evaluate(tx, s, &q, sequence, selected, collection.get("at")).await?;
    if !verified {
        return Err(Error::HostQueueState);
    }
    let report: QueueObservation = serde_json::from_value(tx.query_one("SELECT report FROM th_github_queue_reports WHERE tenant=$1 AND repository=$2 AND queue=$3 AND sequence=$4", &[&s.tenant(),&s.repository(),&q.get::<_,Uuid>("queue"),&sequence]).await?.get("report")).map_err(|_|Error::SchemaMismatch)?;
    let capture = report.host_policy.as_ref().ok_or(Error::SchemaMismatch)?;
    // Join the immediate transport reads to the complete capture. Queue-setting
    // omission is explicit in ordinary membership reads; all known fields match.
    for source in [&preflight.before, &preflight.after] {
        let cp::SourceView::Group(group) = source else {
            return Err(Error::HostPublicationState);
        };
        let mut group = *group.clone();
        if group.target.configuration.is_none() {
            group.target.configuration = capture.before.group.target.configuration.clone();
        }
        if group != capture.before.group || group != capture.after.group {
            return Err(Error::HostQueueState);
        }
    }
    for (observed, captured) in [
        (&preflight.policy_before, &capture.before),
        (&preflight.policy_after, &capture.after),
    ] {
        let raw = captured
            .protection
            .value
            .as_ref()
            .ok_or(Error::SchemaMismatch)?;
        let bytes = serde_json::to_vec(raw).map_err(|_| Error::SchemaMismatch)?;
        let protection_digest = ObjectId::from_bytes(Sha256::digest(bytes).into()).to_string();
        if observed.protection.sha256 != protection_digest {
            return Err(Error::HostQueueState);
        }
    }
    let preflight_digest = preflight_digest(preflight)?;
    let host_policy_digest = capture
        .digest(expected)
        .map_err(|_| Error::SchemaMismatch)?;
    let proof = PolicyProof {
        version: 1,
        tenant: s.tenant().into(),
        repository: s.repository(),
        queue: q.get("queue"),
        check_run: c.get("check_run"),
        execution_run: e.get("execution_run"),
        collection_key: collection.get("key"),
        collected_at_unix_ms: collection.get("at"),
        decided_at_unix_ms: q.get("now_ms"),
        target: t,
        host_policy_sequence: sequence,
        host_policy_digest,
        configuration_digest: configuration
            .digest(&configuration.visibility.target)
            .map_err(|_| Error::SchemaMismatch)?,
        preflight_digest,
        capture_started_at_unix_ms: report.started_at_unix_ms,
        capture_finished_at_unix_ms: report.finished_at_unix_ms,
    };
    proof.validate().map_err(|_| Error::HostQueueState)?;
    Ok(Some(proof))
}

pub(in crate::github) async fn retain(
    tx: &Transaction<'_>,
    s: &Scope,
    r: &Row,
    p: &cp::Preflight,
) -> Result<()> {
    if let Some(proof) = evaluate(tx, s, r, p).await? {
        let digest: ObjectId = proof.digest().map_err(|_| Error::SchemaMismatch)?.parse()?;
        tx.execute("INSERT INTO th_github_check_dispatch_policy(tenant,repository,publication,queue,host_policy_sequence,collection_key,proof,digest) VALUES($1,$2,$3,$4,$5,$6,$7,$8)", &[&s.tenant(),&s.repository(),&proof.target.publication,&proof.queue,&proof.host_policy_sequence,&proof.collection_key,&json!(proof),&digest.as_bytes().as_slice()]).await?;
    }
    Ok(())
}

pub(in crate::github) async fn recheck(tx: &Transaction<'_>, s: &Scope, r: &Row) -> Result<()> {
    let p: cp::Preflight = serde_json::from_value(
        r.get::<_, Option<Value>>("preflight")
            .ok_or(Error::SchemaMismatch)?,
    )
    .map_err(|_| Error::SchemaMismatch)?;
    let current = evaluate(tx, s, r, &p).await?;
    let stored = tx.query_opt("SELECT proof,digest FROM th_github_check_dispatch_policy WHERE tenant=$1 AND repository=$2 AND publication=$3", &[&s.tenant(),&s.repository(),&r.get::<_,Uuid>("publication")]).await?;
    match (current, stored) {
        (None, None) => Ok(()),
        (Some(mut current), Some(stored)) => {
            let old: PolicyProof =
                serde_json::from_value(stored.get("proof")).map_err(|_| Error::SchemaMismatch)?;
            let digest: ObjectId = old.digest().map_err(|_| Error::SchemaMismatch)?.parse()?;
            current.decided_at_unix_ms = old.decided_at_unix_ms;
            if current != old || stored.get::<_, Vec<u8>>("digest") != digest.as_bytes() {
                return Err(Error::HostQueueState);
            }
            Ok(())
        }
        _ => Err(Error::SchemaMismatch),
    }
}
