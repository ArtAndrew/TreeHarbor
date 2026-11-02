use super::*;
use treeharbor_metadata::github::{RestoreReviewSpec, restore_digest};

#[derive(clap::Args)]
pub struct Options {
    #[arg(long)]
    socket: PathBuf,
    #[arg(long)]
    credential_file: PathBuf,
    #[arg(long,value_parser=clap::value_parser!(i64).range(1..))]
    epoch: i64,
    #[arg(long)]
    manifest_sha256: String,
    /// Repeat for one sealed capture per old binding. Omitted bindings are reported.
    #[arg(long = "inventory")]
    inventories: Vec<String>,
}
pub(super) async fn run(o: Options) -> Result<bool> {
    let c = Credential::load(&o.credential_file)?;
    let spec = RestoreReviewSpec {
        version: 1,
        manifest_sha256: o.manifest_sha256.clone(),
        inventory_keys: o.inventories,
    };
    let value = command(
        &o.socket,
        &c,
        json!({"action":"restore_review","epoch":o.epoch,"specification":spec}),
    )
    .await?;
    let a = &value["assessment"];
    if value["version"] != 1
        || a["version"] != 1
        || a["tenant"] != c.scope.tenant()
        || a["repository"] != json!(c.scope.repository())
        || a["epoch"] != o.epoch
        || a["manifest_sha256"] != o.manifest_sha256
        || a["publication_authorized"] != false
        || a["reconciliation_complete"] != false
        || value["assessment_sha256"] != restore_digest(a).map_err(|_| Error::RestoreAssessment)?
    {
        return Err(Error::RestoreAssessment);
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&value).map_err(|_| Error::Http)?
    );
    // Successful inspection is not successful recovery. The explicit fields
    // describe unresolved work; this command makes no external request.
    Ok(true)
}
