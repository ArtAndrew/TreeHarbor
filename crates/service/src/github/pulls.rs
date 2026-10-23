use super::publish::{
    journal::{Journal, Record},
    native, now, uuid,
};
use super::*;
use treeharbor_github::PullTarget;
use treeharbor_metadata::github::PullReport;

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
}
struct Session<'a> {
    r: &'a Request,
    credential: &'a Credential,
    identity: Value,
    target: PullTarget,
}
fn terminal(c: &Value) -> bool {
    matches!(c["state"].as_str(), Some("observed" | "cancelled"))
}
fn identity(c: &Value, r: &Request, credential: &Credential) -> Result<(Value, PullTarget)> {
    if c["version"] != 1
        || uuid(&c["pull"])? != r.id
        || c["epoch"] != r.epoch
        || c["merge_authorized"] != false
    {
        return Err(Error::PublicationState);
    }
    uuid(&c["publication"])?;
    uuid(&c["projection"])?;
    uuid(&c["binding"])?;
    native(&c["native_revision"])?;
    if !c["previous"].is_null() {
        uuid(&c["previous"])?;
    }
    let target: PullTarget =
        serde_json::from_value(c["target"].clone()).map_err(|_| Error::PublicationState)?;
    target.validate()?;
    if target.operation != r.id || c["expires_at_unix_ms"].as_i64().is_none() {
        return Err(Error::PublicationState);
    }
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    spec.validate().map_err(|_| Error::PublicationState)?;
    Ok((
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"pull":r.id.to_string(),
        "epoch":r.epoch,"publication":c["publication"],"projection":c["projection"],"binding":c["binding"],"native_revision":c["native_revision"],
        "target":target,"previous":c["previous"],"specification":spec,"expires_at_unix_ms":c["expires_at_unix_ms"]}),
        target,
    ))
}
impl Session<'_> {
    async fn command(&self, mut request: Value) -> Result<Value> {
        request["epoch"] = json!(self.r.epoch);
        request["pull"] = json!(self.r.id.to_string());
        command(&self.r.socket, self.credential, request).await
    }
    fn matches(&self, c: &Value, attempt: Option<Uuid>) -> Result<()> {
        for field in [
            "version",
            "pull",
            "epoch",
            "publication",
            "projection",
            "binding",
            "native_revision",
            "target",
            "previous",
            "expires_at_unix_ms",
        ] {
            if c[field] != self.identity[field] {
                return Err(Error::PublicationState);
            }
        }
        if c["merge_authorized"] != false
            || attempt.is_some_and(|id| c["attempt"] != id.to_string())
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn context(&self) -> Result<Value> {
        let c = self.command(json!({"action":"pull_context"})).await?;
        if identity(&c, self.r, self.credential)?.0 != self.identity {
            return Err(Error::PublicationState);
        }
        Ok(c)
    }
    async fn eligible(&self, attempt: Uuid) -> Result<()> {
        let c = self
            .command(json!({"action":"pull_dispatch_context","attempt":attempt.to_string()}))
            .await?;
        self.matches(&c, Some(attempt))?;
        if c["state"] != "attempting"
            || c["cancel_requested"] != false
            || c["unexpired"] != true
            || c["expires_at_unix_ms"]
                .as_i64()
                .ok_or(Error::PublicationState)?
                <= now()?
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn pending(&self, j: &Journal, c: &Value) -> Result<()> {
        if let Some((index, record)) = j.pending::<PullReport>()? {
            self.matches(c, Some(uuid(&json!(record.report.attempt))?))?;
            if record.report.binding != self.identity["binding"]
                || record.report.git_revision != self.target.git_revision
            {
                return Err(Error::PublicationState);
            }
            if c["observation"]["observed_at_unix_ms"]
                .as_i64()
                .is_some_and(|time| time > record.report.observed_at_unix_ms)
            {
                j.ack(index, &json!({"superseded_by":c["observation"]}))?;
                return Ok(());
            }
            let result = self
                .command(json!({"action":"pull_report","key":record.key,"report":record.report}))
                .await?;
            self.matches(&result, Some(uuid(&json!(record.report.attempt))?))?;
            j.ack(index, &result)?;
        }
        Ok(())
    }
}
fn print(c: &Value) -> Result<bool> {
    println!(
        "{}",
        serde_json::to_string_pretty(c).map_err(|_| Error::Http)?
    );
    Ok(c["state"] == "observed")
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let credential = Credential::load(&r.credential_file)?;
    let mut c = command(
        &r.socket,
        &credential,
        json!({"action":"pull_context","epoch":r.epoch,"pull":r.id.to_string()}),
    )
    .await?;
    let (identity, target) = identity(&c, &r, &credential)?;
    let s = Session {
        r: &r,
        credential: &credential,
        identity,
        target,
    };
    if terminal(&c) {
        return print(&c);
    }
    let journal = Journal::open(&r.state, &s.identity)?;
    s.pending(&journal, &c).await?;
    c = s.context().await?;
    if terminal(&c) {
        return print(&c);
    }
    let spec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let connection = Connection::new(app)?;
    let mut dispatch = false;
    let attempt = if c["state"] == "requested" {
        if c["unexpired"] != true || c["cancel_requested"] != false || !c["attempt"].is_null() {
            return Err(Error::PublicationState);
        }
        let attempt = journal.attempt()?;
        let begin=s.command(json!({"action":"pull_begin","key":format!("github-pull-begin-{}",r.id),"attempt":attempt.to_string()})).await?;
        s.matches(&begin["receipt"], Some(attempt))?;
        if begin["receipt"]["state"] != "attempting" {
            return Err(Error::PublicationState);
        }
        dispatch = begin["dispatch_granted"]
            .as_bool()
            .ok_or(Error::PublicationState)?;
        attempt
    } else if matches!(c["state"].as_str(), Some("attempting" | "uncertain")) {
        uuid(&c["attempt"])?
    } else {
        return Err(Error::PublicationState);
    };
    let result = if dispatch {
        connection
            .publish_pull(&config.repository, &s.target, || async {
                s.eligible(attempt)
                    .await
                    .map_err(|_| treeharbor_github::Error::PublicationStopped)
            })
            .await
    } else {
        connection.observe_pull(&config.repository, &s.target).await
    };
    let (pull, revoked, expiry) = match result {
        Ok(result) => {
            if result.version != 1
                || result.operation != r.id
                || result.binding != config.repository
            {
                return Err(Error::PublicationState);
            }
            (
                result.pull,
                result.token_revoked && (dispatch || c["observation"]["token_revoked"] == true),
                Some(result.token_expires_at),
            )
        }
        Err(error) => {
            eprintln!("{error}");
            (None, false, None)
        }
    };
    let pull = serde_json::from_value(json!(pull)).map_err(|_| Error::PublicationState)?;
    let report = PullReport {
        attempt: attempt.to_string(),
        binding: uuid(&s.identity["binding"])?.to_string(),
        git_revision: s.target.git_revision.clone(),
        pull,
        observed_at_unix_ms: now()?,
        token_revoked: revoked,
    };
    journal.append(&Record::new(report, expiry))?;
    c = s.context().await?;
    if !terminal(&c) {
        s.pending(&journal, &c).await?;
    }
    print(&s.context().await?)
}
