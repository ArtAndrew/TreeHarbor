use super::*;
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_github::RestoreInventory;
use treeharbor_metadata::github::{RestoreContext, RestoreEffectKind, restore_digest};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    config: PathBuf,
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long)]
    binding: Uuid,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long)]
    manifest_sha256: String,
}
async fn context(o: &Options, credential: &Credential) -> Result<RestoreContext> {
    let value = command(
        &o.socket,
        credential,
        json!({"action":"restore_context","epoch":o.epoch,
        "binding":o.binding,"manifest_sha256":o.manifest_sha256}),
    )
    .await?;
    let c: RestoreContext = serde_json::from_value(value).map_err(|_| Error::RestoreAssessment)?;
    if c.version != 2
        || c.tenant != credential.scope.tenant()
        || c.repository != credential.scope.repository()
        || c.epoch != o.epoch
        || c.binding != o.binding
        || c.manifest_sha256 != o.manifest_sha256
        || c.reconciliation_complete
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(c)
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let credential = Credential::load(&o.credential_file)?;
    let before = context(&o, &credential).await?;
    let (app, config) = load(&o.config)?;
    matches_config(&config, &before.specification)?;
    let inventory = Connection::new(app)?
        .observe_restore_inventory(&config.repository)
        .await?;
    let after = context(&o, &credential).await?;
    let report = assess(&before, &after, &inventory)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|_| Error::Http)?
    );
    Ok(inventory.token_revoked)
}

fn assess(
    before: &RestoreContext,
    after: &RestoreContext,
    inventory: &RestoreInventory,
) -> Result<Value> {
    if before != after
        || before.version != 2
        || before.reconciliation_complete
        || inventory.version != 1
        || inventory.reconciliation_complete
        || json!(before.specification.upstream) != json!(inventory.binding)
        || before.specification.app_id != inventory.app_id
    {
        return Err(Error::RestoreAssessment);
    }
    let heads: BTreeMap<_, _> = inventory
        .heads
        .iter()
        .map(|h| (h.name.as_str(), h))
        .collect();
    let pulls: BTreeMap<_, _> = inventory.pulls.iter().map(|p| (p.number, p)).collect();
    if heads.len() != inventory.heads.len() || pulls.len() != inventory.pulls.len() {
        return Err(Error::RestoreAssessment);
    }
    let mut known_branches = BTreeSet::new();
    let mut known_pulls = BTreeSet::new();
    let mut findings = Vec::new();
    let repository = inventory.binding.repository_id;
    for e in &before.effects {
        let finding = match e.kind {
            RestoreEffectKind::Branch => {
                let branch = format!(
                    "treeharbor/{}",
                    e.branch.ok_or(Error::RestoreAssessment)?.simple()
                );
                known_branches.insert(branch.clone());
                let actual = heads
                    .get(branch.as_str())
                    .map(|h| format!("git:sha1:{}", h.target.oid));
                let classification = match &actual {
                    None => "branch_absent",
                    Some(_) if actual == e.prepared_git => "head_matches_prepared",
                    Some(_) if actual == e.expected_git => "head_matches_expected",
                    Some(_) => "head_differs",
                };
                json!({"kind":"branch","operation":e.id,"branch":branch,"classification":classification,
                    "current_head":actual,"retained_state":e.state,"retained_attempt":e.attempt})
            }
            RestoreEffectKind::Pull => {
                let target = e.target.as_ref().ok_or(Error::RestoreAssessment)?;
                let branch = format!("treeharbor/{}", target.branch_id.simple());
                known_branches.insert(branch.clone());
                if let Some(number) = e.pull_number {
                    let number = u64::try_from(number).map_err(|_| Error::RestoreAssessment)?;
                    known_pulls.insert(number);
                    let p = pulls.get(&number);
                    let classification = match p {
                        None => "known_pull_absent",
                        Some(p)
                            if Some(&p.id) != e.pull_node.as_ref()
                                || p.base_repository.database_id != repository
                                || p.head_repository.as_ref().map(|r| r.database_id)
                                    != Some(repository) =>
                        {
                            "known_pull_identity_differs"
                        }
                        Some(p)
                            if p.head_ref_name != branch
                                || p.base_ref_name != target.base
                                || format!("git:sha1:{}", p.head_ref_oid)
                                    != target.git_revision =>
                        {
                            "known_pull_code_differs"
                        }
                        Some(_) => "known_pull_code_matches",
                    };
                    json!({"kind":"pull","operation":e.id,"classification":classification,"number":number,
                        "current_state":p.map(|p|p.state.as_str()),"retained_state":e.state,"retained_attempt":e.attempt,
                        "metadata_and_marker_verified":false})
                } else {
                    // Matching branches identify candidates only. Never invent a
                    // missing attempt or adopt a differently marked closed PR.
                    let candidates: Vec<_> = inventory
                        .pulls
                        .iter()
                        .filter(|p| {
                            p.base_repository.database_id == repository
                                && p.head_repository.as_ref().map(|r| r.database_id)
                                    == Some(repository)
                                && p.head_ref_name == branch
                                && p.base_ref_name == target.base
                        })
                        .map(|p| p.number)
                        .collect();
                    json!({"kind":"pull","operation":e.id,"classification":"pull_identity_unresolved",
                        "candidate_numbers":candidates,"retained_state":e.state,"retained_attempt":e.attempt,
                        "metadata_and_marker_verified":false})
                }
            }
            RestoreEffectKind::Queue | RestoreEffectKind::Check => {
                json!({"kind":e.kind,"operation":e.id,"classification":"external_inspection_required",
                    "retained_state":e.state,"retained_attempt":e.attempt})
            }
        };
        findings.push(finding);
    }
    for e in &before.dequeues {
        findings.push(json!({"kind":"dequeue","operation":e.id,"queue":e.queue,
            "retained_attempt":e.attempt,"classification":"external_inspection_required"}));
    }
    for e in &before.recoveries {
        findings.push(
            json!({"kind":"recovery","recovery":e.recovery,"channel":e.channel,
            "cancelled":e.cancelled,"classification":"external_inspection_required"}),
        );
    }
    for e in &before.redeliveries {
        findings.push(
            json!({"kind":"redelivery","recovery":e.recovery,"provider_id":e.provider_id,
            "delivery":e.delivery,"retained_dispatch":e.dispatch,
            "classification":"external_inspection_required"}),
        );
    }
    let unmapped_heads: Vec<_> = inventory
        .heads
        .iter()
        .filter(|h| h.name.starts_with("treeharbor/") && !known_branches.contains(&h.name))
        .map(|h| h.name.as_str())
        .collect();
    let unmapped_pulls: Vec<_> = inventory
        .pulls
        .iter()
        .filter(|p| {
            p.head_ref_name.starts_with("treeharbor/")
                && p.head_repository.as_ref().map(|r| r.database_id) == Some(repository)
                && !known_pulls.contains(&p.number)
        })
        .map(|p| p.number)
        .collect();
    let context_digest = restore_digest(before).map_err(|_| Error::RestoreAssessment)?;
    let inventory_digest = restore_digest(inventory).map_err(|_| Error::RestoreAssessment)?;
    Ok(
        json!({"version":2,"context":before,"context_sha256":context_digest,
        "inventory":inventory,"inventory_sha256":inventory_digest,"findings":findings,
        "unmapped_to_selected_binding":{"heads":unmapped_heads,"pulls":unmapped_pulls},
        "reconciliation_complete":false,"publication_authorized":false}),
    )
}

#[cfg(test)]
#[path = "restore/tests.rs"]
mod tests;
