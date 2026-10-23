use super::publish::{
    journal::{Journal, Record},
    uuid,
};
use super::*;
use treeharbor_github_contract::{InspectionReport, InspectionTarget};

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
}
fn identity(r: &Request, credential: &Credential, c: &Value) -> Result<(Value, InspectionTarget)> {
    if c["version"] != 1
        || uuid(&c["inspection"])? != r.id
        || c["epoch"] != r.epoch
        || c["merge_authorized"] != false
        || c["host_policy_verified"] != false
        || c["legacy_branch_protection_included"] != false
        || c["expires_at_unix_ms"].as_i64().is_none()
    {
        return Err(Error::PublicationState);
    }
    uuid(&c["pull"])?;
    uuid(&c["projection"])?;
    uuid(&c["binding"])?;
    let t: InspectionTarget =
        serde_json::from_value(c["target"].clone()).map_err(|_| Error::PublicationState)?;
    t.validate().map_err(|_| Error::PublicationState)?;
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    spec.validate().map_err(|_| Error::PublicationState)?;
    if spec.upstream.repository_id != t.repository_id {
        return Err(Error::PublicationState);
    }
    Ok((
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"inspection":r.id.to_string(),"epoch":r.epoch,
        "pull":c["pull"],"projection":c["projection"],"binding":c["binding"],"target":t,"specification":spec,"expires_at_unix_ms":c["expires_at_unix_ms"]}),
        t,
    ))
}
struct Session<'a> {
    r: &'a Request,
    credential: &'a Credential,
    intent: Value,
    target: InspectionTarget,
}
impl Session<'_> {
    async fn command(&self, mut v: Value) -> Result<Value> {
        v["inspection"] = json!(self.r.id.to_string());
        v["epoch"] = json!(self.r.epoch);
        command(&self.r.socket, self.credential, v).await
    }
    async fn context(&self, collect: bool) -> Result<Value> {
        let v=self.command(json!({"action":if collect{"inspection_collect_context"}else{"inspection_context"}})).await?;
        if identity(self.r, self.credential, &v)?.0 != self.intent {
            return Err(Error::PublicationState);
        }
        if collect
            && (v["state"] != "requested"
                || v["is_current_version"] != true
                || v["unexpired"] != true)
        {
            return Err(Error::PublicationState);
        }
        Ok(v)
    }
    async fn pending(&self, j: &Journal) -> Result<()> {
        if let Some((i, record)) = j.pending::<InspectionReport>()? {
            record
                .report
                .validate(&self.target)
                .map_err(|_| Error::PublicationState)?;
            let result = self
                .command(
                    json!({"action":"inspection_report","key":record.key,"report":record.report}),
                )
                .await?;
            if result["inspection"] != self.r.id.to_string()
                || result["state"] != "observed"
                || result["target"] != json!(self.target)
                || result["report"] != json!(record.report)
                || result["merge_authorized"] != false
            {
                return Err(Error::PublicationState);
            }
            j.ack(i, &result)?;
        }
        Ok(())
    }
}
fn terminal(v: &Value) -> bool {
    matches!(v["state"].as_str(), Some("observed" | "cancelled"))
}
fn print(v: &Value) -> Result<bool> {
    println!(
        "{}",
        serde_json::to_string_pretty(v).map_err(|_| Error::Http)?
    );
    Ok(v["state"] == "observed")
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let credential = Credential::load(&r.credential_file)?;
    let c = command(
        &r.socket,
        &credential,
        json!({"action":"inspection_context","epoch":r.epoch,"inspection":r.id.to_string()}),
    )
    .await?;
    let (intent, target) = identity(&r, &credential, &c)?;
    let s = Session {
        r: &r,
        credential: &credential,
        intent,
        target,
    };
    if terminal(&c) {
        return print(&c);
    }
    let journal = Journal::open(&r.state, &s.intent)?;
    s.pending(&journal).await?;
    let c = s.context(false).await?;
    if terminal(&c) {
        return print(&c);
    }
    s.context(true).await?;
    let spec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let connection = Connection::new(app)?;
    let observed = connection
        .inspect_pull(&config.repository, &s.target, || async {
            s.context(true)
                .await
                .map(|_| ())
                .map_err(|_| treeharbor_github::Error::PublicationStopped)
        })
        .await?;
    if observed.binding != config.repository {
        return Err(Error::PublicationState);
    }
    observed
        .report
        .validate(&s.target)
        .map_err(|_| Error::PublicationState)?;
    journal.append(&Record::new(
        observed.report,
        Some(observed.token_expires_at),
    ))?;
    let c = s.context(false).await?;
    if !terminal(&c) {
        s.pending(&journal).await?;
    }
    print(&s.context(false).await?)
}
