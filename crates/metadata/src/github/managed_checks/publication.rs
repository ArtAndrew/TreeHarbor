//! A host target is derived from collected runtime evidence, never a caller outcome.
use super::*;
use treeharbor_github_contract::{RequiredCheck, check_publication as cp};

pub(in crate::github) async fn derive(
    tx: &Transaction<'_>,
    s: &Scope,
    check_run: Uuid,
    publication: Uuid,
) -> Result<(Uuid, cp::Target)> {
    let c = check(tx, s, check_run).await?;
    let e = execution(tx, s, check_run)
        .await?
        .ok_or(Error::ExecutionState)?;
    if e.get::<_, String>("state") != "collected" {
        return Err(Error::ExecutionState);
    }
    fresh(tx, s, &c).await?;
    validator(tx, s, &c, Some(&e)).await?;
    let (definition, evidence, outcome) = evidence(tx, s, &c, &e).await?;
    if e.get::<_, Option<Vec<u8>>>("definition").as_deref() != Some(definition.as_slice())
        || e.get::<_, Option<Vec<u8>>>("evidence").as_deref() != Some(evidence.as_slice())
        || e.get::<_, Option<String>>("outcome").as_deref() != Some(outcome.as_str())
    {
        return Err(Error::SchemaMismatch);
    }
    let epoch: i64 = c.get("epoch");
    let program = oid(c.get("program"))?.to_string();
    let (binding_id, source) = if let Some(id) = c.get::<_, Option<Uuid>>("pull") {
        let (binding_id, mut subject) = pull::source(tx, s, id, epoch).await?;
        if subject != c.get::<_, Value>("subject") {
            return Err(Error::ExecutionState);
        }
        let spec = specification(&binding(tx, s, binding_id).await?)?;
        subject
            .as_object_mut()
            .ok_or(Error::SchemaMismatch)?
            .remove("version");
        // Structural identity only: PR preflight must not depend on an already
        // passing inspection/queue or on its own result.
        subject["required"] = json!([RequiredCheck {
            name: format!("TreeHarbor/e{epoch}/{}", &program[7..]),
            app_id: spec.app_id
        }]);
        let target = serde_json::from_value(subject).map_err(|_| Error::SchemaMismatch)?;
        (binding_id, cp::Source::Pull { target })
    } else {
        let q = queue::load(tx, s, c.get("queue")).await?;
        let selected =
            serde_json::from_value(c.get("subject")).map_err(|_| Error::SchemaMismatch)?;
        (
            q.get("binding"),
            cp::Source::Group {
                queue: Box::new(queue::target(&q)?),
                target: Box::new(selected),
            },
        )
    };
    let spec = specification(&binding(tx, s, binding_id).await?)?;
    let target = cp::Target {
        version: 1,
        publication,
        epoch,
        app_id: spec.app_id,
        program,
        evidence: oid(evidence)?.to_string(),
        source,
        conclusion: match outcome.as_str() {
            "passed" => cp::Conclusion::Success,
            "failed" => cp::Conclusion::Failure,
            "cancelled" => cp::Conclusion::Cancelled,
            "error" => cp::Conclusion::ActionRequired,
            _ => return Err(Error::SchemaMismatch),
        },
    };
    target.validate().map_err(|_| Error::SchemaMismatch)?;
    Ok((binding_id, target))
}
