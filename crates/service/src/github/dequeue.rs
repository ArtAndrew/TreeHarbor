use super::publish::{
    journal::{Journal, Record},
    now, uuid,
};
use super::*;
use treeharbor_github_contract::{dequeue::DequeueObservation, queue::QueueTarget};

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
    pub queue_state: Option<PathBuf>,
}
fn identity(c: &Value, r: &Request, credential: &Credential) -> Result<(Value, QueueTarget)> {
    if c["version"] != 1
        || uuid(&c["dequeue"])? != r.id
        || c["epoch"] != r.epoch
        || c["scope"] != "pull_request"
        || c["merge_verified"] != false
        || c["expires_at_unix_ms"].as_i64().is_none()
        || c["enqueue_settled"].as_bool().is_none()
    {
        return Err(Error::PublicationState);
    }
    uuid(&c["queue"])?;
    uuid(&c["binding"])?;
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
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository().to_string(),
        "dequeue":r.id.to_string(),"queue":c["queue"],"scope":"pull_request","epoch":r.epoch,
        "binding":c["binding"],"target":target,"expires_at_unix_ms":c["expires_at_unix_ms"],"specification":spec}),
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
        v["dequeue"] = json!(self.r.id.to_string());
        v["epoch"] = json!(self.r.epoch);
        command(&self.r.socket, self.credential, v).await
    }
    fn matches(&self, c: &Value, attempt: Option<Uuid>) -> Result<()> {
        for f in [
            "version",
            "dequeue",
            "queue",
            "scope",
            "epoch",
            "binding",
            "target",
            "expires_at_unix_ms",
        ] {
            if c[f] != self.intent[f] {
                return Err(Error::PublicationState);
            }
        }
        if c["merge_verified"] != false || attempt.is_some_and(|a| c["attempt"] != a.to_string()) {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn context(&self) -> Result<Value> {
        let c = self.command(json!({"action":"dequeue_context"})).await?;
        if identity(&c, self.r, self.credential)?.0 != self.intent {
            return Err(Error::PublicationState);
        }
        Ok(c)
    }
    async fn eligible(&self, attempt: Uuid) -> Result<()> {
        let c = self
            .command(json!({"action":"dequeue_dispatch_context","attempt":attempt.to_string()}))
            .await?;
        self.matches(&c, Some(attempt))?;
        if c["enqueue_settled"] != true
            || c["unexpired"] != true
            || c["mutation_not_sent"] != false
            || !c["retained_receipt"].is_null()
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
        let Some((index, record)) = j.pending::<DequeueObservation>()? else {
            return Ok(false);
        };
        let attempt = uuid(&c["attempt"])?;
        record
            .report
            .validate(&self.target, self.r.id)
            .map_err(|_| Error::PublicationState)?;
        let v=self.command(json!({"action":"dequeue_report","key":record.key,"attempt":attempt.to_string(),"report":record.report})).await?;
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
        json!({"action":"dequeue_context","dequeue":r.id.to_string(),"epoch":r.epoch}),
    )
    .await?;
    let (intent, target) = identity(&c, &r, &credential)?;
    let s = Session {
        r: &r,
        credential: &credential,
        intent,
        target,
    };
    let j = Journal::open(&r.state, &s.intent)?;
    // Flush the private receipt before requiring any App key or provider access.
    if s.pending(&j, &c).await? {
        return print(&s.context().await?);
    }
    c = s.context().await?;
    if c["enqueue_settled"] != true {
        if let Some(queue_state) = &r.queue_state {
            queue::collect(
                queue::Request {
                    config: r.config.clone(),
                    socket: r.socket.clone(),
                    credential_file: r.credential_file.clone(),
                    id: uuid(&c["queue"])?,
                    epoch: r.epoch,
                    state: queue_state.clone(),
                    check: None,
                    attempt_check: None,
                    definition_attempt: None,
                },
                false,
            )
            .await?;
            c = s.context().await?;
        }
        if c["enqueue_settled"] != true {
            return print(&c);
        }
    }
    let spec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let connection = Connection::new(app)?;
    let mut dispatch = false;
    let attempt = if c["attempt"].is_null() {
        if c["unexpired"] != true
            || c["mutation_not_sent"] != false
            || !c["retained_receipt"].is_null()
        {
            return Err(Error::PublicationState);
        }
        let attempt = j.attempt()?;
        let v=s.command(json!({"action":"dequeue_begin","key":format!("github-dequeue-begin-{}",r.id),"attempt":attempt.to_string()})).await?;
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
            .dequeue_pull_attempt(&config.repository, &s.target, r.id, || async {
                s.eligible(attempt)
                    .await
                    .map_err(|_| treeharbor_github::Error::PublicationStopped)
            })
            .await
    } else {
        connection
            .observe_dequeue(&config.repository, &s.target, r.id)
            .await
            .map_err(|error| treeharbor_github::MutationFailure {
                error,
                mutation_not_sent: false,
            })
    };
    let (report, expiry) = match result {
        Ok(v) => {
            if v.binding != config.repository {
                return Err(Error::PublicationState);
            }
            (v.observation, Some(v.token_expires_at))
        }
        Err(e) => {
            eprintln!("{}", e.error);
            (
                DequeueObservation {
                    version: 1,
                    started_at_unix_ms: now()?,
                    finished_at_unix_ms: now()?,
                    before: None,
                    receipt: None,
                    after: None,
                    mutation_not_sent: e.mutation_not_sent,
                    token_revoked: false,
                },
                None,
            )
        }
    };
    report
        .validate(&s.target, r.id)
        .map_err(|_| Error::PublicationState)?;
    j.append(&Record::new(report, expiry))?;
    c = s.context().await?;
    s.pending(&j, &c).await?;
    print(&s.context().await?)
}
