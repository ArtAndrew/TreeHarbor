use super::*;
use treeharbor_metadata::github::RestoreTargetSpec;
#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long)]
    key: String,
    /// Private JSON file; a wrong immutable target requires a fresh staging restore.
    #[arg(long)]
    specification: PathBuf,
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let c = Credential::load(&o.credential_file)?;
    let spec: RestoreTargetSpec =
        serde_json::from_slice(&files::read_private(&o.specification, 32 * 1024)?)
            .map_err(|_| Error::RestoreAssessment)?;
    spec.validate().map_err(|_| Error::RestoreAssessment)?;
    let v=command(&o.socket,&c,json!({"action":"restore_target_register","epoch":o.epoch,"key":o.key,"specification":spec})).await?;
    if v["version"] != 1
        || v["tenant"] != c.scope.tenant()
        || v["repository"] != json!(c.scope.repository())
        || v["epoch"] != o.epoch
        || v["key"] != o.key
        || v["specification"] != json!(spec)
        || v["outcome"] != "prospective_recovery_target_registered"
        || v["binding_state"] != "revoked"
        || v["historical_connection_asserted"] != false
        || v["remote_history_empty_asserted"] != false
        || v["publication_authorized"] != false
    {
        return Err(Error::RestoreAssessment);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&v).map_err(|_| Error::Http)?
    );
    Ok(true)
}
