use super::publish::journal::{Journal, Record};
use super::*;
use treeharbor_metadata::github::{
    RestoreContext, RestoreEffectKind, RestorePullEvidence, restore_digest,
};

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
    #[arg(long)]
    operation: Uuid,
    #[arg(long,value_parser=clap::value_parser!(u64).range(1..))]
    number: u64,
    #[arg(long)]
    node_id: String,
    /// Existing private directory; reuse it to recover a lost acknowledgment.
    #[arg(long)]
    state: PathBuf,
}
async fn context(o: &Options, c: &Credential) -> Result<RestoreContext> {
    let value=command(&o.socket,c,json!({"action":"restore_context","epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256})).await?;
    let ctx: RestoreContext =
        serde_json::from_value(value).map_err(|_| Error::RestoreAssessment)?;
    if ctx.version != 2
        || ctx.tenant != c.scope.tenant()
        || ctx.repository != c.scope.repository()
        || ctx.epoch != o.epoch
        || ctx.binding != o.binding
        || ctx.manifest_sha256 != o.manifest_sha256
        || ctx.reconciliation_complete
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(ctx)
}
async fn record(
    o: &Options,
    c: &Credential,
    j: &Journal,
    r: &Record<RestorePullEvidence>,
) -> Result<bool> {
    let value=command(&o.socket,c,json!({"action":"restore_pull_record","key":r.key,"epoch":o.epoch,"specification":r.report})).await?;
    if value["version"] != 1
        || value["key"] != r.key
        || value["specification"] != json!(r.report)
        || value["tenant"] != c.scope.tenant()
        || value["repository"] != json!(c.scope.repository())
        || value["epoch"] != o.epoch
        || value["publication_authorized"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    j.ack(0, &value)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|_| Error::Http)?
    );
    Ok(r.report.report.token_revoked)
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let c = Credential::load(&o.credential_file)?;
    let identity = treeharbor_metadata::github::PullIdentity {
        number: o.number,
        node_id: o.node_id.clone(),
    };
    let intent = json!({"version":1,"kind":"restore_pull","tenant":c.scope.tenant(),"repository":c.scope.repository(),
        "epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"operation":o.operation,"identity":identity});
    let journal = Journal::open(&o.state, &intent)?;
    // Retry the original report before reading GitHub or today's local context.
    // The service rechecks current authorization and returns history on replay.
    if let Some(r) = journal.single::<RestorePullEvidence>()? {
        if r.report.binding != o.binding
            || r.report.operation != o.operation
            || r.report.identity != identity
            || r.report.manifest_sha256 != o.manifest_sha256
        {
            return Err(Error::RestoreAssessment);
        }
        return record(&o, &c, &journal, &r).await;
    }
    let before = context(&o, &c).await?;
    let effect = before
        .effects
        .iter()
        .find(|e| e.kind == RestoreEffectKind::Pull && e.id == o.operation)
        .ok_or(Error::RestoreAssessment)?;
    if effect.pull_number.is_some_and(|n| n as u64 != o.number)
        || effect.pull_node.as_ref().is_some_and(|n| *n != o.node_id)
    {
        return Err(Error::RestoreAssessment);
    }
    let target: treeharbor_github::PullTarget = serde_json::from_value(json!(
        effect.target.as_ref().ok_or(Error::RestoreAssessment)?
    ))
    .map_err(|_| Error::RestoreAssessment)?;
    let selected: treeharbor_github::PullIdentity =
        serde_json::from_value(json!(identity)).map_err(|_| Error::RestoreAssessment)?;
    let (app, config) = load(&o.config)?;
    matches_config(&config, &before.specification)?;
    let observed = Connection::new(app)?
        .observe_restore_pull(&config.repository, &target, &selected)
        .await?;
    if observed.binding != config.repository || context(&o, &c).await? != before {
        return Err(Error::RestoreAssessment);
    }
    let report =
        serde_json::from_value(json!(observed.report)).map_err(|_| Error::RestoreAssessment)?;
    let evidence = RestorePullEvidence {
        version: 1,
        binding: o.binding,
        manifest_sha256: o.manifest_sha256.clone(),
        context_sha256: restore_digest(&before).map_err(|_| Error::RestoreAssessment)?,
        operation: o.operation,
        identity,
        report,
    };
    journal.append(&Record::new(evidence, Some(observed.token_expires_at)))?;
    let (_, r) = journal
        .pending::<RestorePullEvidence>()?
        .ok_or(Error::RestoreAssessment)?;
    record(&o, &c, &journal, &r).await
}
