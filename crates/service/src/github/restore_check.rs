use super::publish::journal::{Journal, Record};
use super::*;
use treeharbor_github_contract::restore_check::CheckIdentity;
use treeharbor_metadata::github::{RestoreCheckContext, RestoreCheckEvidence, restore_digest};
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
    check_id: u64,
    #[arg(long)]
    node_id: String,
    /// Existing private directory; reuse it to recover a lost acknowledgment.
    #[arg(long)]
    state: PathBuf,
}
impl Options {
    fn identity(&self) -> CheckIdentity {
        CheckIdentity {
            id: self.check_id,
            node_id: self.node_id.clone(),
        }
    }
}
async fn context(o: &Options, c: &Credential) -> Result<RestoreCheckContext> {
    let value=command(&o.socket,c,json!({"action":"restore_check_context","epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"operation":o.operation,"identity":o.identity()})).await?;
    let bundle: RestoreCheckContext =
        serde_json::from_value(value).map_err(|_| Error::RestoreAssessment)?;
    let ctx = &bundle.context;
    if bundle.version != 1
        || ctx.version != 2
        || ctx.tenant != c.scope.tenant()
        || ctx.repository != c.scope.repository()
        || ctx.epoch != o.epoch
        || ctx.binding != o.binding
        || ctx.manifest_sha256 != o.manifest_sha256
        || ctx.reconciliation_complete
        || bundle.publication_authorized
        || bundle.operation != o.operation
        || bundle.identity != o.identity()
        || restore_digest(ctx).map_err(|_| Error::RestoreAssessment)? != bundle.context_sha256
        || bundle.target.publication != o.operation
        || bundle.target.epoch != o.epoch
        || !ctx.effects.iter().any(|e| {
            e.kind == treeharbor_metadata::github::RestoreEffectKind::Check && e.id == o.operation
        })
    {
        return Err(Error::RestoreAssessment);
    }
    bundle
        .target
        .validate()
        .map_err(|_| Error::RestoreAssessment)?;
    Ok(bundle)
}
async fn record(
    o: &Options,
    c: &Credential,
    j: &Journal,
    r: &Record<RestoreCheckEvidence>,
) -> Result<bool> {
    let value=command(&o.socket,c,json!({"action":"restore_check_record","key":r.key,"epoch":o.epoch,"specification":r.report})).await?;
    if value["version"] != 1
        || value["key"] != r.key
        || value["specification"] != json!(r.report)
        || value["tenant"] != c.scope.tenant()
        || value["repository"] != json!(c.scope.repository())
        || value["epoch"] != o.epoch
        || value["publication_authorized"] != false
        || value["validation_authorized"] != false
        || value["reconciliation_complete"] != false
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
    let identity = o.identity();
    identity.validate().map_err(|_| Error::RestoreAssessment)?;
    let intent = json!({"version":1,"kind":"restore_check","tenant":c.scope.tenant(),"repository":c.scope.repository(),"epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"operation":o.operation,"identity":identity});
    let j = Journal::open(&o.state, &intent)?;
    if let Some(r) = j.single::<RestoreCheckEvidence>()? {
        if r.report.binding != o.binding
            || r.report.manifest_sha256 != o.manifest_sha256
            || r.report.operation != o.operation
            || r.report.identity != identity
        {
            return Err(Error::RestoreAssessment);
        }
        return record(&o, &c, &j, &r).await;
    }
    let before = context(&o, &c).await?;
    let (app, config) = load(&o.config)?;
    matches_config(&config, &before.context.specification)?;
    let observed = Connection::new(app)?
        .observe_restore_check(&config.repository, &before.target, &identity)
        .await?;
    if observed.binding != config.repository || context(&o, &c).await? != before {
        return Err(Error::RestoreAssessment);
    }
    let spec = RestoreCheckEvidence {
        version: 1,
        binding: o.binding,
        manifest_sha256: o.manifest_sha256.clone(),
        context_sha256: before.context_sha256,
        operation: o.operation,
        identity,
        report: observed.report,
    };
    j.append(&Record::new(spec, Some(observed.token_expires_at)))?;
    let (_, r) = j
        .pending::<RestoreCheckEvidence>()?
        .ok_or(Error::RestoreAssessment)?;
    record(&o, &c, &j, &r).await
}
