use super::*;
use treeharbor_github_contract::group_checks as gc;

const QUERY: &str = r"query TreeHarborGroupChecks($owner:String!,$name:String!,$oid:GitObjectID!){
 repository(owner:$owner,name:$name){databaseId object(oid:$oid){... on Commit{
 oid tree{oid} checkSuites(first:20){totalCount pageInfo{hasNextPage hasPreviousPage}
 nodes{id app{databaseId} repository{databaseId} commit{oid}
 workflowRun{id workflow{id} event runAttempt file{path repositoryName repositoryFileUrl}}
 checkRuns(first:100,filterBy:{checkType:ALL}){totalCount pageInfo{hasNextPage hasPreviousPage}
 nodes{id name status conclusion startedAt completedAt}}}}}}}}";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    has_next_page: bool,
    has_previous_page: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Items<T> {
    total_count: usize,
    page_info: Page,
    nodes: Vec<T>,
}
impl<T> Items<T> {
    fn complete(self, max: usize) -> Result<Vec<T>> {
        if self.page_info.has_next_page
            || self.page_info.has_previous_page
            || self.total_count != self.nodes.len()
            || self.total_count > max
        {
            return Err(Error::Evidence);
        }
        Ok(self.nodes)
    }
}
#[derive(Deserialize)]
struct Id {
    id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    path: String,
    repository_name: String,
    repository_file_url: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Workflow {
    id: String,
    workflow: Id,
    event: String,
    run_attempt: u32,
    file: Option<File>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    id: String,
    name: String,
    status: String,
    conclusion: Option<String>,
    started_at: Option<String>,
    completed_at: Option<String>,
}
#[derive(Deserialize)]
struct Oid {
    oid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Suite {
    id: String,
    app: Option<DatabaseId>,
    repository: DatabaseId,
    commit: Oid,
    workflow_run: Option<Workflow>,
    check_runs: Items<Run>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Subject {
    oid: String,
    tree: Tree,
    check_suites: Items<Suite>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    database_id: u64,
    object: Subject,
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
pub(super) fn timestamp(value: Option<String>) -> Result<Option<i64>> {
    value
        .map(|s| {
            if s.len() > 64 {
                return Err(Error::Evidence);
            }
            time::OffsetDateTime::parse(&s, &time::format_description::well_known::Rfc3339)
                .ok()
                .and_then(|t| i64::try_from(t.unix_timestamp_nanos() / 1_000_000).ok())
                .filter(|t| *t > 0)
                .ok_or(Error::Evidence)
        })
        .transpose()
}
fn decode(g: Graph, b: &RepositoryBinding, subject: &GitCommitObservation) -> Result<gc::Snapshot> {
    if g.errors
        .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Error::Evidence);
    }
    let r = g.data.ok_or(Error::Evidence)?.repository;
    if r.database_id != b.repository_id
        || format!("git:sha1:{}", r.object.oid) != subject.revision
        || format!("git:sha1:{}", r.object.tree.oid) != subject.tree
    {
        return Err(Error::Evidence);
    }
    let mut suites = Vec::new();
    for s in r.object.check_suites.complete(gc::MAX_SUITES)? {
        let mut runs = Vec::new();
        for r in s.check_runs.complete(gc::MAX_RUNS)? {
            runs.push(gc::Run {
                id: r.id,
                name: r.name,
                status: r.status,
                conclusion: r.conclusion,
                started_at_unix_ms: timestamp(r.started_at)?,
                completed_at_unix_ms: timestamp(r.completed_at)?,
            });
        }
        runs.sort_by(|a, b| a.id.cmp(&b.id));
        suites.push(gc::Suite {
            id: s.id,
            repository_id: s.repository.id,
            head_revision: format!("git:sha1:{}", s.commit.oid),
            app_id: s.app.map(|a| a.id),
            workflow: s.workflow_run.map(|w| gc::WorkflowRun {
                id: w.id,
                workflow_id: w.workflow.id,
                event: w.event,
                observed_run_attempt: w.run_attempt,
                file: w.file.map(|f| gc::WorkflowFile {
                    path: f.path,
                    repository_name: f.repository_name,
                    repository_file_url: f.repository_file_url,
                }),
            }),
            runs,
        });
    }
    suites.sort_by(|a, b| a.id.cmp(&b.id));
    let v = gc::Snapshot { suites };
    v.validate(b.repository_id, subject)
        .map_err(|_| Error::Evidence)?;
    Ok(v)
}
impl Connection {
    async fn group_checks(
        &self,
        b: &RepositoryBinding,
        subject: &GitCommitObservation,
        token: &wire::Token,
    ) -> Result<gc::Snapshot> {
        let g=self.json(Stage::CheckEvidence,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":QUERY,"variables":{"owner":b.owner,"name":b.name,"oid":subject.revision.strip_prefix("git:sha1:").ok_or(Error::Evidence)?}})),StatusCode::OK).await?;
        decode(g, b, subject)
    }
    /// Read a candidate explicitly linked to retained membership. The service
    /// independently validates that link when accepting this report.
    pub async fn observe_group_checks(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        selected: &gc::GroupCheckTarget,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        t.validate().map_err(|_| Error::Evidence)?;
        selected.validate().map_err(|_| Error::Evidence)?;
        if b.repository_id != t.pull.repository_id {
            return Err(Error::Evidence);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let cap = wire::Capability::Evidence;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, cap))
            .await
            .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, cap).await?;
            let before = self.group_checks(b, &selected.subject, &token).await?;
            let after = self.group_checks(b, &selected.subject, &token).await?;
            Ok::<_, Error>(gc::GroupChecks {
                subject: selected.subject.clone(),
                before,
                after,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let checks = result?;
        let observation = QueueObservation {
            version: 5,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            snapshot: None,
            group_capture: None,
            group_attempt: None,
            workflow_definition: None,
            publication: None,
            group_checks: Some(gc::GroupCheckReport {
                target: selected.clone(),
                checks,
            }),
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
