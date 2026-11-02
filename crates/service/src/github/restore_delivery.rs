use super::publish::journal::{Journal, Record};
use super::*;
use treeharbor_metadata::github::{
    RestoreDeliveryContext, RestoreDeliveryEvidence, restore_digest,
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
    recovery: Uuid,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    provider_id: i64,
    /// Existing private directory; reuse it to recover a lost acknowledgment.
    #[arg(long)]
    state: PathBuf,
}
async fn context(o: &Options, c: &Credential) -> Result<RestoreDeliveryContext> {
    let value = command(&o.socket,c,json!({"action":"restore_delivery_context","epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"recovery":o.recovery,"provider_id":o.provider_id})).await?;
    let bundle: RestoreDeliveryContext =
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
        || bundle.recovery != o.recovery
        || bundle.provider_id != o.provider_id
        || bundle.expected.id != o.provider_id as u64
        || restore_digest(ctx).map_err(|_| Error::RestoreAssessment)? != bundle.context_sha256
        || !ctx.redeliveries.iter().any(|a| {
            a.recovery == o.recovery
                && a.provider_id == o.provider_id
                && a.delivery == bundle.expected.guid
        })
        || !ctx
            .recoveries
            .iter()
            .any(|r| r.recovery == o.recovery && r.channel == bundle.channel)
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(bundle)
}
async fn record(
    o: &Options,
    c: &Credential,
    j: &Journal,
    r: &Record<RestoreDeliveryEvidence>,
) -> Result<bool> {
    let value = command(&o.socket,c,json!({"action":"restore_delivery_record","key":r.key,"epoch":o.epoch,"specification":r.report})).await?;
    if value["version"] != 1
        || value["key"] != r.key
        || value["specification"] != json!(r.report)
        || value["tenant"] != c.scope.tenant()
        || value["repository"] != json!(c.scope.repository())
        || value["epoch"] != o.epoch
        || value["publication_authorized"] != false
        || value["prior_dispatch_settled"] != false
        || value["event_processing_verified"] != false
        || value["reconciliation_complete"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    j.ack(0, &value)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|_| Error::Http)?
    );
    Ok(true)
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let c = Credential::load(&o.credential_file)?;
    let intent = json!({"version":1,"kind":"restore_delivery","tenant":c.scope.tenant(),"repository":c.scope.repository(),"epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"recovery":o.recovery,"provider_id":o.provider_id});
    let j = Journal::open(&o.state, &intent)?;
    if let Some(r) = j.single::<RestoreDeliveryEvidence>()? {
        if r.report.binding != o.binding
            || r.report.manifest_sha256 != o.manifest_sha256
            || r.report.recovery != o.recovery
            || r.report.provider_id != o.provider_id
        {
            return Err(Error::RestoreAssessment);
        }
        return record(&o, &c, &j, &r).await;
    }
    let before = context(&o, &c).await?;
    let (app, config) = load(&o.config)?;
    matches_config(&config, &before.context.specification)?;
    let target = treeharbor_github::RecoveryTarget {
        app_id: before.context.specification.app_id,
        binding: config.repository.clone(),
        callback_url: before.callback_url.clone(),
    };
    let report = Connection::new(app)?
        .observe_restore_delivery(&target, &before.expected)
        .await?;
    if context(&o, &c).await? != before {
        return Err(Error::RestoreAssessment);
    }
    let spec = RestoreDeliveryEvidence {
        version: 1,
        binding: o.binding,
        manifest_sha256: o.manifest_sha256.clone(),
        context_sha256: before.context_sha256,
        recovery: o.recovery,
        provider_id: o.provider_id,
        report,
    };
    j.append(&Record::new(spec, None))?;
    let (_, r) = j
        .pending::<RestoreDeliveryEvidence>()?
        .ok_or(Error::RestoreAssessment)?;
    record(&o, &c, &j, &r).await
}
