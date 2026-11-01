//! Scoped PR metadata delivery; neither admission nor merge authorization.
use super::*;
use serde::{Deserialize, Serialize};
use treeharbor_core::{GitOid, RevisionRef};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullIdentity {
    pub number: u64,
    pub node_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPull {
    pub identity: PullIdentity,
    pub title: String,
    pub body: String,
    pub draft: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullTarget {
    pub operation: Uuid,
    pub branch_id: Uuid,
    pub git_revision: String,
    pub base: String,
    pub title: String,
    /// Includes the server-derived operation marker.
    pub body: String,
    pub draft: bool,
    pub previous: Option<ExpectedPull>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullSnapshot {
    pub identity: PullIdentity,
    pub head_repository: u64,
    pub base_repository: u64,
    pub head_ref: String,
    pub base_ref: String,
    pub head_revision: String,
    pub base_revision: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub state: String,
    pub merged: bool,
}
#[derive(Debug, Serialize)]
pub struct PullObservation {
    pub version: u8,
    pub binding: RepositoryBinding,
    pub operation: Uuid,
    pub pull: Option<PullSnapshot>,
    pub observed_at_unix: u64,
    pub token_revoked: bool,
    pub token_expires_at: String,
}

#[derive(Debug, Serialize)]
pub struct RestorePullReport {
    pub version: u8,
    pub app_id: u64,
    pub installation_id: u64,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: PullSnapshot,
    pub after: PullSnapshot,
    pub token_revoked: bool,
}
#[derive(Debug, Serialize)]
pub struct ObservedRestorePull {
    pub binding: RepositoryBinding,
    pub report: RestorePullReport,
    pub token_expires_at: String,
}
fn capture_time() -> Result<i64> {
    let seconds = i64::try_from(now()?).map_err(|_| Error::Configuration)?;
    seconds.checked_mul(1000).ok_or(Error::Configuration)
}
fn identity(id: &PullIdentity) -> Result<()> {
    if id.number == 0
        || id.number > i64::MAX as u64
        || id.node_id.is_empty()
        || id.node_id.len() > 256
        || !id
            .node_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"=_-".contains(&c))
    {
        return Err(Error::Configuration);
    }
    Ok(())
}
fn revision(value: &str) -> Result<()> {
    match value.parse::<RevisionRef>() {
        Ok(RevisionRef::Git(oid @ GitOid::Sha1(bytes)))
            if bytes != [0; 20] && RevisionRef::Git(oid).to_string() == value =>
        {
            Ok(())
        }
        _ => Err(Error::Configuration),
    }
}
fn metadata(title: &str, body: &str) -> Result<()> {
    if title.trim().is_empty()
        || title.len() > 256
        || title.chars().any(char::is_control)
        || body.len() > 9000
        || body.contains('\0')
    {
        return Err(Error::Configuration);
    }
    Ok(())
}
impl PullTarget {
    pub fn marker(&self) -> String {
        format!("<!-- treeharbor-pull:{} -->", self.operation)
    }
    pub fn head(&self) -> String {
        format!("treeharbor/{}", self.branch_id.simple())
    }
    pub fn validate(&self) -> Result<()> {
        if self.operation.is_nil()
            || self.branch_id.is_nil()
            || self.base.is_empty()
            || self.base.len() > 128
            || self.base.split('/').any(|p| {
                p.is_empty()
                    || p.len() > 64
                    || !p.as_bytes()[0].is_ascii_alphanumeric()
                    || p.ends_with('.')
                    || p.ends_with(".lock")
                    || p.contains("..")
                    || !p
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            })
            || self.base == self.head()
            || !self.body.ends_with(&self.marker())
        {
            return Err(Error::Configuration);
        }
        revision(&self.git_revision)?;
        metadata(&self.title, &self.body)?;
        if let Some(previous) = &self.previous {
            identity(&previous.identity)?;
            metadata(&previous.title, &previous.body)?;
            if previous.draft != self.draft {
                return Err(Error::Configuration);
            }
        }
        Ok(())
    }
    pub fn matches(&self, p: &PullSnapshot) -> bool {
        p.head_revision == self.git_revision
            && p.title == self.title
            && p.body == self.body
            && p.draft == self.draft
    }
}
#[derive(Deserialize)]
struct Side {
    #[serde(rename = "ref")]
    name: String,
    sha: String,
    repo: wire::RepositoryId,
}
#[derive(Deserialize)]
struct Pull {
    number: u64,
    node_id: String,
    title: String,
    #[serde(deserialize_with = "required_body")]
    body: Option<String>,
    draft: bool,
    state: String,
    head: Side,
    base: Side,
    merged: Option<bool>,
}
fn required_body<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
impl Pull {
    fn scoped(&self, b: &RepositoryBinding, t: &PullTarget) -> Result<()> {
        identity(&PullIdentity {
            number: self.number,
            node_id: self.node_id.clone(),
        })?;
        if self.head.repo.id != b.repository_id
            || self.base.repo.id != b.repository_id
            || self.head.name != t.head()
            || self.base.name != t.base
            || !matches!(self.state.as_str(), "open" | "closed")
        {
            return Err(Error::PullChanged);
        }
        if let Some(previous) = &t.previous
            && (self.number != previous.identity.number
                || self.node_id != previous.identity.node_id)
        {
            return Err(Error::PullChanged);
        }
        Ok(())
    }
    fn snapshot(self, b: &RepositoryBinding, t: &PullTarget) -> Result<PullSnapshot> {
        self.scoped(b, t)?;
        let head_revision = format!("git:sha1:{}", self.head.sha);
        let base_revision = format!("git:sha1:{}", self.base.sha);
        revision(&head_revision)?;
        revision(&base_revision)?;
        let body = self.body.unwrap_or_default();
        metadata(&self.title, &body)?;
        let merged = self.merged.ok_or(Error::Response {
            stage: Stage::PullRequest,
        })?;
        if merged && self.state != "closed" {
            return Err(Error::PullChanged);
        }
        Ok(PullSnapshot {
            identity: PullIdentity {
                number: self.number,
                node_id: self.node_id,
            },
            head_repository: self.head.repo.id,
            base_repository: self.base.repo.id,
            head_ref: self.head.name,
            base_ref: self.base.name,
            head_revision,
            base_revision,
            title: self.title,
            body,
            draft: self.draft,
            state: self.state,
            merged,
        })
    }
}
impl Connection {
    /// Read one explicitly selected PR twice. Unlike normal operation recovery,
    /// this does not search for or automatically select a matching marker.
    pub async fn observe_restore_pull(
        &self,
        b: &RepositoryBinding,
        target: &PullTarget,
        selected: &PullIdentity,
    ) -> Result<ObservedRestorePull> {
        b.validate()?;
        target.validate()?;
        identity(selected)?;
        if target
            .previous
            .as_ref()
            .is_some_and(|p| p.identity != *selected)
        {
            return Err(Error::PullChanged);
        }
        let mut target = target.clone();
        target.previous = Some(ExpectedPull {
            identity: selected.clone(),
            title: target.title.clone(),
            body: target.body.clone(),
            draft: target.draft,
        });
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, wire::Capability::PullRead))
                .await
                .map_err(|_| Error::Deadline)??;
        let observed = tokio::time::timeout(PROBE_TIMEOUT, async {
            let started_at_unix_ms = capture_time()?;
            let probe = self
                .observe_for(b, &installation, &token, wire::Capability::PullRead)
                .await?;
            if now()?.saturating_add(PROBE_TIMEOUT.as_secs() + 30)
                >= token.check_for(b.repository_id, now()?, wire::Capability::PullRead)?
            {
                return Err(Error::TokenLifetime);
            }
            let before = self.pull_get(b, &target, &token, selected.number).await?;
            let after = self.pull_get(b, &target, &token, selected.number).await?;
            Ok(RestorePullReport {
                version: 1,
                app_id: probe.app_id,
                installation_id: b.installation_id,
                started_at_unix_ms,
                finished_at_unix_ms: capture_time()?,
                before,
                after,
                token_revoked: false,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|v| v);
        let revoked = self.revoke(&token).await;
        let mut report = observed?;
        report.token_revoked = revoked.is_ok();
        Ok(ObservedRestorePull {
            binding: b.clone(),
            report,
            token_expires_at: token.expires_at.clone(),
        })
    }

    async fn pull_get(
        &self,
        b: &RepositoryBinding,
        t: &PullTarget,
        token: &wire::Token,
        number: u64,
    ) -> Result<PullSnapshot> {
        let p: Pull = self
            .json(
                Stage::PullRequest,
                Method::GET,
                &format!("repos/{}/{}/pulls/{number}", b.owner, b.name),
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        if p.number != number {
            return Err(Error::PullChanged);
        }
        p.snapshot(b, t)
    }
    // Ignore all response URLs and Link headers; derive bounded pages locally.
    async fn pull_find(
        &self,
        b: &RepositoryBinding,
        t: &PullTarget,
        token: &wire::Token,
    ) -> Result<(Option<PullSnapshot>, bool)> {
        if let Some(previous) = &t.previous {
            return self
                .pull_get(b, t, token, previous.identity.number)
                .await
                .map(|p| (Some(p), false));
        }
        let mut found = None;
        let mut open = false;
        let mut seen = std::collections::BTreeSet::new();
        for page in 1..=20 {
            let path = format!("repos/{}/{}/pulls", b.owner, b.name);
            let mut url = self.base.join(&path).map_err(|_| Error::Configuration)?;
            url.query_pairs_mut()
                .append_pair("state", "all")
                .append_pair("head", &format!("{}:{}", b.owner, t.head()))
                .append_pair("base", &t.base)
                .append_pair("per_page", "5")
                .append_pair("page", &page.to_string());
            let path = format!("{}?{}", path, url.query().ok_or(Error::Configuration)?);
            let list: Vec<Pull> = self
                .json(
                    Stage::PullRequest,
                    Method::GET,
                    &path,
                    token.token.header()?,
                    None,
                    StatusCode::OK,
                )
                .await?;
            if list.len() > 5 {
                return Err(Error::PullSearch);
            }
            let end = list.len() < 5;
            for p in list {
                p.scoped(b, t)?;
                if !seen.insert(p.number) {
                    return Err(Error::PullSearch);
                }
                open |= p.state == "open";
                if p.body
                    .as_deref()
                    .is_some_and(|body| body.ends_with(&t.marker()))
                    && found.replace((p.number, p.node_id)).is_some()
                {
                    return Err(Error::PullSearch);
                }
            }
            if end {
                return match found {
                    None => Ok((None, open)),
                    Some((number, node)) => {
                        let pull = self.pull_get(b, t, token, number).await?;
                        if pull.identity.node_id != node || !pull.body.ends_with(&t.marker()) {
                            return Err(Error::PullChanged);
                        }
                        Ok((Some(pull), open))
                    }
                };
            }
        }
        Err(Error::PullSearch)
    }
    /// No write, even for missing/closed/different PRs. Caller persists facts.
    pub async fn observe_pull(
        &self,
        b: &RepositoryBinding,
        t: &PullTarget,
    ) -> Result<PullObservation> {
        self.deliver_pull(b, t, false, || async { Ok(()) }).await
    }
    /// Caller must hold one fresh durable dispatch; this attempts at most one
    /// POST or PATCH, then observes independently even after an HTTP error.
    pub async fn publish_pull<G, F>(
        &self,
        b: &RepositoryBinding,
        t: &PullTarget,
        eligible: G,
    ) -> Result<PullObservation>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        self.deliver_pull(b, t, true, eligible).await
    }
    async fn deliver_pull<G, F>(
        &self,
        b: &RepositoryBinding,
        t: &PullTarget,
        write: bool,
        mut eligible: G,
    ) -> Result<PullObservation>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        b.validate()?;
        t.validate()?;
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let capability = if write {
            wire::Capability::PullWrite
        } else {
            wire::Capability::PullRead
        };
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, async {
            if write {
                eligible().await?;
            }
            self.mint_for(b, capability).await
        })
        .await
        .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            let repository = self
                .observe_for(b, &installation, &token, capability)
                .await?;
            let (before, open) = self.pull_find(b, t, &token).await?;
            if !write || before.as_ref().is_some_and(|p| t.matches(p)) {
                return Ok(before);
            }
            if repository.archived || repository.disabled {
                return Err(Error::RepositoryUnavailable);
            }
            let (method, path, body) = if let Some(previous) = &t.previous {
                let current = before.ok_or(Error::PullChanged)?;
                if current.state != "open"
                    || current.merged
                    || current.draft != previous.draft
                    || current.title != previous.title
                    || current.body != previous.body
                    || current.head_revision != t.git_revision
                {
                    return Err(Error::PullChanged);
                }
                (
                    Method::PATCH,
                    format!(
                        "repos/{}/{}/pulls/{}",
                        b.owner, b.name, previous.identity.number
                    ),
                    json!({"title": t.title, "body": t.body}),
                )
            } else {
                if before.is_some() || open {
                    return Err(Error::PullChanged);
                }
                // Confirm source head before a new PR; this token only reads refs.
                let reference = self
                    .reference(b, &token, &format!("refs/heads/{}", t.head()))
                    .await?;
                if reference.map(|v| format!("git:sha1:{v}")).as_deref() != Some(&t.git_revision) {
                    return Err(Error::RefChanged);
                }
                (
                    Method::POST,
                    format!("repos/{}/{}/pulls", b.owner, b.name),
                    json!({"head": t.head(), "base": t.base, "title": t.title,
                        "body": t.body, "draft": t.draft, "maintainer_can_modify": false}),
                )
            };
            eligible().await?;
            if now()?.saturating_add(30) >= token.check_for(b.repository_id, now()?, capability)? {
                return Err(Error::TokenLifetime);
            }
            let status = if method == Method::POST {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            // Never retry this mutation. Its response may be lost after effect.
            let sent = self
                .json::<Pull>(
                    Stage::PullRequest,
                    method,
                    &path,
                    token.token.header()?,
                    Some(body),
                    status,
                )
                .await;
            if let Ok(ref p) = sent {
                p.scoped(b, t)?;
            }
            let (after, _) = self.pull_find(b, t, &token).await?;
            if let (Ok(sent), Some(after)) = (sent, after.as_ref())
                && (sent.number != after.identity.number || sent.node_id != after.identity.node_id)
            {
                return Err(Error::PullChanged);
            }
            Ok(after)
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|v| v);
        let revoked = self.revoke(&token).await;
        Ok(PullObservation {
            version: 1,
            binding: b.clone(),
            operation: t.operation,
            pull: result?,
            observed_at_unix: now()?,
            token_revoked: revoked.is_ok(),
            token_expires_at: token.expires_at.clone(),
        })
    }
}
