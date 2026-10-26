//! One-shot exact check creation and read-only recovery. No branch/queue writes.
use super::*;
use crate::MutationFailure;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::sync::atomic::{AtomicBool, Ordering};
use treeharbor_github_contract::{RequiredCheck, check_publication::*};

pub struct ObservedCheckPublication {
    pub binding: RepositoryBinding,
    pub report: Report,
    pub token_expires_at: String,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct App {
    id: u64,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
struct Run {
    id: u64,
    node_id: String,
    app: App,
    name: String,
    head_sha: String,
    external_id: Option<String>,
    status: String,
    conclusion: Option<String>,
}
#[derive(Deserialize)]
struct Runs {
    total_count: u64,
    check_runs: Vec<Run>,
}
impl Run {
    fn checked(self, t: &Target) -> Result<PublishedCheck> {
        let c = PublishedCheck {
            id: self.id,
            node_id: Some(self.node_id),
            app_id: self.app.id,
            name: self.name,
            head_revision: format!("git:sha1:{}", self.head_sha),
            external_id: self.external_id.ok_or(Error::Evidence)?,
            status: self.status,
            conclusion: serde_json::from_value(json!(self.conclusion.ok_or(Error::Evidence)?))
                .map_err(|_| Error::Evidence)?,
        };
        c.validate(t).map_err(|_| Error::Evidence)?;
        Ok(c)
    }
}
fn protection(v: Value) -> Result<Protection> {
    let required = v["required_status_checks"]["checks"]
        .as_array()
        .ok_or(Error::Evidence)?;
    if required.len() > 128 {
        return Err(Error::Evidence);
    }
    let mut app_bound_checks = Vec::new();
    let mut names = std::collections::BTreeSet::new();
    for c in required {
        let name = c["context"]
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 256 && !s.chars().any(char::is_control))
            .ok_or(Error::Evidence)?;
        if !names.insert(name) {
            return Err(Error::Evidence);
        }
        // Other checks may intentionally accept any App. Only an explicit
        // positive App ID can prove this publication's required context.
        if c["app_id"].is_null() || c["app_id"] == -1 {
            continue;
        }
        let app_id = c["app_id"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= i64::MAX as u64)
            .ok_or(Error::Evidence)?;
        app_bound_checks.push(RequiredCheck {
            name: name.to_owned(),
            app_id,
        });
    }
    app_bound_checks.sort();
    let mut has_bypass = false;
    let allowances = &v["required_pull_request_reviews"]["bypass_pull_request_allowances"];
    if !allowances.is_null() {
        for kind in ["users", "teams", "apps"] {
            has_bypass |= !allowances[kind]
                .as_array()
                .ok_or(Error::Evidence)?
                .is_empty();
        }
    }
    let sha256 = treeharbor_core::ObjectId::from_bytes(
        Sha256::digest(serde_json::to_vec(&v).map_err(|_| Error::Evidence)?).into(),
    )
    .to_string();
    Ok(Protection {
        sha256,
        app_bound_checks,
        enforce_admins: v["enforce_admins"]["enabled"]
            .as_bool()
            .ok_or(Error::Evidence)?,
        allow_force_pushes: v["allow_force_pushes"]["enabled"]
            .as_bool()
            .ok_or(Error::Evidence)?,
        allow_deletions: v["allow_deletions"]["enabled"]
            .as_bool()
            .ok_or(Error::Evidence)?,
        has_bypass,
    })
}
impl Connection {
    async fn check_source(
        &self,
        b: &RepositoryBinding,
        t: &Target,
        token: &wire::Token,
    ) -> Result<SourceView> {
        match &t.source {
            Source::Pull { target } => Ok(SourceView::Pull(
                self.evidence_pull(b, target, token).await?,
            )),
            Source::Group { queue, .. } => Ok(SourceView::Group(Box::new(
                self.group_snapshot(b, queue, token).await?,
            ))),
        }
    }
    async fn check_policy(
        &self,
        b: &RepositoryBinding,
        t: &Target,
        token: &wire::Token,
    ) -> Result<Policy> {
        let base = t.source.pull().base_ref.replace('/', "%2F");
        let v: Value = self
            .json(
                Stage::CheckPublication,
                Method::GET,
                &format!("repos/{}/{}/branches/{base}/protection", b.owner, b.name),
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        Ok(Policy {
            protection: protection(v)?,
            rules: self.evidence_rules(b, t.source.pull(), token).await?,
        })
    }
    /// The caller must durably fence dispatch and retain the preflight in this
    /// callback. An uncertain POST is never retried by this transport.
    pub async fn publish_check<F, Fut>(
        &self,
        b: &RepositoryBinding,
        t: &Target,
        before_write: F,
    ) -> std::result::Result<ObservedCheckPublication, MutationFailure>
    where
        F: FnOnce(Preflight) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let sent = AtomicBool::new(false);
        let result = self.publish_check_inner(b, t, before_write, &sent).await;
        result.map_err(|error| MutationFailure {
            error,
            mutation_not_sent: !sent.load(Ordering::SeqCst),
        })
    }
    async fn publish_check_inner<F, Fut>(
        &self,
        b: &RepositoryBinding,
        t: &Target,
        before_write: F,
        sent: &AtomicBool,
    ) -> Result<ObservedCheckPublication>
    where
        F: FnOnce(Preflight) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        self.check_target(b, t)?;
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let capability = wire::Capability::CheckWrite;
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, capability))
                .await
                .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, capability).await?;
            let before = self.check_source(b, t, &token).await?;
            let policy_before = self.check_policy(b, t, &token).await?;
            let after = self.check_source(b, t, &token).await?;
            let policy_after = self.check_policy(b, t, &token).await?;
            let preflight = Preflight { before, policy_before, after, policy_after };
            if !preflight.eligible(t) {
                return Err(Error::PublicationStopped);
            }
            before_write(preflight.clone()).await?;
            let sha = t.source.head().map_err(|_| Error::Evidence)?
                .strip_prefix("git:sha1:").ok_or(Error::Evidence)?;
            let body = json!({
                "name": t.name().map_err(|_| Error::Evidence)?,
                "head_sha": sha,
                "external_id": t.external_id().map_err(|_| Error::Evidence)?,
                "status": "completed",
                "conclusion": t.conclusion,
                "output": {
                    "title": "TreeHarbor managed check",
                    "summary": format!("Execution evidence: {}\nProgram: {}\nAuthority epoch: {}", t.evidence, t.program, t.epoch)
                }
            });
            let header = token.token.header()?;
            sent.store(true, Ordering::SeqCst);
            let run: Run = self.json(
                Stage::CheckPublication, Method::POST,
                &format!("repos/{}/{}/check-runs", b.owner, b.name),
                header, Some(body), StatusCode::CREATED,
            ).await?;
            let run = run.checked(t)?;
            let observed: Run = self.json(
                Stage::CheckPublication, Method::GET,
                &format!("repos/{}/{}/check-runs/{}", b.owner, b.name, run.id),
                token.token.header()?, None, StatusCode::OK,
            ).await?;
            let observed = observed.checked(t)?;
            if run != observed {
                return Err(Error::Evidence);
            }
            Ok::<_, Error>((preflight, observed))
        }).await.map_err(|_| Error::Deadline).and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let (preflight, check) = result?;
        let report = Report {
            version: 2,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            outcome: Outcome::Observed,
            preflight: Some(preflight),
            check: Some(check),
            token_revoked: revoked,
        };
        report.validate(t).map_err(|_| Error::Evidence)?;
        Ok(ObservedCheckPublication {
            binding: b.clone(),
            report,
            token_expires_at: token.expires_at.clone(),
        })
    }
    fn check_target(&self, b: &RepositoryBinding, t: &Target) -> Result<()> {
        b.validate()?;
        t.validate().map_err(|_| Error::Evidence)?;
        if b.repository_id != t.source.pull().repository_id
            || t.app_id != self.credentials.app_id
            || t.source.pull().pull_number > i32::MAX as u64
        {
            return Err(Error::Evidence);
        }
        Ok(())
    }
    async fn check_scan(
        &self,
        b: &RepositoryBinding,
        t: &Target,
        token: &wire::Token,
    ) -> Result<Vec<Run>> {
        let sha = t
            .source
            .head()
            .map_err(|_| Error::Evidence)?
            .strip_prefix("git:sha1:")
            .ok_or(Error::Evidence)?;
        let name = t.name().map_err(|_| Error::Evidence)?.replace('/', "%2F");
        let mut all = Vec::new();
        let mut total = None;
        let mut ids = std::collections::BTreeSet::new();
        let mut nodes = std::collections::BTreeSet::new();
        for page in 1..=51 {
            let r: Runs = self.json(
                Stage::CheckPublication, Method::GET,
                &format!("repos/{}/{}/commits/{sha}/check-runs?check_name={name}&filter=all&per_page=20&page={page}", b.owner, b.name),
                token.token.header()?, None, StatusCode::OK,
            ).await?;
            if r.total_count > 1000
                || r.check_runs.len() > 20
                || total.is_some_and(|n| n != r.total_count)
            {
                return Err(Error::Evidence);
            }
            total = Some(r.total_count);
            let end = r.check_runs.len() < 20;
            for run in r.check_runs {
                if run.id == 0
                    || run.id > i64::MAX as u64
                    || !ids.insert(run.id)
                    || run.node_id.is_empty()
                    || run.node_id.len() > 256
                    || !run.node_id.is_ascii()
                    || run
                        .node_id
                        .bytes()
                        .any(|b| b.is_ascii_control() || b.is_ascii_whitespace())
                    || !nodes.insert(run.node_id.clone())
                {
                    return Err(Error::Evidence);
                }
                all.push(run);
            }
            if all.len() as u64 > r.total_count {
                return Err(Error::Evidence);
            }
            if end || all.len() as u64 == r.total_count {
                if all.len() as u64 != r.total_count {
                    return Err(Error::Evidence);
                }
                all.sort_by_key(|r| r.id);
                return Ok(all);
            }
        }
        Err(Error::Evidence)
    }
    /// Does not create/update a check. Stable absence stays unknown, never not-sent.
    pub async fn observe_published_check(
        &self,
        b: &RepositoryBinding,
        t: &Target,
        preflight: Option<Preflight>,
    ) -> Result<ObservedCheckPublication> {
        self.check_target(b, t)?;
        if let Some(p) = &preflight {
            p.validate(t).map_err(|_| Error::Evidence)?;
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let capability = wire::Capability::Evidence;
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, capability))
                .await
                .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, capability)
                .await?;
            let before = self.check_scan(b, t, &token).await?;
            let after = self.check_scan(b, t, &token).await?;
            if before != after {
                return Err(Error::Evidence);
            }
            let external = t.external_id().map_err(|_| Error::Evidence)?;
            let mut found = None;
            for r in after {
                if r.app.id == t.app_id && r.external_id.as_deref() == Some(external.as_str()) {
                    if found.is_some() {
                        return Err(Error::Evidence);
                    }
                    found = Some(r.checked(t)?);
                }
            }
            Ok::<_, Error>(found)
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let check = result?;
        let report = Report {
            version: 2,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            outcome: if check.is_some() {
                Outcome::Observed
            } else {
                Outcome::Unknown
            },
            preflight,
            check,
            token_revoked: revoked,
        };
        report.validate(t).map_err(|_| Error::Evidence)?;
        Ok(ObservedCheckPublication {
            binding: b.clone(),
            report,
            token_expires_at: token.expires_at.clone(),
        })
    }
}
