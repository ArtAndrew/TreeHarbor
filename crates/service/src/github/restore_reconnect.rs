use super::publish::journal::{Journal, Record};
use super::*;
use treeharbor_metadata::github::{
    RestoreActivateSpec, RestoreReconnectProbe, RestoreReconnectSpec, restore_digest,
};

#[derive(clap::Args)]
pub struct Options {
    #[command(subcommand)]
    action: Reconnect,
}
#[derive(clap::Args)]
struct Endpoint {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
}
#[derive(clap::Subcommand)]
enum Reconnect {
    /// Owner's time-limited plan; external fencing remains an operator assertion.
    Plan {
        #[command(flatten)]
        endpoint: Endpoint,
        #[arg(long)]
        key: String,
        /// Private JSON file containing the exact plan and explicit acknowledgments.
        #[arg(long)]
        specification: PathBuf,
    },
    /// Assigned bridge's read-only probe; retain the private journal for retries.
    Probe {
        #[command(flatten)]
        endpoint: Endpoint,
        #[arg(long)]
        plan_key: String,
        #[arg(long)]
        config: PathBuf,
        #[arg(long)]
        state: PathBuf,
    },
    /// Owner's atomic re-review and fresh binding. Does not replay old operations.
    Activate {
        #[command(flatten)]
        endpoint: Endpoint,
        #[arg(long)]
        key: String,
        #[arg(long)]
        specification: PathBuf,
    },
    Cancel {
        #[command(flatten)]
        endpoint: Endpoint,
        #[arg(long)]
        key: String,
        #[arg(long)]
        plan_key: String,
    },
    Status {
        #[command(flatten)]
        endpoint: Endpoint,
        #[arg(long)]
        plan_key: String,
    },
}
fn now() -> Result<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.as_secs())
        .map_err(|_| Error::RestoreAssessment)
}
async fn context(e: &Endpoint, c: &Credential, key: &str) -> Result<Value> {
    let v = command(
        &e.socket,
        c,
        json!({"action":"restore_reconnect_context","epoch":e.epoch,"plan_key":key}),
    )
    .await?;
    let spec: RestoreReconnectSpec =
        serde_json::from_value(v["specification"].clone()).map_err(|_| Error::RestoreAssessment)?;
    spec.validate().map_err(|_| Error::RestoreAssessment)?;
    if v["version"] != 1
        || v["tenant"] != c.scope.tenant()
        || v["repository"] != json!(c.scope.repository())
        || v["epoch"] != e.epoch
        || v["key"] != key
        || v["publication_authorized"] != false
        || v["external_fencing_verified"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    Ok(v)
}
async fn probe(e: Endpoint, key: String, config: PathBuf, state: PathBuf) -> Result<Value> {
    let c = Credential::load(&e.credential_file)?;
    let j = Journal::open(
        &state,
        &json!({"version":1,"kind":"restore_reconnect_probe","tenant":c.scope.tenant(),"repository":c.scope.repository(),"epoch":e.epoch,"plan_key":key}),
    )?;
    let record = match j.single::<RestoreReconnectProbe>()? {
        Some(r) => r,
        None => {
            let before = context(&e, &c, &key).await?;
            let after = before["observe_after_unix"]
                .as_u64()
                .ok_or(Error::RestoreAssessment)?;
            let current = now()?;
            if after > current {
                if after - current > 5 {
                    return Err(Error::RestoreAssessment);
                }
                tokio::time::sleep(Duration::from_secs(after - current)).await;
            }
            let spec: RestoreReconnectSpec =
                serde_json::from_value(before["specification"].clone())
                    .map_err(|_| Error::RestoreAssessment)?;
            let (app, cfg) = load(&config)?;
            matches_config(&cfg, &spec.binding)?;
            let started_at_unix = now()?;
            let observed = Connection::new(app)?.probe(&cfg.repository).await?;
            let finished_at_unix = now()?;
            if context(&e, &c, &key).await? != before {
                return Err(Error::RestoreAssessment);
            }
            let observation =
                serde_json::from_value(json!(observed)).map_err(|_| Error::RestoreAssessment)?;
            let report = RestoreReconnectProbe {
                version: 1,
                plan_key: key.clone(),
                plan_sha256: restore_digest(&before).map_err(|_| Error::RestoreAssessment)?,
                started_at_unix,
                finished_at_unix,
                observation,
            };
            j.append(&Record::new(report, None))?;
            j.single()?.ok_or(Error::RestoreAssessment)?
        }
    };
    if record.report.plan_key != key {
        return Err(Error::RestoreAssessment);
    }
    // Same journal/key recovers a committed receipt before any config or App IO.
    let v=command(&e.socket,&c,json!({"action":"restore_reconnect_probe","epoch":e.epoch,"key":record.key,"specification":record.report})).await?;
    if v["version"] != 1
        || v["tenant"] != c.scope.tenant()
        || v["repository"] != json!(c.scope.repository())
        || v["epoch"] != e.epoch
        || v["key"] != record.key
        || v["specification"] != json!(record.report)
        || v["publication_authorized"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    j.ack(0, &v)?;
    Ok(v)
}
async fn owner(e: Endpoint, request: Value) -> Result<Value> {
    let c = Credential::load(&e.credential_file)?;
    command(&e.socket, &c, request).await
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let v = match o.action {
        Reconnect::Plan {
            endpoint,
            key,
            specification,
        } => {
            let spec: RestoreReconnectSpec =
                serde_json::from_slice(&files::read_private(&specification, 32 * 1024)?)
                    .map_err(|_| Error::RestoreAssessment)?;
            spec.validate().map_err(|_| Error::RestoreAssessment)?;
            let req = json!({"action":"restore_reconnect_plan","epoch":endpoint.epoch,"key":key,"specification":spec});
            owner(endpoint, req).await?
        }
        Reconnect::Activate {
            endpoint,
            key,
            specification,
        } => {
            let spec: RestoreActivateSpec =
                serde_json::from_slice(&files::read_private(&specification, 48 * 1024)?)
                    .map_err(|_| Error::RestoreAssessment)?;
            let req = json!({"action":"restore_reconnect_activate","epoch":endpoint.epoch,"key":key,"specification":spec});
            owner(endpoint, req).await?
        }
        Reconnect::Probe {
            endpoint,
            plan_key,
            config,
            state,
        } => probe(endpoint, plan_key, config, state).await?,
        Reconnect::Cancel {
            endpoint,
            key,
            plan_key,
        } => {
            let req = json!({"action":"restore_reconnect_cancel","epoch":endpoint.epoch,"key":key,"plan_key":plan_key});
            owner(endpoint, req).await?
        }
        Reconnect::Status { endpoint, plan_key } => {
            let req = json!({"action":"restore_reconnect_status","epoch":endpoint.epoch,"plan_key":plan_key});
            owner(endpoint, req).await?
        }
    };
    println!(
        "{}",
        serde_json::to_string_pretty(&v).map_err(|_| Error::Http)?
    );
    Ok(true)
}
