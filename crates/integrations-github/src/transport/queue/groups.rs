//! Bounded, read-scoped capture of queue membership and immutable parent links.
use super::*;
use treeharbor_github_contract::groups::*;

const GROUP_QUERY: &str = r"query TreeHarborGroups($owner:String!,$name:String!,$number:Int!,$base:String!){
 repository(owner:$owner,name:$name){databaseId
 ref(qualifiedName:$base){target{... on Commit{oid tree{oid}}}}
 pullRequest(number:$number){
 id number headRefName headRefOid baseRefName baseRefOid state isDraft merged
 headRepository{databaseId} baseRepository{databaseId} isMergeQueueEnabled isInMergeQueue
 mergeCommit{oid tree{oid}}
 mergeQueueEntry{...Member}
 mergeQueue{id repository{databaseId} entries(first:100){totalCount pageInfo{hasNextPage hasPreviousPage} nodes{...Member}}}
 }}}
 fragment Member on MergeQueueEntry {
 id position state mergeQueue{id}
 pullRequest{id number headRefOid baseRefName headRepository{databaseId} baseRepository{databaseId}}
 baseCommit{oid tree{oid}}
 headCommit{oid tree{oid} parents(first:8){totalCount pageInfo{hasNextPage hasPreviousPage} nodes{oid tree{oid}}}}
 }";

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
            return Err(Error::Queue);
        }
        Ok(self.nodes)
    }
}
#[derive(Deserialize)]
struct ParentCommit {
    oid: String,
    tree: Tree,
    parents: Items<Commit>,
}
impl ParentCommit {
    fn observation(self) -> Result<CommitParents> {
        Ok(CommitParents {
            commit: Commit {
                oid: self.oid,
                tree: self.tree,
            }
            .observation(),
            parents: self
                .parents
                .complete(MAX_PARENTS)?
                .into_iter()
                .map(Commit::observation)
                .collect(),
        })
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemberPull {
    id: String,
    number: u64,
    head_ref_oid: String,
    base_ref_name: String,
    head_repository: DatabaseId,
    base_repository: DatabaseId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Member {
    id: String,
    position: u32,
    state: String,
    merge_queue: QueueId,
    pull_request: MemberPull,
    base_commit: Option<Commit>,
    head_commit: Option<ParentCommit>,
}
#[derive(Deserialize)]
struct MembersQueue {
    id: String,
    entries: Items<Member>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MembersPull {
    merge_queue: Option<MembersQueue>,
}
#[derive(Deserialize)]
struct Reference {
    target: Commit,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MembersRepository {
    #[serde(rename = "ref")]
    reference: Reference,
    pull_request: MembersPull,
}
#[derive(Deserialize)]
struct MembersData {
    repository: MembersRepository,
}
#[derive(Deserialize)]
struct MembersGraph {
    data: MembersData,
}

fn decode(value: Value, b: &RepositoryBinding, t: &QueueTarget) -> Result<GroupSnapshot> {
    let target = decode_snapshot(
        serde_json::from_value(value.clone()).map_err(|_| Error::Queue)?,
        b,
        t,
        false,
    )?;
    let r = serde_json::from_value::<MembersGraph>(value)
        .map_err(|_| Error::Queue)?
        .data
        .repository;
    let mut members = Vec::new();
    if let Some(q) = r.pull_request.merge_queue {
        for m in q.entries.complete(MAX_MEMBERS)? {
            if m.merge_queue.id != q.id || m.pull_request.base_repository.id != b.repository_id {
                return Err(Error::Queue);
            }
            let candidate = m.head_commit.map(ParentCommit::observation).transpose()?;
            members.push(QueueMember {
                entry: QueueEntryObservation {
                    id: m.id,
                    state: m.state,
                    base: m.base_commit.map(Commit::observation),
                    head: candidate.as_ref().map(|c| c.commit.clone()),
                },
                position: m.position,
                pull_node: m.pull_request.id,
                pull_number: m.pull_request.number,
                source_repository_id: m.pull_request.head_repository.id,
                source_revision: format!("git:sha1:{}", m.pull_request.head_ref_oid),
                base_ref: m.pull_request.base_ref_name,
                candidate,
            });
        }
    }
    // Provider connection order is not used as proof of queue order.
    members.sort_by_key(|m| m.position);
    let result = GroupSnapshot {
        target,
        main: r.reference.target.observation(),
        members,
    };
    result.validate(t).map_err(|_| Error::Queue)?;
    Ok(result)
}
impl Connection {
    async fn group_snapshot(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        token: &wire::Token,
    ) -> Result<GroupSnapshot> {
        let value = self.json(Stage::Queue,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":GROUP_QUERY,"variables":{"owner":b.owner,"name":b.name,"number":t.pull.pull_number,"base":format!("refs/heads/{}",t.pull.base_ref)}})),StatusCode::OK).await?;
        decode(value, b, t)
    }
    /// Captures both reads even on drift. Neither read authorizes a mutation.
    pub async fn observe_queue_groups(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        t.validate().map_err(|_| Error::Queue)?;
        if b.repository_id != t.pull.repository_id || t.pull.pull_number > i32::MAX as u64 {
            return Err(Error::Queue);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let capability = wire::Capability::QueueRead;
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, capability))
                .await
                .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            self.observe_for(b, &installation, &token, capability)
                .await?;
            let before = self.group_snapshot(b, t, &token).await?;
            let after = self.group_snapshot(b, t, &token).await?;
            Ok::<_, Error>(GroupCapture { before, after })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let capture = result?;
        let observation = QueueObservation {
            version: 4,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            snapshot: Some(capture.after.target.clone()),
            group_capture: Some(capture),
            group_checks: None,
            group_attempt: None,
            workflow_definition: None,
            receipt: None,
            enqueue_receipt: None,
            async_result: None,
            mutation_not_sent: false,
            token_revoked: revoked,
        };
        observation.validate(t).map_err(|_| Error::Queue)?;
        Ok(ObservedQueue {
            binding: b.clone(),
            observation,
            token_expires_at: token.expires_at.clone(),
        })
    }
}
