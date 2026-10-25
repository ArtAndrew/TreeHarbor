use super::publish::{
    journal::{Journal, Record},
    now, uuid,
};
use super::*;
use serde::Serialize;
use treeharbor_github_contract::check_publication::{Outcome, Preflight, Report, Target};

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
    pub recover_only: bool,
}
fn identity(c: &Value, r: &Request, credential: &Credential) -> Result<(Value, Target)> {
    if c["version"] != 1
        || uuid(&c["publication"])? != r.id
        || c["epoch"] != r.epoch
        || c["merge_verified"] != false
        || c["host_policy_verified"] != false
        || c["admission_verified"] != false
        || c["expires_at_unix_ms"].as_i64().is_none()
    {
        return Err(Error::PublicationState);
    }
    uuid(&c["check_run"])?;
    uuid(&c["binding"])?;
    let t: Target =
        serde_json::from_value(c["target"].clone()).map_err(|_| Error::PublicationState)?;
    t.validate().map_err(|_| Error::PublicationState)?;
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    spec.validate().map_err(|_| Error::PublicationState)?;
    if t.publication != r.id
        || t.epoch != r.epoch
        || t.app_id != spec.app_id
        || t.source.pull().repository_id != spec.upstream.repository_id
    {
        return Err(Error::PublicationState);
    }
    Ok((
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository(),"publication":r.id,"epoch":r.epoch,"check_run":c["check_run"],"binding":c["binding"],"target":t,"expires_at_unix_ms":c["expires_at_unix_ms"],"specification":spec}),
        t,
    ))
}
struct Session<'a> {
    r: &'a Request,
    credential: &'a Credential,
    intent: Value,
    target: Target,
}
impl Session<'_> {
    async fn command(&self, mut v: Value) -> Result<Value> {
        v["publication"] = json!(self.r.id);
        v["epoch"] = json!(self.r.epoch);
        command(&self.r.socket, self.credential, v).await
    }
    fn matches(&self, c: &Value, attempt: Option<Uuid>) -> Result<()> {
        for field in [
            "version",
            "publication",
            "check_run",
            "binding",
            "epoch",
            "target",
            "expires_at_unix_ms",
        ] {
            if c[field] != self.intent[field] {
                return Err(Error::PublicationState);
            }
        }
        if c["merge_verified"] != false
            || c["host_policy_verified"] != false
            || c["admission_verified"] != false
            || attempt.is_some_and(|a| c["attempt"] != a.to_string())
        {
            return Err(Error::PublicationState);
        }
        Ok(())
    }
    async fn context(&self) -> Result<Value> {
        let c = self
            .command(json!({"action":"check_publication_context"}))
            .await?;
        if identity(&c, self.r, self.credential)?.0 != self.intent {
            return Err(Error::PublicationState);
        }
        Ok(c)
    }
    async fn pending(&self, j: &Journal, c: &Value) -> Result<bool> {
        let Some((index, record)) = j.pending::<Report>()? else {
            return Ok(false);
        };
        record
            .report
            .validate(&self.target)
            .map_err(|_| Error::PublicationState)?;
        let attempt = uuid(&c["attempt"])?;
        let v=self.command(json!({"action":"check_publication_report","key":record.key,"attempt":attempt,"report":record.report})).await?;
        if v["version"] != 1
            || v["publication"] != self.r.id.to_string()
            || v["attempt"] != attempt.to_string()
            || !v["sequence"]
                .as_i64()
                .is_some_and(|n| (1..=1024).contains(&n))
            || v["recorded_observation"] != json!(record.report)
        {
            return Err(Error::PublicationState);
        }
        j.ack(index, &v)?;
        Ok(true)
    }
    async fn dispatch(&self, j: &Journal, attempt: Uuid, p: Preflight) -> Result<()> {
        if !p.eligible(&self.target) {
            return Err(Error::PublicationState);
        }
        j.bind("preflight.json", &json!(p))?;
        let v=self.command(json!({"action":"check_publication_dispatch","key":format!("github-check-dispatch-{attempt}"),"attempt":attempt,"preflight":p})).await?;
        self.matches(&v["receipt"], Some(attempt))?;
        if v["dispatch_granted"] != true
            || v["receipt"]["state"] != "attempting"
            || v["receipt"]["cancel_requested"] != false
            || v["receipt"]["unexpired"] != true
            || v["receipt"]["preflight"] != json!(p)
            || v["receipt"]["expires_at_unix_ms"]
                .as_i64()
                .ok_or(Error::PublicationState)?
                <= now()?
        {
            return Err(Error::PublicationState);
        }
        j.bind("dispatch.json", &v["receipt"])?;
        Ok(())
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    version: u8,
    mutation_not_sent: bool,
    started_at_unix_ms: i64,
    finished_at_unix_ms: i64,
}
fn preflight(c: &Value) -> Result<Option<Preflight>> {
    serde_json::from_value(c["preflight"].clone()).map_err(|_| Error::PublicationState)
}
fn failure_report(f: &Failure, c: &Value) -> Result<Report> {
    if f.version != 1 {
        return Err(Error::PublicationState);
    }
    Ok(Report {
        version: 1,
        started_at_unix_ms: f.started_at_unix_ms,
        finished_at_unix_ms: f.finished_at_unix_ms,
        outcome: if f.mutation_not_sent {
            Outcome::NotSent
        } else {
            Outcome::Unknown
        },
        preflight: preflight(c)?,
        check: None,
        token_revoked: false,
    })
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let v = collect(&r).await?;
    println!(
        "{}",
        serde_json::to_string_pretty(&v).map_err(|_| Error::Http)?
    );
    Ok(v["state"] == "observed")
}
async fn collect(r: &Request) -> Result<Value> {
    let credential = Credential::load(&r.credential_file)?;
    let mut c = command(
        &r.socket,
        &credential,
        json!({"action":"check_publication_context","publication":r.id,"epoch":r.epoch}),
    )
    .await?;
    let (intent, target) = identity(&c, r, &credential)?;
    let s = Session {
        r,
        credential: &credential,
        intent,
        target,
    };
    let j = Journal::open(&r.state, &s.intent)?;
    if s.pending(&j, &c).await? {
        return s.context().await;
    }
    c = s.context().await?;
    // Persisting the local no-send/unknown fact before another socket exchange
    // preserves it even when the coordinator's final response is unavailable.
    if c["report_sequence"] == 0
        && !c["attempt"].is_null()
        && let Some(f) = j.read_optional::<Failure>("transport-failure.json")?
    {
        let report = failure_report(&f, &c)?;
        report
            .validate(&s.target)
            .map_err(|_| Error::PublicationState)?;
        j.append(&Record::new(report, None))?;
        s.pending(&j, &c).await?;
        return s.context().await;
    }
    if matches!(
        c["state"].as_str(),
        Some("observed" | "not_sent" | "cancelled")
    ) || (r.recover_only && c["attempt"].is_null())
    {
        return Ok(c);
    }
    if c["state"] == "attempting" && c["preflight"].is_null() {
        // No irreversible permission has been committed. Recording no-send
        // atomically fences a late callback from the old process. If that
        // callback won the race, the server rejects this mismatched preflight.
        let report = match j.read_optional::<Report>("no-dispatch-report.json")? {
            Some(v) => v,
            None => {
                let stamp = now()?;
                let report = Report {
                    version: 1,
                    started_at_unix_ms: stamp,
                    finished_at_unix_ms: stamp,
                    outcome: Outcome::NotSent,
                    preflight: None,
                    check: None,
                    token_revoked: false,
                };
                j.bind("no-dispatch-report.json", &json!(report))?;
                report
            }
        };
        report
            .validate(&s.target)
            .map_err(|_| Error::PublicationState)?;
        if report.outcome != Outcome::NotSent || report.preflight.is_some() {
            return Err(Error::PublicationState);
        }
        let attempt = uuid(&c["attempt"])?;
        match s.command(json!({"action":"check_publication_report","key":format!("github-check-no-dispatch-{attempt}"),"attempt":attempt,"report":report})).await {
            Ok(v) => {
                if v["publication"]!=r.id.to_string() || v["attempt"]!=attempt.to_string()
                    || v["recorded_observation"]!=json!(report) {return Err(Error::PublicationState);}
                j.bind("no-dispatch-ack.json", &v)?;
                return s.context().await;
            }
            Err(e) => {
                c=s.context().await?;
                if matches!(c["state"].as_str(), Some("observed"|"not_sent"|"cancelled")) {return Ok(c);}
                if c["preflight"].is_null() {return Err(e);}
                // The old dispatch won the atomic race. Keep the rejected local
                // intent separately, and reconcile the now-authorized write.
            }
        }
    }
    let spec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    let (app, config) = load(&r.config)?;
    matches_config(&config, &spec)?;
    let connection = Connection::new(app)?;
    let dispatch = if c["attempt"].is_null() {
        let attempt = j.attempt()?;
        let v=s.command(json!({"action":"check_publication_begin","key":format!("github-check-begin-{attempt}"),"attempt":attempt})).await?;
        s.matches(&v["receipt"], Some(attempt))?;
        let granted = v["dispatch_granted"]
            .as_bool()
            .ok_or(Error::PublicationState)?;
        if granted && v["receipt"]["state"] != "attempting" {
            return Err(Error::PublicationState);
        }
        j.bind("begin.json", &v["receipt"])?;
        c = s.context().await?;
        s.matches(&c, Some(attempt))?;
        granted
    } else {
        false
    };
    let attempt = uuid(&c["attempt"])?;
    let start = now()?;
    let result = if dispatch {
        connection
            .publish_check(&config.repository, &s.target, |p| async {
                s.dispatch(&j, attempt, p)
                    .await
                    .map_err(|_| treeharbor_github::Error::PublicationStopped)
            })
            .await
    } else {
        connection
            .observe_published_check(&config.repository, &s.target, preflight(&c)?)
            .await
            .map_err(|error| treeharbor_github::MutationFailure {
                error,
                mutation_not_sent: false,
            })
    };
    match result {
        Ok(v) => {
            if v.binding != config.repository {
                return Err(Error::PublicationState);
            }
            v.report
                .validate(&s.target)
                .map_err(|_| Error::PublicationState)?;
            j.append(&Record::new(v.report, Some(v.token_expires_at)))?;
        }
        Err(e) => {
            eprintln!("{}", e.error);
            let failure = Failure {
                version: 1,
                mutation_not_sent: dispatch && e.mutation_not_sent,
                started_at_unix_ms: start,
                finished_at_unix_ms: now()?,
            };
            if dispatch {
                j.bind("transport-failure.json", &json!(failure))?;
            }
            c = s.context().await?;
            let report = failure_report(&failure, &c)?;
            report
                .validate(&s.target)
                .map_err(|_| Error::PublicationState)?;
            j.append(&Record::new(report, None))?;
        }
    }
    s.pending(&j, &c).await?;
    s.context().await
}
