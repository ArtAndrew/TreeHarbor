use super::publish::{
    journal::{Journal, Record},
    now, uuid,
};
use super::*;
use serde::Serialize;
use treeharbor_github::{DeliveryAttempt, DeliveryDetails, RecoveryTarget};
use treeharbor_github_contract::deliveries::DeliveryPageReport;
use treeharbor_metadata::github::{RedeliveryOutcome, RedeliveryReport};

pub(super) struct Request {
    pub config: PathBuf,
    pub socket: PathBuf,
    pub credential_file: PathBuf,
    pub id: Uuid,
    pub epoch: i64,
    pub state: PathBuf,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Saved {
    Page {
        report: DeliveryPageReport,
    },
    Details {
        details: DeliveryDetails,
    },
    Report {
        provider_id: i64,
        dispatch: Uuid,
        report: RedeliveryReport,
    },
}
fn identity(c: &Value, r: &Request, credential: &Credential) -> Result<Value> {
    if c["version"] != 1
        || uuid(&c["recovery"])? != r.id
        || c["epoch"] != r.epoch
        || c["coverage_complete"] != false
        || c["merge_verified"] != false
    {
        return Err(Error::PublicationState);
    }
    uuid(&c["channel"])?;
    uuid(&c["binding"])?;
    let spec: treeharbor_metadata::github::BindingSpec =
        serde_json::from_value(c["specification"].clone()).map_err(|_| Error::PublicationState)?;
    spec.validate().map_err(|_| Error::PublicationState)?;
    let target = target(c)?;
    target.validate()?;
    for field in ["until_unix_ms", "expires_at_unix_ms", "since_unix_ms"] {
        if !c[field].as_i64().is_some_and(|v| v > 0) {
            return Err(Error::PublicationState);
        }
    }
    for field in ["max_pages", "max_deliveries"] {
        if !c[field].as_i64().is_some_and(|v| (1..=100).contains(&v)) {
            return Err(Error::PublicationState);
        }
    }
    Ok(
        json!({"version":1,"tenant":credential.scope.tenant(),"repository":credential.scope.repository(),
        "recovery":r.id,"epoch":r.epoch,"channel":c["channel"],"binding":c["binding"],"specification":spec,
        "callback_url":c["callback_url"],"since_unix_ms":c["since_unix_ms"],"until_unix_ms":c["until_unix_ms"],
        "expires_at_unix_ms":c["expires_at_unix_ms"],"max_pages":c["max_pages"],"max_deliveries":c["max_deliveries"]}),
    )
}
fn target(c: &Value) -> Result<RecoveryTarget> {
    Ok(RecoveryTarget {
        app_id: c["specification"]["app_id"]
            .as_u64()
            .ok_or(Error::PublicationState)?,
        binding: serde_json::from_value(c["specification"]["upstream"].clone())
            .map_err(|_| Error::PublicationState)?,
        callback_url: c["callback_url"]
            .as_str()
            .ok_or(Error::PublicationState)?
            .into(),
    })
}
struct Session<'a> {
    r: &'a Request,
    credential: &'a Credential,
    intent: Value,
}
impl Session<'_> {
    async fn command(&self, mut v: Value) -> Result<Value> {
        v["recovery"] = json!(self.r.id);
        v["epoch"] = json!(self.r.epoch);
        command(&self.r.socket, self.credential, v).await
    }
    async fn context(&self, after: i64) -> Result<Value> {
        let c = self
            .command(json!({"action":"recovery_context","after":after}))
            .await?;
        if identity(&c, self.r, self.credential)? != self.intent {
            return Err(Error::PublicationState);
        }
        Ok(c)
    }
    async fn delivery(&self, after: i64, provider_id: i64) -> Result<(Value, Value)> {
        let c = self.context(after).await?;
        let a = c["deliveries"]
            .as_array()
            .and_then(|values| values.iter().find(|v| v["provider_id"] == provider_id))
            .ok_or(Error::PublicationState)?
            .clone();
        Ok((c, a))
    }
    async fn flush(&self, j: &Journal) -> Result<()> {
        let Some((i, record)) = j.pending::<Saved>()? else {
            return Ok(());
        };
        let v = match &record.report {
            Saved::Page { report } => {
                let v = self
                    .command(json!({"action":"recovery_page","key":record.key,"report":report}))
                    .await?;
                if v["recorded_page"] != json!(report) {
                    return Err(Error::PublicationState);
                }
                v
            }
            Saved::Details { details } => {
                let v = self
                    .command(
                        json!({"action":"recovery_details","key":record.key,"details":details}),
                    )
                    .await?;
                if v["details"] != json!(details) {
                    return Err(Error::PublicationState);
                }
                v
            }
            Saved::Report {
                provider_id,
                dispatch,
                report,
            } => {
                let v=self.command(json!({"action":"redelivery_report","key":record.key,"provider_id":provider_id,"dispatch":dispatch,"report":report})).await?;
                if v["dispatch"] != json!(dispatch)
                    || v["provider_id"] != *provider_id
                    || v["report"] != json!(report)
                {
                    return Err(Error::PublicationState);
                }
                v
            }
        };
        j.ack(i, &v)
    }
    async fn save(&self, j: &Journal, value: Saved) -> Result<()> {
        j.append(&Record::new(value, None))?;
        self.flush(j).await
    }
    fn connection(&self) -> Result<Connection> {
        let spec = serde_json::from_value(self.intent["specification"].clone())
            .map_err(|_| Error::PublicationState)?;
        let (app, config) = load(&self.r.config)?;
        matches_config(&config, &spec)?;
        Ok(Connection::new(app)?)
    }
}
fn live(c: &Value) -> Result<bool> {
    let unexpired = c["unexpired"].as_bool().ok_or(Error::PublicationState)?;
    let cancelled = c["cancelled"].as_bool().ok_or(Error::PublicationState)?;
    Ok(unexpired
        && !cancelled
        && c["expires_at_unix_ms"]
            .as_i64()
            .ok_or(Error::PublicationState)?
            > now()?)
}
fn number(c: &Value, field: &str) -> Result<i64> {
    c[field]
        .as_i64()
        .filter(|n| *n >= 0)
        .ok_or(Error::PublicationState)
}
fn dispatch(j: &Journal, p: i64) -> Result<Uuid> {
    if p <= 0 {
        return Err(Error::PublicationState);
    }
    let name = format!("dispatch-{p}.json");
    let path = j.root.join(&name);
    if !path.try_exists()? {
        j.bind(
            &name,
            &json!({"version":1,"provider_id":p,"dispatch":Uuid::new_v4()}),
        )?;
    }
    let v: Value = serde_json::from_slice(&files::read_private(&path, 1024)?)
        .map_err(|_| Error::PublicationState)?;
    if v["version"] != 1 || v["provider_id"] != p {
        return Err(Error::PublicationState);
    }
    uuid(&v["dispatch"])
}
pub(super) async fn run(r: Request) -> Result<bool> {
    let credential = Credential::load(&r.credential_file)?;
    let c = command(
        &r.socket,
        &credential,
        json!({"action":"recovery_context","epoch":r.epoch,"recovery":r.id,"after":0}),
    )
    .await?;
    let s = Session {
        r: &r,
        credential: &credential,
        intent: identity(&c, &r, &credential)?,
    };
    let j = Journal::open(&r.state, &s.intent)?;
    // Recorded local observations are flushed before requiring an App key.
    s.flush(&j).await?;
    let t = target(&c)?;
    for _ in 0..100 {
        let c = s.context(0).await?;
        if !live(&c)?
            || c["scan_exhausted"] == true
            || number(&c, "page_count")? >= number(&c, "max_pages")?
        {
            break;
        }
        let cursor = if c["next_cursor"].is_null() {
            None
        } else {
            Some(c["next_cursor"].as_str().ok_or(Error::PublicationState)?)
        };
        let page = s.connection()?.list_webhook_deliveries(&t, cursor).await?;
        if page.target != t {
            return Err(Error::PublicationState);
        }
        let report = DeliveryPageReport {
            version: page.version,
            observed_at_unix_ms: page.observed_at_unix_ms,
            cursor: page.cursor,
            next_cursor: page.next_cursor,
            scanned: u16::try_from(page.scanned).map_err(|_| Error::PublicationState)?,
            attempts: page.attempts,
        };
        s.save(&j, Saved::Page { report }).await?;
    }
    let mut after = 0;
    let mut all_received = true;
    let mut seen = 0;
    let mut received = 0;
    let mut exhausted = false;
    for _ in 0..1250 {
        let c = s.context(after).await?;
        let values = c["deliveries"].as_array().ok_or(Error::PublicationState)?;
        if values.len() > 8 {
            return Err(Error::PublicationState);
        }
        for value in values {
            let p = number(value, "provider_id")?;
            if p <= after {
                return Err(Error::PublicationState);
            }
            // A previous entry may have dispatched the same GUID, or a callback
            // may have arrived since this page was read. Refresh before effects.
            let (fresh, mut a) = s.delivery(after, p).await?;
            if a["details"].is_null()
                && live(&fresh)?
                && number(&fresh, "detail_count")? < number(&fresh, "max_deliveries")?
            {
                let observed: DeliveryAttempt = serde_json::from_value(a["observation"].clone())
                    .map_err(|_| Error::PublicationState)?;
                let details = s
                    .connection()?
                    .inspect_webhook_delivery(&t, &observed)
                    .await?;
                s.save(&j, Saved::Details { details }).await?;
                a = s.delivery(after, p).await?.1;
            }
            if !a["details"].is_null()
                && a["dispatch"].is_null()
                && a["related_dispatch"].is_null()
                && a["inbox"]["state"] == "missing"
                && live(&s.context(after).await?)?
            {
                let expected: DeliveryDetails = serde_json::from_value(a["details"].clone())
                    .map_err(|_| Error::PublicationState)?;
                let connection = s.connection()?;
                let id = dispatch(&j, p)?;
                let begin=s.command(json!({"action":"redelivery_begin","key":format!("github-redelivery-{}-{p}-{id}",r.id),"provider_id":p,"dispatch":id})).await?;
                if begin["receipt"]["recovery"] != json!(r.id)
                    || begin["receipt"]["provider_id"] != p
                    || begin["receipt"]["dispatch"] != json!(id)
                    || begin["receipt"]["details"] != json!(expected)
                {
                    return Err(Error::PublicationState);
                }
                if begin["dispatch_granted"]
                    .as_bool()
                    .ok_or(Error::PublicationState)?
                {
                    let result=connection.redeliver_webhook(&t,&expected,||async {
                        let v=s.command(json!({"action":"redelivery_context","provider_id":p,"dispatch":id})).await.map_err(|_|treeharbor_github::Error::PublicationStopped)?;
                        if v["recovery"]!=json!(r.id) || v["provider_id"]!=p || v["dispatch"]!=json!(id) || v["details"]!=json!(expected) {return Err(treeharbor_github::Error::PublicationStopped);}
                        Ok(())
                    }).await;
                    let outcome = match result {
                        Ok(v) => {
                            if v.target != t
                                || !v.attempt.same_attempt(&expected.attempt)
                                || v.payload_sha256 != expected.payload_sha256
                                || v.callback_confirmed
                            {
                                return Err(Error::PublicationState);
                            }
                            RedeliveryOutcome::Accepted
                        }
                        Err(f) => {
                            eprintln!("{}", f.error);
                            if f.mutation_not_sent {
                                RedeliveryOutcome::NotSent
                            } else {
                                RedeliveryOutcome::Unknown
                            }
                        }
                    };
                    s.save(
                        &j,
                        Saved::Report {
                            provider_id: p,
                            dispatch: id,
                            report: RedeliveryReport {
                                version: 1,
                                observed_at_unix_ms: now()?,
                                outcome,
                            },
                        },
                    )
                    .await?;
                }
                a = s.delivery(after, p).await?.1;
            }
            seen += 1;
            if a["inbox"]["state"] == "received" {
                received += 1;
            } else {
                all_received = false;
            }
        }
        if c["next_after"].is_null() {
            exhausted = true;
            break;
        }
        let next = number(&c, "next_after")?;
        if next <= after {
            return Err(Error::PublicationState);
        }
        after = next;
    }
    let mut result = s.context(0).await?;
    result["observed_deliveries"] = json!(seen);
    result["observed_received"] = json!(received);
    result["all_observed_received"] = json!(all_received && exhausted);
    println!(
        "{}",
        serde_json::to_string_pretty(&result).map_err(|_| Error::Http)?
    );
    Ok(exhausted
        && all_received
        && result["scan_exhausted"] == true
        && result["retention_gap"] == false)
}
