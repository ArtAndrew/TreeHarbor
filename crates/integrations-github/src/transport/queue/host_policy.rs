//! Read-only complete policy/review observations. No configuration or merge writes.
use super::*;
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_github_contract::{host_policy as hp, publication};

const REVIEW_QUERY: &str = r"query TreeHarborPolicyReviews($owner:String!,$name:String!,$number:Int!,$after:String){
 repository(owner:$owner,name:$name){databaseId pullRequest(number:$number){
 id number headRefName headRefOid baseRefName baseRefOid state isDraft merged
 headRepository{databaseId} baseRepository{databaseId} reviewDecision
 reviewThreads(first:100,after:$after){totalCount pageInfo{hasNextPage hasPreviousPage endCursor}
 nodes{id isResolved isOutdated}}}}}";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    has_next_page: bool,
    has_previous_page: bool,
    end_cursor: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Thread {
    id: String,
    is_resolved: bool,
    is_outdated: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Threads {
    total_count: usize,
    page_info: Page,
    nodes: Vec<Thread>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    id: String,
    number: u64,
    head_ref_name: String,
    head_ref_oid: String,
    base_ref_name: String,
    base_ref_oid: String,
    state: String,
    is_draft: bool,
    merged: bool,
    head_repository: DatabaseId,
    base_repository: DatabaseId,
    #[serde(deserialize_with = "nullable_decision")]
    review_decision: Option<String>,
    review_threads: Threads,
}
fn nullable_decision<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<String>, D::Error> {
    Option::<String>::deserialize(d)
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    database_id: u64,
    pull_request: Pull,
}
#[derive(Deserialize)]
struct Data {
    repository: Repository,
}
#[derive(Deserialize)]
struct Graph {
    data: Option<Data>,
    errors: Option<Value>,
}

impl Connection {
    async fn host_document(&self, path: &str, token: &wire::Token) -> Result<hp::Document> {
        // Do not store provider error bodies or turn inaccessible documents into {}.
        match self
            .json(
                Stage::CheckEvidence,
                Method::GET,
                path,
                token.token.header()?,
                None,
                StatusCode::OK,
            )
            .await
        {
            Ok(value) => Ok(hp::Document {
                status: 200,
                value: Some(value),
            }),
            Err(Error::Http {
                status: status @ (403 | 404),
                ..
            }) => Ok(hp::Document {
                status,
                value: None,
            }),
            Err(e) => Err(e),
        }
    }
    async fn host_rules(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        token: &wire::Token,
    ) -> Result<(Vec<Value>, Vec<hp::Ruleset>)> {
        let base = t.pull.base_ref.replace('/', "%2F");
        let repository = format!("{}/{}", b.owner, b.name);
        let mut all = Vec::new();
        let mut sources = BTreeMap::new();
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
            for r in rules {
                let s = hp::RuleSource::from_rule(&r, &repository).map_err(|_| Error::Evidence)?;
                if sources.insert(s.id, s.clone()).is_some_and(|old| old != s) {
                    return Err(Error::Evidence);
                }
                all.push(r);
            }
            // Detect duplicate rules, inconsistent sources and bounds before GET details.
            hp::rules_fingerprint(&all).map_err(|_| Error::Evidence)?;
            if sources.len() > hp::MAX_RULESETS {
                return Err(Error::Evidence);
            }
            if end {
                all.sort_by_key(|v| serde_json::to_string(v).unwrap_or_default());
                let mut sets = Vec::new();
                for source in sources.into_values() {
                    let document = self
                        .host_document(
                            &format!(
                                "repos/{}/{}/rulesets/{}?includes_parents=true",
                                b.owner, b.name, source.id
                            ),
                            token,
                        )
                        .await?;
                    sets.push(hp::Ruleset { source, document });
                }
                return Ok((all, sets));
            }
        }
        Err(Error::Evidence)
    }
    async fn host_review(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        token: &wire::Token,
    ) -> Result<hp::Review> {
        let mut after: Option<String> = None;
        let mut cursors = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut identity = None;
        let mut threads = Vec::new();
        for _ in 0..10 {
            let g: Graph = self.json(Stage::CheckEvidence,Method::POST,"graphql",token.token.header()?,
                Some(json!({"query":REVIEW_QUERY,"variables":{"owner":b.owner,"name":b.name,"number":t.pull.pull_number,"after":after}})),StatusCode::OK).await?;
            if g.errors
                .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
            {
                return Err(Error::Evidence);
            }
            let r = g.data.ok_or(Error::Evidence)?.repository;
            let p = r.pull_request;
            if r.database_id != b.repository_id
                || p.head_repository.id != b.repository_id
                || p.base_repository.id != b.repository_id
            {
                return Err(Error::Identity);
            }
            let state = match p.state.as_str() {
                "OPEN" => "open",
                "CLOSED" | "MERGED" => "closed",
                _ => return Err(Error::Evidence),
            };
            if (p.state == "MERGED") != p.merged {
                return Err(Error::Evidence);
            }
            let pull = PullState {
                repository_id: r.database_id,
                number: p.number,
                node_id: p.id,
                head_ref: p.head_ref_name,
                base_ref: p.base_ref_name,
                head_revision: format!("git:sha1:{}", p.head_ref_oid),
                base_revision: format!("git:sha1:{}", p.base_ref_oid),
                state: state.into(),
                draft: p.is_draft,
                merged: p.merged,
            };
            pull.validate(&t.pull).map_err(|_| Error::Evidence)?;
            let page = p.review_threads;
            let current = (pull.clone(), p.review_decision.clone(), page.total_count);
            if identity.as_ref().is_some_and(|old| old != &current)
                || page.total_count > hp::MAX_THREADS
                || page.nodes.len() > 100
                || (after.is_none() && page.page_info.has_previous_page)
            {
                return Err(Error::Evidence);
            }
            identity = Some(current);
            for node in page.nodes {
                if !ids.insert(node.id.clone()) {
                    return Err(Error::Evidence);
                }
                threads.push(hp::ReviewThread {
                    id: node.id,
                    resolved: node.is_resolved,
                    outdated: node.is_outdated,
                });
            }
            if !page.page_info.has_next_page {
                if threads.len() != page.total_count {
                    return Err(Error::Evidence);
                }
                threads.sort_by(|a, b| a.id.cmp(&b.id));
                return Ok(hp::Review {
                    pull,
                    decision: p.review_decision,
                    threads,
                });
            }
            let cursor = page.page_info.end_cursor.ok_or(Error::Evidence)?;
            if cursor.is_empty()
                || cursor.len() > 1024
                || cursor.chars().any(char::is_control)
                || !cursors.insert(cursor.clone())
                || threads.len() >= page.total_count
            {
                return Err(Error::Evidence);
            }
            after = Some(cursor);
        }
        Err(Error::Evidence)
    }
    async fn host_snapshot(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        token: &wire::Token,
    ) -> Result<hp::Snapshot> {
        let group = self.group_snapshot(b, t, token).await?;
        let base = t.pull.base_ref.replace('/', "%2F");
        let protection = self
            .host_document(
                &format!("repos/{}/{}/branches/{base}/protection", b.owner, b.name),
                token,
            )
            .await?;
        let (rules, rulesets) = self.host_rules(b, t, token).await?;
        let review = self.host_review(b, t, token).await?;
        let snapshot = hp::Snapshot {
            group,
            protection,
            rules,
            rulesets,
            review,
        };
        snapshot
            .validate(&format!("{}/{}", b.owner, b.name), t)
            .map_err(|_| Error::Evidence)?;
        Ok(snapshot)
    }
    /// Capture both full rounds even on policy drift. Token permissions remain
    /// read-only; redacted bypass metadata stays explicitly unproven.
    pub async fn observe_host_policy(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        target: &publication::Target,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        t.validate().map_err(|_| Error::Evidence)?;
        target.validate(t).map_err(|_| Error::Evidence)?;
        if b.repository_id != t.pull.repository_id || t.pull.pull_number > i32::MAX as u64 {
            return Err(Error::Identity);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let cap = wire::Capability::HostPolicy;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, cap))
            .await
            .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, cap).await?;
            let before = self.host_snapshot(b, t, &token).await?;
            let after = self.host_snapshot(b, t, &token).await?;
            Ok::<_, Error>(hp::Report {
                version: 1,
                repository_name: format!("{}/{}", b.owner, b.name),
                target: target.clone(),
                before,
                after,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let observation = QueueObservation {
            version: 9,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            host_policy: Some(Box::new(result?)),
            snapshot: None,
            receipt: None,
            async_result: None,
            enqueue_receipt: None,
            group_capture: None,
            group_checks: None,
            group_attempt: None,
            workflow_definition: None,
            publication: None,
            mutation_not_sent: false,
            token_revoked: revoked,
        };
        observation.validate(t).map_err(|_| Error::Evidence)?;
        Ok(ObservedQueue {
            binding: b.clone(),
            observation,
            token_expires_at: token.expires_at.clone(),
        })
    }
}
