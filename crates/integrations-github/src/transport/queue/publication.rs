use super::*;
use treeharbor_github_contract::publication as p;

const QUERY: &str = r"query TreeHarborPublication($owner:String!,$name:String!,$number:Int!,$base:String!){
 repository(owner:$owner,name:$name){databaseId
 ref(qualifiedName:$base){name prefix target{__typename ... on Commit{oid tree{oid}}}}
 pullRequest(number:$number){id number headRefName headRefOid baseRefName baseRefOid state isDraft merged
 headRepository{databaseId} baseRepository{databaseId}
 mergeCommit{oid tree{oid} parents(first:8){totalCount pageInfo{hasNextPage hasPreviousPage} nodes{oid tree{oid}}}}}}}";
#[derive(Deserialize)]
struct Main {
    name: String,
    prefix: String,
    target: TypedCommit,
}
#[derive(Deserialize)]
struct TypedCommit {
    #[serde(rename = "__typename")]
    kind: String,
    #[serde(flatten)]
    commit: Commit,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinalPull {
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
    merge_commit: Option<groups::ParentCommit>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FinalRepository {
    database_id: u64,
    #[serde(rename = "ref")]
    main: Main,
    pull_request: FinalPull,
}
#[derive(Deserialize)]
struct FinalData {
    repository: FinalRepository,
}
#[derive(Deserialize)]
struct FinalGraph {
    data: Option<FinalData>,
    errors: Option<Value>,
}
#[derive(Deserialize)]
struct RestTree {
    sha: String,
}
#[derive(Deserialize)]
struct RestContent {
    tree: RestTree,
}
#[derive(Deserialize)]
struct RestCommit {
    sha: String,
    commit: RestContent,
}
impl RestCommit {
    fn observation(self) -> GitCommitObservation {
        GitCommitObservation {
            revision: format!("git:sha1:{}", self.sha),
            tree: format!("git:sha1:{}", self.commit.tree.sha),
        }
    }
}
#[derive(Deserialize)]
struct Compare {
    base_commit: RestCommit,
    merge_base_commit: RestCommit,
    status: String,
    ahead_by: u32,
    behind_by: u32,
    total_commits: u32,
}
impl Connection {
    async fn publication_snapshot(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        token: &wire::Token,
    ) -> Result<p::Snapshot> {
        let g:FinalGraph=self.json(Stage::Queue,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":QUERY,"variables":{"owner":b.owner,"name":b.name,"number":t.pull.pull_number,"base":format!("refs/heads/{}",t.pull.base_ref)}})),StatusCode::OK).await?;
        if g.errors
            .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(Error::Evidence);
        }
        let r = g.data.ok_or(Error::Evidence)?.repository;
        let pull = r.pull_request;
        if r.database_id != b.repository_id
            || pull.head_repository.id != b.repository_id
            || pull.base_repository.id != b.repository_id
            || r.main.name != t.pull.base_ref
            || r.main.prefix != "refs/heads/"
            || r.main.target.kind != "Commit"
        {
            return Err(Error::Evidence);
        }
        let state = match pull.state.as_str() {
            "OPEN" => "open",
            "CLOSED" | "MERGED" => "closed",
            _ => return Err(Error::Evidence),
        };
        if (pull.state == "MERGED") != pull.merged {
            return Err(Error::Evidence);
        }
        let snapshot = p::Snapshot {
            pull: PullState {
                repository_id: b.repository_id,
                number: pull.number,
                node_id: pull.id,
                head_ref: pull.head_ref_name,
                base_ref: pull.base_ref_name,
                head_revision: format!("git:sha1:{}", pull.head_ref_oid),
                base_revision: format!("git:sha1:{}", pull.base_ref_oid),
                state: state.into(),
                draft: pull.is_draft,
                merged: pull.merged,
            },
            main: r.main.target.commit.observation(),
            final_commit: pull
                .merge_commit
                .map(groups::ParentCommit::observation)
                .transpose()?,
        };
        snapshot
            .pull
            .validate(&t.pull)
            .map_err(|_| Error::Evidence)?;
        snapshot.main.validate().map_err(|_| Error::Evidence)?;
        Ok(snapshot)
    }
    /// Immutable comparison plus bracketed PR/target reads; no mutation or CI approval.
    pub async fn observe_publication(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        selected: &p::Target,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        selected.validate(t).map_err(|_| Error::Evidence)?;
        if b.repository_id != t.pull.repository_id || t.pull.pull_number > i32::MAX as u64 {
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
            let before = self.publication_snapshot(b, t, &token).await?;
            let base = selected
                .candidate()
                .map_err(|_| Error::Evidence)?
                .commit
                .revision
                .strip_prefix("git:sha1:")
                .ok_or(Error::Evidence)?;
            let head = before
                .main
                .revision
                .strip_prefix("git:sha1:")
                .ok_or(Error::Evidence)?;
            // Page one preserves comparison summary; never infer completeness from its commit list.
            // Fixed immutable SHA path; provider URLs and Link headers are not followed.
            let route = format!(
                "repos/{}/{}/compare/{base}...{head}?per_page=1&page=1",
                b.owner, b.name
            );
            let c: Compare = self
                .json(
                    Stage::Queue,
                    Method::GET,
                    &route,
                    token.token.header()?,
                    None,
                    StatusCode::OK,
                )
                .await?;
            let comparison = p::Comparison {
                base: c.base_commit.observation(),
                head_revision: before.main.revision.clone(),
                merge_base: c.merge_base_commit.observation(),
                status: c.status,
                ahead_by: c.ahead_by,
                behind_by: c.behind_by,
                total_commits: c.total_commits,
            };
            let after = self.publication_snapshot(b, t, &token).await?;
            Ok::<_, Error>(p::Report {
                target: selected.clone(),
                before,
                comparison,
                after,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let observation = QueueObservation {
            version: 8,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            publication: Some(result?),
            snapshot: None,
            receipt: None,
            async_result: None,
            enqueue_receipt: None,
            group_capture: None,
            group_checks: None,
            group_attempt: None,
            workflow_definition: None,
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
