use super::publish::journal::{Journal, Record};
use super::*;
use treeharbor_metadata::github::{
    RestoreQueueContext, RestoreQueueEvidence, RestoreQueueSubject, restore_digest,
};

#[derive(Clone, clap::ValueEnum)]
enum Kind {
    Queue,
    Dequeue,
}
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
    #[arg(long, value_enum)]
    kind: Kind,
    #[arg(long)]
    operation: Uuid,
    /// Existing private directory; reuse it to recover a lost acknowledgment.
    #[arg(long)]
    state: PathBuf,
}
impl Options {
    fn subject(&self) -> RestoreQueueSubject {
        match self.kind {
            Kind::Queue => RestoreQueueSubject::Queue {
                operation: self.operation,
            },
            Kind::Dequeue => RestoreQueueSubject::Dequeue {
                operation: self.operation,
            },
        }
    }
}
async fn context(o: &Options, c: &Credential) -> Result<RestoreQueueContext> {
    let value=command(&o.socket,c,json!({"action":"restore_queue_context","epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"subject":o.subject()})).await?;
    let bundle: RestoreQueueContext =
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
        || bundle.subject != o.subject()
        || restore_digest(ctx).map_err(|_| Error::RestoreAssessment)? != bundle.context_sha256
    {
        return Err(Error::RestoreAssessment);
    }
    let selected = match bundle.subject {
        RestoreQueueSubject::Queue { operation } => operation,
        RestoreQueueSubject::Dequeue { operation } => {
            ctx.dequeues
                .iter()
                .find(|d| d.id == operation)
                .ok_or(Error::RestoreAssessment)?
                .queue
        }
    };
    if selected != bundle.queue
        || !ctx.effects.iter().any(|e| {
            e.kind == treeharbor_metadata::github::RestoreEffectKind::Queue && e.id == selected
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
    r: &Record<RestoreQueueEvidence>,
) -> Result<bool> {
    let value=command(&o.socket,c,json!({"action":"restore_queue_record","key":r.key,"epoch":o.epoch,"specification":r.report})).await?;
    if value["version"] != 1
        || value["key"] != r.key
        || value["specification"] != json!(r.report)
        || value["tenant"] != c.scope.tenant()
        || value["repository"] != json!(c.scope.repository())
        || value["epoch"] != o.epoch
        || value["publication_authorized"] != false
        || value["prior_mutation_settled"] != false
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
    let subject = o.subject();
    let intent = json!({"version":1,"kind":"restore_queue","tenant":c.scope.tenant(),"repository":c.scope.repository(),
        "epoch":o.epoch,"binding":o.binding,"manifest_sha256":o.manifest_sha256,"subject":subject});
    let j = Journal::open(&o.state, &intent)?;
    // A durable capture is retried verbatim, before touching App credentials or GitHub.
    if let Some(r) = j.single::<RestoreQueueEvidence>()? {
        if r.report.binding != o.binding
            || r.report.manifest_sha256 != o.manifest_sha256
            || r.report.subject != subject
        {
            return Err(Error::RestoreAssessment);
        }
        return record(&o, &c, &j, &r).await;
    }
    let before = context(&o, &c).await?;
    let (app, config) = load(&o.config)?;
    matches_config(&config, &before.context.specification)?;
    let observed = Connection::new(app)?
        .observe_restore_queue(&config.repository, &before.target)
        .await?;
    if observed.binding != config.repository || context(&o, &c).await? != before {
        return Err(Error::RestoreAssessment);
    }
    let spec = RestoreQueueEvidence {
        version: 1,
        binding: o.binding,
        manifest_sha256: o.manifest_sha256.clone(),
        context_sha256: before.context_sha256,
        subject,
        report: observed.report,
    };
    j.append(&Record::new(spec, Some(observed.token_expires_at)))?;
    let (_, r) = j
        .pending::<RestoreQueueEvidence>()?
        .ok_or(Error::RestoreAssessment)?;
    record(&o, &c, &j, &r).await
}
