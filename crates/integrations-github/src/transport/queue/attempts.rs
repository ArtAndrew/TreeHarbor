use super::*;
use treeharbor_github_contract::{group_attempts as ga, group_checks as gc};

const QUERY: &str = r"query TreeHarborAttemptIds($workflow:ID!,$check:ID!){
 workflow:node(id:$workflow){__typename ... on WorkflowRun{id databaseId workflow{id databaseId}
 checkSuite{id databaseId repository{databaseId} commit{oid}}}}
 check:node(id:$check){__typename ... on CheckRun{id databaseId name repository{databaseId}
 checkSuite{id databaseId commit{oid}}}}}";
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Identity {
    id: String,
    database_id: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SuiteIdentity {
    id: String,
    database_id: u64,
    repository: Option<DatabaseId>,
    commit: Tree,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WorkflowIdentity {
    #[serde(rename = "__typename")]
    kind: String,
    id: String,
    database_id: u64,
    workflow: Identity,
    check_suite: SuiteIdentity,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CheckIdentity {
    #[serde(rename = "__typename")]
    kind: String,
    id: String,
    database_id: u64,
    name: String,
    repository: DatabaseId,
    check_suite: SuiteIdentity,
}
#[derive(Deserialize)]
struct Identities {
    workflow: WorkflowIdentity,
    check: CheckIdentity,
}
#[derive(Deserialize)]
struct Graph {
    data: Option<Identities>,
    errors: Option<Value>,
}
#[derive(Deserialize)]
struct Id {
    id: u64,
}
#[derive(Deserialize)]
struct Referenced {
    path: String,
    sha: String,
    #[serde(rename = "ref")]
    reference: Option<String>,
}
#[derive(Deserialize)]
struct Attempt {
    id: u64,
    node_id: String,
    workflow_id: u64,
    check_suite_id: u64,
    check_suite_node_id: String,
    repository: Id,
    head_sha: String,
    run_attempt: u32,
    event: String,
    status: String,
    conclusion: Option<String>,
    path: String,
    referenced_workflows: Option<Vec<Referenced>>,
}
impl Attempt {
    fn observation(self) -> ga::Attempt {
        let referenced_workflows = self.referenced_workflows.map(|v| {
            let mut v: Vec<_> = v
                .into_iter()
                .map(|r| ga::ReferencedWorkflow {
                    path: r.path,
                    revision: format!("git:sha1:{}", r.sha),
                    reference: r.reference,
                })
                .collect();
            v.sort_by(|a, b| a.path.cmp(&b.path));
            v
        });
        ga::Attempt {
            run_id: self.id,
            node_id: self.node_id,
            workflow_id: self.workflow_id,
            suite_id: self.check_suite_id,
            suite_node: self.check_suite_node_id,
            repository_id: self.repository.id,
            head_revision: format!("git:sha1:{}", self.head_sha),
            number: self.run_attempt,
            event: self.event,
            status: self.status.to_ascii_uppercase(),
            conclusion: self.conclusion.map(|s| s.to_ascii_uppercase()),
            path: self.path,
            referenced_workflows,
        }
    }
}
#[derive(Deserialize)]
struct Job {
    id: u64,
    node_id: String,
    run_id: u64,
    run_attempt: u32,
    head_sha: String,
    check_run_url: String,
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}
#[derive(Deserialize)]
struct Jobs {
    total_count: usize,
    jobs: Vec<Job>,
}
#[derive(Deserialize)]
struct Check {
    id: u64,
    node_id: String,
    app: Id,
    check_suite: Id,
    head_sha: String,
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}
fn check_id(b: &RepositoryBinding, s: &str) -> Result<u64> {
    let u = Url::parse(s).map_err(|_| Error::Evidence)?;
    if u.scheme() != "https"
        || u.host_str() != Some("api.github.com")
        || u.port().is_some()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || u.query().is_some()
        || u.as_str() != s
    {
        return Err(Error::Evidence);
    }
    let p: Vec<_> = u.path_segments().ok_or(Error::Evidence)?.collect();
    if p.len() != 5
        || p[0] != "repos"
        || !p[1].eq_ignore_ascii_case(&b.owner)
        || !p[2].eq_ignore_ascii_case(&b.name)
        || p[3] != "check-runs"
    {
        return Err(Error::Evidence);
    }
    let id = p[4]
        .parse::<u64>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or(Error::Evidence)?;
    if id.to_string() != p[4] {
        return Err(Error::Evidence);
    }
    Ok(id)
}
impl Connection {
    async fn attempt_ids(
        &self,
        b: &RepositoryBinding,
        t: &ga::Target,
        token: &wire::Token,
    ) -> Result<ga::Resolved> {
        let g:Graph=self.json(Stage::CheckEvidence,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":QUERY,"variables":{"workflow":t.workflow.id,"check":t.check.id}})),StatusCode::OK).await?;
        if g.errors
            .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(Error::Evidence);
        }
        let g = g.data.ok_or(Error::Evidence)?;
        let w = g.workflow;
        let c = g.check;
        if w.kind != "WorkflowRun"
            || c.kind != "CheckRun"
            || w.id != t.workflow.id
            || w.workflow.id != t.workflow.workflow_id
            || c.id != t.check.id
            || c.name != t.check.name
            || c.repository.id != b.repository_id
            || w.check_suite.repository.as_ref().map(|r| r.id) != Some(b.repository_id)
            || w.check_suite.id != t.suite_id
            || c.check_suite.id != t.suite_id
            || c.check_suite.database_id != w.check_suite.database_id
            || format!("git:sha1:{}", w.check_suite.commit.oid) != t.subject.revision
            || format!("git:sha1:{}", c.check_suite.commit.oid) != t.subject.revision
            || [
                c.database_id,
                w.database_id,
                w.workflow.database_id,
                w.check_suite.database_id,
            ]
            .contains(&0)
        {
            return Err(Error::Evidence);
        }
        Ok(ga::Resolved {
            check_id: c.database_id,
            run_id: w.database_id,
            workflow_id: w.workflow.database_id,
            suite_id: w.check_suite.database_id,
        })
    }
    async fn attempt_json<T: DeserializeOwned>(
        &self,
        path: &str,
        token: &wire::Token,
    ) -> Result<T> {
        let (_, headers, bytes) = self
            .request_headers(
                Stage::CheckEvidence,
                Method::GET,
                path,
                token.token.header()?,
                None,
                &[StatusCode::OK],
            )
            .await?;
        if headers.contains_key(header::LINK) {
            return Err(Error::Evidence);
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Evidence)
    }
    async fn attempt_snapshot(
        &self,
        b: &RepositoryBinding,
        t: &ga::Target,
        ids: &ga::Resolved,
        token: &wire::Token,
    ) -> Result<ga::Snapshot> {
        let prefix = format!("repos/{}/{}", b.owner, b.name);
        let path = format!(
            "{prefix}/actions/runs/{}/attempts/{}",
            ids.run_id, t.workflow.observed_run_attempt
        );
        let selected: Attempt = self.attempt_json(&path, token).await?;
        let jobs: Jobs = self
            .attempt_json(&format!("{path}/jobs?per_page=100"), token)
            .await?;
        if jobs.total_count != jobs.jobs.len() || jobs.total_count > ga::MAX_JOBS {
            return Err(Error::Evidence);
        }
        let mut decoded = Vec::new();
        for j in jobs.jobs {
            decoded.push(ga::Job {
                id: j.id,
                node_id: j.node_id,
                run_id: j.run_id,
                attempt: j.run_attempt,
                head_revision: format!("git:sha1:{}", j.head_sha),
                check_run_id: check_id(b, &j.check_run_url)?,
                name: j.name,
                status: j.status.to_ascii_uppercase(),
                conclusion: j.conclusion.map(|s| s.to_ascii_uppercase()),
                started_at_unix_ms: super::checks::timestamp(j.started_at)?,
                completed_at_unix_ms: super::checks::timestamp(j.completed_at)?,
            });
        }
        decoded.sort_by_key(|j| j.id);
        let c: Check = self
            .attempt_json(&format!("{prefix}/check-runs/{}", ids.check_id), token)
            .await?;
        let current: Attempt = self
            .attempt_json(&format!("{prefix}/actions/runs/{}", ids.run_id), token)
            .await?;
        Ok(ga::Snapshot {
            selected: selected.observation(),
            current: current.observation(),
            jobs: decoded,
            check: ga::Check {
                database_id: c.id,
                app_id: c.app.id,
                suite_id: c.check_suite.id,
                head_revision: format!("git:sha1:{}", c.head_sha),
                run: gc::Run {
                    id: c.node_id,
                    name: c.name,
                    status: c.status.to_ascii_uppercase(),
                    conclusion: c.conclusion.map(|s| s.to_ascii_uppercase()),
                    started_at_unix_ms: super::checks::timestamp(c.started_at)?,
                    completed_at_unix_ms: super::checks::timestamp(c.completed_at)?,
                },
            },
        })
    }
    pub async fn observe_group_attempt(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        selected: &ga::Target,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        t.validate().map_err(|_| Error::Evidence)?;
        selected.validate(t).map_err(|_| Error::Evidence)?;
        if b.repository_id != t.pull.repository_id {
            return Err(Error::Evidence);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let cap = wire::Capability::AttemptEvidence;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, cap))
            .await
            .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, cap).await?;
            let resolved = self.attempt_ids(b, selected, &token).await?;
            let before = self
                .attempt_snapshot(b, selected, &resolved, &token)
                .await?;
            let after = self
                .attempt_snapshot(b, selected, &resolved, &token)
                .await?;
            Ok::<_, Error>(ga::Report {
                target: selected.clone(),
                resolved,
                before,
                after,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let report = result?;
        let observation = QueueObservation {
            version: 6,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            group_attempt: Some(report),
            workflow_definition: None,
            group_checks: None,
            group_capture: None,
            snapshot: None,
            receipt: None,
            enqueue_receipt: None,
            async_result: None,
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
