use super::publish::{
    journal::{Journal, Record},
    now, uuid,
};
use super::*;
use treeharbor_github_contract::queue::{QueueAsyncResult, QueueObservation, QueueTarget};
pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
}
fn identity(c: &Value, r: &Request, credential: &Credential) -> Result<(Value, QueueTarget)> {
    if c["version"] != 1
        || uuid(&c["queue"])? != r.id
        || c["epoch"] != r.epoch
        || c["merge_verified"] != false
        || c["host_policy_verified"] != false
        || c["expires_at_unix_ms"].as_i64().is_none()
    {
        return Err(Error::PublicationState);
    }
    for field in ["inspection", "pull", "projection", "binding"] {
        uuid(&c[field])?;
    }
    let target: QueueTarget =
        serde_json::from_value(c["target"].clone()).map_err(|_| Error::PublicationState)?;
    target.validate().map_err(|_| Error::PublicationState)?;
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    spec.validate().map_err(|_| Error::PublicationState)?;
    if spec.upstream.repository_id != target.pull.repository_id {
        return Err(Error::PublicationState);
    }
    Ok((
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),"queue":r.id.to_string(),"epoch":r.epoch,
        "inspection":c["inspection"],"pull":c["pull"],"projection":c["projection"],"binding":c["binding"],"target":target,
        "expires_at_unix_ms":c["expires_at_unix_ms"],"specification":spec}),
        target,
    ))
}
struct Session<'a> {
    r: &'a Request,
    credential: &'a Credential,
    intent: Value,
    target: QueueTarget,
}
impl Session<'_> {
    async fn command(&self, mut v: Value) -> Result<Value> {
        v["queue"] = json!(self.r.id.to_string());
        v["epoch"] = json!(self.r.epoch);
        command(&self.r.socket, self.credential, v).await
    }
    fn matches(&self, c: &Value, attempt: Option<Uuid>) -> Result<()> {
        for f in [
            "version",
            "queue",
            "epoch",
            "inspection",
            "pull",
            "projection",
            "binding",
            "target",
            "expires_at_unix_ms",
        ] {
            if c[f] != self.intent[f] {
                return Err(Error::PublicationState);
            }
        }
        if c["merge_verified"] != false
            || c["host_policy_verified"] != false
            || attempt.is_some_and(|a| c["attempt"] != a.to_string())
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn context(&self) -> Result<Value> {
        let c = self.command(json!({"action":"queue_context"})).await?;
        if identity(&c, self.r, self.credential)?.0 != self.intent {
            return Err(Error::PublicationState);
        }
        Ok(c)
    }
    async fn eligible(&self, attempt: Uuid) -> Result<()> {
        let c = self
            .command(json!({"action":"queue_dispatch_context","attempt":attempt.to_string()}))
            .await?;
        self.matches(&c, Some(attempt))?;
        if c["cancel_requested"] != false
            || c["unexpired"] != true
            || c["is_current_version"] != true
            || c["expires_at_unix_ms"]
                .as_i64()
                .ok_or(Error::PublicationState)?
                <= now()?
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn pending(&self, j: &Journal, c: &Value) -> Result<bool> {
        let Some((index, record)) = j.pending::<QueueObservation>()? else {
            return Ok(false);
        };
        let attempt = uuid(&c["attempt"])?;
        record
            .report
            .validate(&self.target)
            .map_err(|_| Error::PublicationState)?;
        let v=self.command(json!({"action":"queue_report","key":record.key,"attempt":attempt.to_string(),"report":record.report})).await?;
        self.matches(&v, Some(attempt))?;
        if v["recorded_observation"] != json!(record.report) {
            return Err(Error::PublicationState);
        }
        j.ack(index, &v)?;
        Ok(true)
    }
}
fn print(c: &Value) -> Result<bool> {
    println!(
        "{}",
        serde_json::to_string_pretty(c).map_err(|_| Error::Http)?
    );
    Ok(c["report_sequence"].as_i64().is_some_and(|n| n > 0))
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let credential = Credential::load(&r.credential_file)?;
    let mut c = command(
        &r.socket,
        &credential,
        json!({"action":"queue_context","queue":r.id.to_string(),"epoch":r.epoch}),
    )
    .await?;
    let (intent, target) = identity(&c, &r, &credential)?;
    let s = Session {
        r: &r,
        credential: &credential,
        intent,
        target,
    };
    if c["state"] == "cancelled" {
        return print(&c);
    }
    let j = Journal::open(&r.state, &s.intent)?;
    if s.pending(&j, &c).await? {
        return print(&s.context().await?);
    }
    c = s.context().await?;
    if c["state"] == "cancelled" {
        return print(&c);
    }
    let spec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let connection = Connection::new(app)?;
    let mut dispatch = false;
    let attempt = if c["attempt"].is_null() {
        if c["state"] != "requested"
            || c["cancel_requested"] != false
            || c["unexpired"] != true
            || c["is_current_version"] != true
        {
            return Err(Error::PublicationState);
        }
        let attempt = j.attempt()?;
        let v=s.command(json!({"action":"queue_begin","key":format!("github-queue-begin-{}",r.id),"attempt":attempt.to_string()})).await?;
        s.matches(&v["receipt"], Some(attempt))?;
        dispatch = v["dispatch_granted"]
            .as_bool()
            .ok_or(Error::PublicationState)?;
        attempt
    } else {
        uuid(&c["attempt"])?
    };
    let result = if dispatch {
        connection
            .enqueue_pull(&config.repository, &s.target, || async {
                s.eligible(attempt)
                    .await
                    .map_err(|_| treeharbor_github::Error::PublicationStopped)
            })
            .await
    } else if let Some(provider) = result_to_recover(&c, &s.target)? {
        connection
            .queue_result(&config.repository, &s.target, &provider)
            .await
    } else {
        connection
            .observe_queue(&config.repository, &s.target)
            .await
    };
    let (report, expiry) = match result {
        Ok(v) => {
            if v.binding != config.repository {
                return Err(Error::PublicationState);
            }
            (v.observation, Some(v.token_expires_at))
        }
        Err(e) => {
            eprintln!("{e}");
            (
                QueueObservation {
                    version: 1,
                    started_at_unix_ms: now()?,
                    finished_at_unix_ms: now()?,
                    snapshot: None,
                    receipt: None,
                    async_result: None,
                    token_revoked: false,
                },
                None,
            )
        }
    };
    report
        .validate(&s.target)
        .map_err(|_| Error::PublicationState)?;
    j.append(&Record::new(report, expiry))?;
    c = s.context().await?;
    s.pending(&j, &c).await?;
    print(&s.context().await?)
}

fn result_to_recover(c: &Value, target: &QueueTarget) -> Result<Option<String>> {
    let retained: Option<QueueAsyncResult> = serde_json::from_value(c["provider_result"].clone())
        .map_err(|_| Error::PublicationState)?;
    if let Some(r) = &retained {
        r.validate(target).map_err(|_| Error::PublicationState)?;
        if c["provider_request"] != r.request_uuid {
            return Err(Error::PublicationState);
        }
    }
    // Alternate pending/unavailable UUID reads with live PR/group observation.
    // A terminal enqueue result is never polled for eventual merge completion.
    if retained.as_ref().is_some_and(QueueAsyncResult::terminal)
        || !c["observation"]["async_result"].is_null()
        || c["provider_request"].is_null()
    {
        return Ok(None);
    }
    Ok(Some(uuid(&c["provider_request"])?.to_string()))
}
