//! App-wide delivery API, narrowed locally to an explicitly pinned repository.
//! Parsed provider payloads are never turned into authenticated webhook facts.
use super::*;
use crate::MutationFailure;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    future::Future,
    sync::atomic::{AtomicBool, Ordering},
};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use uuid::Uuid;

const PAGE_SIZE: usize = 100;
const RETENTION_MS: i64 = 3 * 24 * 60 * 60 * 1000;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RecoveryTarget {
    pub app_id: u64,
    pub binding: RepositoryBinding,
    pub callback_url: String,
}
impl RecoveryTarget {
    pub fn validate(&self) -> Result<()> {
        self.binding.validate()?;
        if [
            self.app_id,
            self.binding.installation_id,
            self.binding.repository_id,
            self.binding.owner_id,
        ]
        .iter()
        .any(|id| *id == 0 || *id > i64::MAX as u64)
            || self.callback_url.len() > 2048
        {
            return Err(Error::Configuration);
        }
        let url = Url::parse(&self.callback_url).map_err(|_| Error::Configuration)?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/github/events"
            || url.as_str() != self.callback_url
        {
            return Err(Error::Configuration);
        }
        Ok(())
    }
}

pub use treeharbor_github_contract::deliveries::DeliveryAttempt;

#[derive(Debug, Serialize)]
pub struct DeliveryPage {
    pub version: u8,
    pub target: RecoveryTarget,
    pub observed_at_unix_ms: i64,
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    /// Includes filtered entries, so an operator cannot evade scan limits by
    /// traversing an App that mostly serves other repositories.
    pub scanned: usize,
    pub attempts: Vec<DeliveryAttempt>,
}
#[derive(Debug, Serialize)]
pub struct RedeliveryReceipt {
    pub version: u8,
    pub target: RecoveryTarget,
    pub attempt: DeliveryAttempt,
    pub accepted_at_unix_ms: i64,
    /// Always false. Only the independent signed inbox can confirm receipt.
    pub callback_confirmed: bool,
}

// Named fields reject duplicate consumed fields; unknown provider fields can be
// added without breaking compatibility. Required nullable fields use Value so
// omission does not silently default to an innocent observation.
#[derive(Deserialize)]
struct App {
    id: u64,
    client_id: String,
}
#[derive(Deserialize)]
struct Configuration {
    url: String,
    content_type: String,
    insecure_ssl: Value,
}
#[derive(Deserialize)]
struct Attempt {
    id: u64,
    guid: String,
    delivered_at: String,
    redelivery: bool,
    status_code: Value,
    event: String,
    action: Value,
    installation_id: Value,
    repository_id: Value,
    throttled_at: Value,
}
// Flattening would buffer duplicate named fields. Explicitly list the consumed
// fields for details too; request/response bodies are deliberately ignored.
#[derive(Deserialize)]
struct Detail {
    id: u64,
    guid: String,
    delivered_at: String,
    redelivery: bool,
    status_code: Value,
    event: String,
    action: Value,
    installation_id: Value,
    repository_id: Value,
    throttled_at: Value,
    url: String,
}
impl Detail {
    fn attempt(self) -> Attempt {
        Attempt {
            id: self.id,
            guid: self.guid,
            delivered_at: self.delivered_at,
            redelivery: self.redelivery,
            status_code: self.status_code,
            event: self.event,
            action: self.action,
            installation_id: self.installation_id,
            repository_id: self.repository_id,
            throttled_at: self.throttled_at,
        }
    }
}
fn timestamp(s: &str) -> Result<i64> {
    if s.len() > 40 {
        return Err(Error::WebhookRecovery);
    }
    OffsetDateTime::parse(s, &Rfc3339)
        .ok()
        .and_then(|t| i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok())
        .filter(|t| *t > 0)
        .ok_or(Error::WebhookRecovery)
}
fn milliseconds() -> Result<i64> {
    i64::try_from(now()?)
        .ok()
        .and_then(|t| t.checked_mul(1000))
        .ok_or(Error::Clock)
}
fn optional_number(v: &Value) -> Result<Option<u64>> {
    if v.is_null() {
        return Ok(None);
    }
    v.as_u64()
        .filter(|n| *n > 0 && *n <= i64::MAX as u64)
        .map(Some)
        .ok_or(Error::WebhookRecovery)
}
impl Attempt {
    fn scoped(self, target: &RecoveryTarget, observed_ms: i64) -> Result<Option<DeliveryAttempt>> {
        let installation = optional_number(&self.installation_id)?;
        let repository = optional_number(&self.repository_id)?;
        if installation != Some(target.binding.installation_id)
            || repository != Some(target.binding.repository_id)
            || self.event != "merge_group"
        {
            return Ok(None);
        }
        let guid: Uuid = self.guid.parse().map_err(|_| Error::WebhookRecovery)?;
        if guid.to_string() != self.guid {
            return Err(Error::WebhookRecovery);
        }
        let attempt = DeliveryAttempt {
            id: self.id,
            guid,
            installation_id: target.binding.installation_id,
            repository_id: target.binding.repository_id,
            delivered_at_unix_ms: timestamp(&self.delivered_at)?,
            redelivery: self.redelivery,
            action: self.action.as_str().ok_or(Error::WebhookRecovery)?.into(),
            status_code: if self.status_code.is_null() {
                None
            } else {
                Some(
                    self.status_code
                        .as_u64()
                        .and_then(|v| u16::try_from(v).ok())
                        .ok_or(Error::WebhookRecovery)?,
                )
            },
            throttled_at_unix_ms: if self.throttled_at.is_null() {
                None
            } else {
                Some(timestamp(
                    self.throttled_at.as_str().ok_or(Error::WebhookRecovery)?,
                )?)
            },
        };
        attempt
            .validate(
                target.binding.installation_id,
                target.binding.repository_id,
                observed_ms,
            )
            .map_err(|_| Error::WebhookRecovery)?;
        Ok(Some(attempt))
    }
}

fn cursor(value: &str) -> Result<()> {
    if value.is_empty() || value.len() > 1024 || !value.bytes().all(|b| b.is_ascii_graphic()) {
        return Err(Error::WebhookPagination);
    }
    Ok(())
}
fn page_path(after: Option<&str>) -> Result<String> {
    let mut url = Url::parse("https://api.github.com/app/hook/deliveries")
        .map_err(|_| Error::Configuration)?;
    url.query_pairs_mut()
        .append_pair("per_page", &PAGE_SIZE.to_string());
    if let Some(after) = after {
        cursor(after)?;
        url.query_pairs_mut().append_pair("cursor", after);
    }
    Ok(format!(
        "app/hook/deliveries?{}",
        url.query().ok_or(Error::Configuration)?
    ))
}
fn next_cursor(headers: &header::HeaderMap, current: Option<&str>) -> Result<Option<String>> {
    let mut found = None;
    let mut relations = BTreeSet::new();
    let mut total = 0;
    for value in headers.get_all(header::LINK) {
        total += value.as_bytes().len();
        if total > 8192 {
            return Err(Error::WebhookPagination);
        }
        let value = value.to_str().map_err(|_| Error::WebhookPagination)?;
        for item in value.split(',') {
            let mut pieces = item.trim().split(';');
            let endpoint = pieces
                .next()
                .and_then(|s| s.strip_prefix('<'))
                .and_then(|s| s.strip_suffix('>'))
                .ok_or(Error::WebhookPagination)?;
            let relation = pieces
                .next()
                .map(str::trim)
                .and_then(|s| s.strip_prefix("rel=\""))
                .and_then(|s| s.strip_suffix('"'))
                .ok_or(Error::WebhookPagination)?;
            if pieces.next().is_some()
                || !matches!(relation, "next" | "prev" | "first" | "last")
                || !relations.insert(relation.to_string())
            {
                return Err(Error::WebhookPagination);
            }
            let url = Url::parse(endpoint).map_err(|_| Error::WebhookPagination)?;
            if url.scheme() != "https"
                || url.host_str() != Some("api.github.com")
                || url.port().is_some()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
                || url.path() != "/app/hook/deliveries"
            {
                return Err(Error::WebhookPagination);
            }
            let mut after = None;
            let mut count = false;
            for (key, value) in url.query_pairs() {
                match key.as_ref() {
                    "per_page" if !count && value == PAGE_SIZE.to_string() => count = true,
                    "cursor" if after.is_none() => {
                        cursor(&value)?;
                        after = Some(value.into_owned());
                    }
                    _ => return Err(Error::WebhookPagination),
                }
            }
            if !count {
                return Err(Error::WebhookPagination);
            }
            if relation == "next" {
                let after = after.ok_or(Error::WebhookPagination)?;
                if Some(after.as_str()) == current {
                    return Err(Error::WebhookPagination);
                }
                found = Some(after);
            }
        }
    }
    Ok(found)
}

impl Connection {
    async fn recovery_preflight(&self, target: &RecoveryTarget) -> Result<crate::auth::Secret> {
        if target.app_id != self.credentials.app_id {
            return Err(Error::Identity);
        }
        let jwt = self.credentials.jwt(now()?)?;
        let app: App = self
            .json(
                Stage::AppIdentity,
                Method::GET,
                "app",
                jwt.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        if app.id != target.app_id || app.client_id != self.credentials.client_id() {
            return Err(Error::Identity);
        }
        let install: wire::Installation = self
            .json(
                Stage::Installation,
                Method::GET,
                &format!("app/installations/{}", target.binding.installation_id),
                jwt.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        install.check(&target.binding, target.app_id)?;
        let config: Configuration = self
            .json(
                Stage::WebhookConfiguration,
                Method::GET,
                "app/hook/config",
                jwt.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        if config.url != target.callback_url
            || config.content_type != "json"
            || !(config.insecure_ssl == "0" || config.insecure_ssl == 0)
        {
            return Err(Error::WebhookRecovery);
        }
        Ok(jwt)
    }
    /// One bounded page, including all statuses. End of pagination is not proof
    /// of retained time coverage or callback receipt. No mutation or mint occurs.
    pub async fn list_webhook_deliveries(
        &self,
        target: &RecoveryTarget,
        after: Option<&str>,
    ) -> Result<DeliveryPage> {
        target.validate()?;
        let path = page_path(after)?;
        tokio::time::timeout(PROBE_TIMEOUT, async {
            let _slot = self
                .slots
                .acquire()
                .await
                .map_err(|_| Error::Configuration)?;
            let jwt = self.recovery_preflight(target).await?;
            let (_, headers, bytes) = self
                .request_headers(
                    Stage::WebhookDeliveries,
                    Method::GET,
                    &path,
                    jwt.header()?,
                    None,
                    &[StatusCode::OK],
                )
                .await?;
            let next_cursor = next_cursor(&headers, after)?;
            let entries: Vec<Attempt> =
                serde_json::from_slice(&bytes).map_err(|_| Error::Response {
                    stage: Stage::WebhookDeliveries,
                })?;
            if entries.len() > PAGE_SIZE {
                return Err(Error::WebhookPagination);
            }
            let observed_at_unix_ms = milliseconds()?;
            let scanned = entries.len();
            let mut attempts = Vec::new();
            let mut ids = BTreeSet::new();
            for entry in entries {
                if entry.id == 0 || entry.id > i64::MAX as u64 || !ids.insert(entry.id) {
                    return Err(Error::WebhookRecovery);
                }
                if let Some(entry) = entry.scoped(target, observed_at_unix_ms)? {
                    attempts.push(entry);
                }
            }
            Ok(DeliveryPage {
                version: 1,
                target: target.clone(),
                observed_at_unix_ms,
                cursor: after.map(str::to_owned),
                next_cursor,
                scanned,
                attempts,
            })
        })
        .await
        .map_err(|_| Error::Deadline)?
    }
    /// Exactly one POST after fresh identity/detail checks and caller eligibility.
    /// The caller must first record durable intent. Cancellation/process death is
    /// unknown; a receipt means only HTTP202, never signed inbox persistence.
    pub async fn redeliver_webhook<F, Fut>(
        &self,
        target: &RecoveryTarget,
        expected: &DeliveryAttempt,
        eligible: F,
    ) -> std::result::Result<RedeliveryReceipt, MutationFailure>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let sent = AtomicBool::new(false);
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            target.validate()?;
            let observed = milliseconds()?;
            expected
                .validate(
                    target.binding.installation_id,
                    target.binding.repository_id,
                    observed,
                )
                .map_err(|_| Error::WebhookRecovery)?;
            if expected.delivered_at_unix_ms < observed.saturating_sub(RETENTION_MS) {
                return Err(Error::WebhookRecovery);
            }
            let _slot = self
                .slots
                .acquire()
                .await
                .map_err(|_| Error::Configuration)?;
            let jwt = self.recovery_preflight(target).await?;
            let detail: Detail = self
                .json(
                    Stage::WebhookDeliveryDetail,
                    Method::GET,
                    &format!("app/hook/deliveries/{}", expected.id),
                    jwt.header()?,
                    None,
                    StatusCode::OK,
                )
                .await?;
            if detail.url != target.callback_url {
                return Err(Error::WebhookRecovery);
            }
            let observed = detail
                .attempt()
                .scoped(target, milliseconds()?)?
                .ok_or(Error::WebhookRecovery)?;
            if !observed.same_attempt(expected) {
                return Err(Error::WebhookRecovery);
            }
            let header = jwt.header()?;
            eligible().await?;
            sent.store(true, Ordering::SeqCst);
            self.request(
                Stage::WebhookRedelivery,
                Method::POST,
                &format!("app/hook/deliveries/{}/attempts", expected.id),
                header,
                None,
                StatusCode::ACCEPTED,
            )
            .await?;
            Ok(RedeliveryReceipt {
                version: 1,
                target: target.clone(),
                attempt: observed,
                accepted_at_unix_ms: milliseconds()?,
                callback_confirmed: false,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        result.map_err(|error| MutationFailure {
            error,
            mutation_not_sent: !sent.load(Ordering::SeqCst),
        })
    }
}
