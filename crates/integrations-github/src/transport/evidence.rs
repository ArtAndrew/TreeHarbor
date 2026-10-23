use super::*;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use treeharbor_github_contract::{
    CheckResult, CheckRun, InspectionReport, InspectionTarget, PullState, RuleObservation,
};

pub struct ObservedInspection {
    pub binding: RepositoryBinding,
    pub report: InspectionReport,
    pub token_expires_at: String,
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
    head: Side,
    base: Side,
    state: String,
    draft: bool,
    merged: bool,
}
#[derive(Deserialize)]
struct App {
    id: u64,
}
#[derive(Deserialize)]
struct Run {
    id: u64,
    app: App,
    name: String,
    head_sha: String,
    status: String,
    conclusion: Option<String>,
}
#[derive(Deserialize)]
struct Runs {
    total_count: u64,
    check_runs: Vec<Run>,
}

impl Connection {
    async fn evidence_pull(
        &self,
        b: &RepositoryBinding,
        t: &InspectionTarget,
        token: &wire::Token,
    ) -> Result<PullState> {
        let p: Pull = self
            .json(
                Stage::CheckEvidence,
                Method::GET,
                &format!("repos/{}/{}/pulls/{}", b.owner, b.name, t.pull_number),
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await?;
        if p.head.repo.id != b.repository_id || p.base.repo.id != b.repository_id {
            return Err(Error::Evidence);
        }
        let p = PullState {
            repository_id: b.repository_id,
            number: p.number,
            node_id: p.node_id,
            head_ref: p.head.name,
            base_ref: p.base.name,
            head_revision: format!("git:sha1:{}", p.head.sha),
            base_revision: format!("git:sha1:{}", p.base.sha),
            state: p.state,
            draft: p.draft,
            merged: p.merged,
        };
        p.validate(t).map_err(|_| Error::Evidence)?;
        Ok(p)
    }
    pub(super) async fn evidence_rules(
        &self,
        b: &RepositoryBinding,
        t: &InspectionTarget,
        token: &wire::Token,
    ) -> Result<RuleObservation> {
        let mut all = Vec::new();
        let mut identities = std::collections::BTreeSet::new();
        let mut queue = false;
        // Rules apply even to absent branches. PR reads separately establish its
        // exact base; this snapshot intentionally excludes legacy protection.
        // Target validation restricts all other characters to URL-safe ASCII.
        let base = t.base_ref.replace('/', "%2F");
        for page in 1..=11 {
            let rules: Vec<Value> = self
                .json(
                    Stage::CheckEvidence,
                    Method::GET,
                    &format!(
                        "repos/{}/{}/rules/branches/{base}?per_page=10&page={page}",
                        b.owner, b.name
                    ),
                    token.token.header()?,
                    None,
                    StatusCode::OK,
                )
                .await?;
            if rules.len() > 10 {
                return Err(Error::Evidence);
            }
            let end = rules.len() < 10;
            for rule in rules {
                let id = rule["ruleset_id"]
                    .as_u64()
                    .filter(|n| *n > 0)
                    .ok_or(Error::Evidence)?;
                let kind = rule["type"]
                    .as_str()
                    .filter(|s| {
                        !s.is_empty()
                            && s.len() <= 64
                            && s.bytes().all(|c| c.is_ascii_lowercase() || c == b'_')
                    })
                    .ok_or(Error::Evidence)?;
                if !identities.insert((id, kind.to_owned())) {
                    return Err(Error::Evidence);
                }
                queue |= kind == "merge_queue";
                let encoded = serde_json::to_string(&rule).map_err(|_| Error::Evidence)?;
                if encoded.len() > 16384 {
                    return Err(Error::Evidence);
                }
                all.push(encoded);
            }
            if all.len() > 100 {
                return Err(Error::Evidence);
            }
            if end {
                all.sort();
                let mut hash = Sha256::new();
                hash.update(b"treeharbor-github-rules-v1\0");
                for value in &all {
                    hash.update((value.len() as u64).to_be_bytes());
                    hash.update(value.as_bytes());
                }
                return Ok(RuleObservation {
                    sha256: hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
                    count: all.len() as u32,
                    merge_queue: queue,
                });
            }
        }
        Err(Error::Evidence)
    }
    /// Capture exact selected check runs and active rulesets. No admission or
    /// provider write; current permissions are checked before and after capture.
    pub async fn inspect_pull<G, F>(
        &self,
        b: &RepositoryBinding,
        t: &InspectionTarget,
        mut eligible: G,
    ) -> Result<ObservedInspection>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        b.validate()?;
        t.validate().map_err(|_| Error::Evidence)?;
        if b.repository_id != t.repository_id {
            return Err(Error::Evidence);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, async {
            eligible().await?;
            self.mint_for(b, wire::Capability::Evidence).await
        })
        .await
        .map_err(|_| Error::Deadline)??;
        let observed = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, wire::Capability::Evidence)
                .await?;
            let before = self.evidence_pull(b, t, &token).await?;
            let rules_before = self.evidence_rules(b, t, &token).await?;
            let mut checks = Vec::new();
            for required in &t.required {
                let path = format!(
                    "repos/{}/{}/commits/{}/check-runs",
                    b.owner,
                    b.name,
                    t.head_revision
                        .strip_prefix("git:sha1:")
                        .ok_or(Error::Evidence)?
                );
                let mut url = self.base.join(&path).map_err(|_| Error::Configuration)?;
                url.query_pairs_mut()
                    .append_pair("check_name", &required.name)
                    .append_pair("app_id", &required.app_id.to_string())
                    .append_pair("filter", "latest")
                    .append_pair("per_page", "2");
                let runs: Runs = self
                    .json(
                        Stage::CheckEvidence,
                        Method::GET,
                        &format!("{path}?{}", url.query().ok_or(Error::Evidence)?),
                        token.token.header()?,
                        None,
                        StatusCode::OK,
                    )
                    .await?;
                checks.push(CheckResult {
                    required: required.clone(),
                    total_count: runs.total_count,
                    runs: runs
                        .check_runs
                        .into_iter()
                        .map(|r| CheckRun {
                            id: r.id,
                            app_id: r.app.id,
                            name: r.name,
                            head_revision: format!("git:sha1:{}", r.head_sha),
                            status: r.status,
                            conclusion: r.conclusion,
                        })
                        .collect(),
                });
            }
            let rules_after = self.evidence_rules(b, t, &token).await?;
            let after = self.evidence_pull(b, t, &token).await?;
            eligible().await?;
            Ok(InspectionReport {
                version: 1,
                started_at_unix_ms: started,
                finished_at_unix_ms: now()? as i64 * 1000,
                before,
                after,
                rules_before,
                rules_after,
                checks,
                token_revoked: false,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|v| v);
        let revoked = self.revoke(&token).await;
        let mut report: InspectionReport = observed?;
        report.token_revoked = revoked.is_ok();
        report.validate(t).map_err(|_| Error::Evidence)?;
        Ok(ObservedInspection {
            binding: b.clone(),
            report,
            token_expires_at: token.expires_at.clone(),
        })
    }
}
