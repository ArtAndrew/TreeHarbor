//! Native GitHub queue only. A host merge remains unverified until separate proof.
use super::*;
use serde::Deserialize;
use std::sync::atomic::{AtomicBool, Ordering};
use treeharbor_github_contract::{PullState, queue::*};
mod attempts;
mod checks;
mod definitions;
mod dequeue;
mod enqueue;
mod groups;
mod result;
pub use dequeue::ObservedDequeue;

pub struct ObservedQueue {
    pub binding: RepositoryBinding,
    pub observation: QueueObservation,
    pub token_expires_at: String,
}
/// A completed local invocation can prove no mutation was sent. A killed worker
/// produces no such evidence; marking before the HTTP call is conservative.
#[derive(Debug)]
pub struct MutationFailure {
    pub error: Error,
    pub mutation_not_sent: bool,
}
const QUERY: &str = r"query TreeHarborQueue($owner:String!,$name:String!,$number:Int!,$configuration:Boolean!){
 repository(owner:$owner,name:$name){databaseId pullRequest(number:$number){
 id number headRefName headRefOid baseRefName baseRefOid state isDraft merged
 headRepository{databaseId} baseRepository{databaseId} isMergeQueueEnabled isInMergeQueue
 mergeCommit{oid tree{oid}}
 mergeQueue{id repository{databaseId} configuration @include(if:$configuration){mergeMethod mergingStrategy maximumEntriesToBuild maximumEntriesToMerge minimumEntriesToMerge minimumEntriesToMergeWaitTime checkResponseTimeout}}
 mergeQueueEntry{id state baseCommit{oid tree{oid}} headCommit{oid tree{oid}} mergeQueue{id} pullRequest{id number headRefOid}}
 }}}
";
#[derive(Deserialize)]
struct DatabaseId {
    #[serde(rename = "databaseId")]
    id: u64,
}
#[derive(Deserialize)]
struct Tree {
    oid: String,
}
#[derive(Deserialize)]
struct Commit {
    oid: String,
    tree: Tree,
}
impl Commit {
    fn observation(self) -> GitCommitObservation {
        GitCommitObservation {
            revision: format!("git:sha1:{}", self.oid),
            tree: format!("git:sha1:{}", self.tree.oid),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Configuration {
    merge_method: String,
    merging_strategy: String,
    maximum_entries_to_build: u32,
    maximum_entries_to_merge: u32,
    minimum_entries_to_merge: u32,
    minimum_entries_to_merge_wait_time: u32,
    check_response_timeout: u32,
}
#[derive(Deserialize)]
struct Queue {
    id: String,
    repository: DatabaseId,
    configuration: Option<Configuration>,
}
#[derive(Deserialize)]
struct QueueId {
    id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct EntryPull {
    id: String,
    number: u64,
    head_ref_oid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    state: String,
    base_commit: Option<Commit>,
    head_commit: Option<Commit>,
    merge_queue: QueueId,
    pull_request: EntryPull,
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
    is_merge_queue_enabled: bool,
    is_in_merge_queue: bool,
    merge_commit: Option<Commit>,
    merge_queue: Option<Queue>,
    merge_queue_entry: Option<Entry>,
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
    async fn queue_snapshot(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        token: &wire::Token,
        include_configuration: bool,
    ) -> Result<QueueSnapshot> {
        let graph:Graph=self.json(Stage::Queue,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":QUERY,"variables":{"owner":b.owner,"name":b.name,"number":t.pull.pull_number,"configuration":include_configuration}})),StatusCode::OK).await?;
        decode_snapshot(graph, b, t, include_configuration)
    }
    /// A read-scoped observation never retries an earlier mutation or proves merge.
    pub async fn observe_queue(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
    ) -> Result<ObservedQueue> {
        self.queue_operation(
            b,
            t,
            false,
            None,
            || async { Ok(()) },
            &AtomicBool::new(false),
        )
        .await
    }
    /// Requires one fresh durable dispatch from the caller. Checks are recollected;
    /// an ambiguous result never causes a second PUT or a direct-merge fallback.
    pub async fn enqueue_pull<G, F>(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        eligible: G,
    ) -> Result<ObservedQueue>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        self.enqueue_pull_attempt(b, t, eligible)
            .await
            .map_err(|e| e.error)
    }
    /// Retains positive no-send evidence on preflight/authorization failure.
    pub async fn enqueue_pull_attempt<G, F>(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        eligible: G,
    ) -> std::result::Result<ObservedQueue, MutationFailure>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        let sent = AtomicBool::new(false);
        self.queue_operation(b, t, true, None, eligible, &sent)
            .await
            .map_err(|error| MutationFailure {
                error,
                mutation_not_sent: !sent.load(Ordering::SeqCst),
            })
    }
    async fn queue_operation<G, F>(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        write: bool,
        graphql: Option<uuid::Uuid>,
        mut eligible: G,
        sent: &AtomicBool,
    ) -> Result<ObservedQueue>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        b.validate()?;
        t.validate().map_err(|_| Error::Queue)?;
        if b.repository_id != t.pull.repository_id
            || t.pull.pull_number > i32::MAX as u64
            || graphql.is_some_and(|id| id.is_nil() || !write)
        {
            return Err(Error::Queue);
        }
        // Do not hold a slot while acquiring another slot for read-only inspection.
        // The queue remains the authority for checks that race these reads.
        let capture = if write {
            let r = self.inspect_pull(b, &t.pull, &mut eligible).await?;
            if !t.admits_capture(&r.report) {
                return Err(Error::Queue);
            }
            Some(r.report)
        } else {
            None
        };
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let capability = if graphql.is_some() {
            wire::Capability::QueueGraphql
        } else if write {
            wire::Capability::QueueWrite
        } else {
            wire::Capability::QueueRead
        };
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, async {
            if write {
                eligible().await?;
            }
            self.mint_for(b, capability).await
        })
        .await
        .map_err(|_| Error::Deadline)??;
        let result=tokio::time::timeout(PROBE_TIMEOUT,async{
            let repository=self.observe_for(b,&installation,&token,capability).await?;
            if write && (repository.owner_type!=OwnerType::Organization || repository.archived || repository.disabled) {return Err(Error::Queue);}
            let before=self.queue_snapshot(b,t,&token,write).await?;
            if !write || before.entry.is_some() || before.pull.merged { return Ok((Some(before),None,None)); }
            if !before.can_request(t) {return Err(Error::Queue);}
            let rules=self.evidence_rules(b,&t.pull,&token).await?;
            let after=self.queue_snapshot(b,t,&token,true).await?;
            if before!=after || rules!=t.expected_rules
                || capture.as_ref().is_none_or(|r|started-r.finished_at_unix_ms>45_000) {return Err(Error::Queue);}
            eligible().await?;
            if now()?.saturating_add(30)>=token.check_for(b.repository_id,now()?,capability)? {return Err(Error::TokenLifetime);}
            if let Some(attempt) = graphql {
                let queue = &after.configuration.as_ref().ok_or(Error::Queue)?.id;
                let header = token.token.header()?;
                let body = json!({"query":enqueue::MUTATION,"variables":{
                    "id":t.pull.pull_node,"head":t.pull.head_revision.strip_prefix("git:sha1:").ok_or(Error::Queue)?,
                    "request":attempt.to_string()}});
                sent.store(true,Ordering::SeqCst);
                let graph: Result<enqueue::Graph> = self.json(Stage::Queue,Method::POST,"graphql",header,Some(body),StatusCode::OK).await;
                let receipt = graph.ok().and_then(|g|enqueue::receipt(g,t,attempt,queue).ok());
                return Ok((None,None,receipt));
            }
            let path=format!("repos/{}/{}/pulls/{}/merge-async",b.owner,b.name,t.pull.pull_number);
            let body=json!({"sha":t.pull.head_revision.strip_prefix("git:sha1:").ok_or(Error::Queue)?,"merge_action":"merge_queue","bypass_rules":false});
            // The request helper has redirects and retries disabled.
            let header=token.token.header()?;
            sent.store(true,Ordering::SeqCst);
            let response=self.request_status(Stage::Queue,Method::PUT,&path,header,Some(body),&[StatusCode::OK,StatusCode::ACCEPTED,StatusCode::CONFLICT,StatusCode::BAD_REQUEST]).await;
            let receipt=response.ok().and_then(|(status,bytes)|decode_receipt(status.as_u16(),&bytes,t).ok());
            // Return the receipt before another provider read so the caller can
            // fsync it. Follow-up observation uses a separate read-only call.
            Ok((None,receipt,None))
        }).await.map_err(|_|Error::Deadline).and_then(|v|v);
        let revoked = self.revoke(&token).await.is_ok();
        let (snapshot, receipt, enqueue_receipt) = result?;
        let mutation_not_sent = write && !sent.load(Ordering::SeqCst);
        let observation = QueueObservation {
            version: if graphql.is_some() || !write {
                3
            } else if mutation_not_sent {
                2
            } else {
                1
            },
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            snapshot,
            receipt,
            enqueue_receipt,
            group_capture: None,
            group_checks: None,
            group_attempt: None,
            workflow_definition: None,
            async_result: None,
            mutation_not_sent,
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
#[derive(Deserialize)]
struct RawReceipt {
    status: String,
    details: Details,
}
#[derive(Deserialize)]
struct Details {
    uuid: Option<String>,
    expected_head_sha: Option<String>,
    merge_action: Option<String>,
    bypass_rules: Option<bool>,
    sha: Option<String>,
}
fn decode_receipt(status: u16, bytes: &[u8], t: &QueueTarget) -> Result<QueueReceipt> {
    let r: RawReceipt = serde_json::from_slice(bytes).map_err(|_| Error::Queue)?;
    let r = QueueReceipt {
        http_status: status,
        status: r.status,
        request_uuid: r.details.uuid,
        expected_head_revision: r.details.expected_head_sha.map(|s| format!("git:sha1:{s}")),
        merge_action: r.details.merge_action,
        bypass_rules: r.details.bypass_rules,
        final_revision: r.details.sha.map(|s| format!("git:sha1:{s}")),
    };
    r.validate(t).map_err(|_| Error::Queue)?;
    Ok(r)
}

fn decode_snapshot(
    graph: Graph,
    b: &RepositoryBinding,
    t: &QueueTarget,
    include_configuration: bool,
) -> Result<QueueSnapshot> {
    if graph
        .errors
        .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Error::Queue);
    }
    let r = graph.data.ok_or(Error::Queue)?.repository;
    let p = r.pull_request;
    if r.database_id != b.repository_id
        || p.head_repository.id != b.repository_id
        || p.base_repository.id != b.repository_id
        || p.is_in_merge_queue != p.merge_queue_entry.is_some()
    {
        return Err(Error::Queue);
    }
    let queue_id = p.merge_queue.as_ref().map(|q| q.id.clone());
    let configuration = p
        .merge_queue
        .map(|q| {
            if q.repository.id != b.repository_id {
                return Err(Error::Queue);
            }
            let Some(c) = q.configuration else {
                return if include_configuration {
                    Err(Error::Queue)
                } else {
                    Ok(None)
                };
            };
            Ok(Some(QueueConfiguration {
                id: q.id,
                merge_method: c.merge_method,
                merging_strategy: c.merging_strategy,
                maximum_entries_to_build: c.maximum_entries_to_build,
                maximum_entries_to_merge: c.maximum_entries_to_merge,
                minimum_entries_to_merge: c.minimum_entries_to_merge,
                minimum_entries_wait_minutes: c.minimum_entries_to_merge_wait_time,
                check_timeout_minutes: c.check_response_timeout,
            }))
        })
        .transpose()?
        .flatten();
    let entry = p
        .merge_queue_entry
        .map(|e| {
            if e.pull_request.id != p.id
                || e.pull_request.number != p.number
                || e.pull_request.head_ref_oid != p.head_ref_oid
                || queue_id.as_ref().is_none_or(|id| *id != e.merge_queue.id)
            {
                return Err(Error::Queue);
            }
            Ok(QueueEntryObservation {
                id: e.id,
                state: e.state,
                base: e.base_commit.map(Commit::observation),
                head: e.head_commit.map(Commit::observation),
            })
        })
        .transpose()?;
    let state = match p.state.as_str() {
        "OPEN" => "open",
        "CLOSED" | "MERGED" => "closed",
        _ => return Err(Error::Queue),
    };
    if (p.state == "MERGED") != p.merged {
        return Err(Error::Queue);
    }
    let snapshot = QueueSnapshot {
        pull: PullState {
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
        },
        queue_enabled: p.is_merge_queue_enabled,
        configuration,
        queue_id: if include_configuration {
            None
        } else {
            queue_id
        },
        entry,
        final_commit: p.merge_commit.map(Commit::observation),
    };
    snapshot.validate(t).map_err(|_| Error::Queue)?;
    Ok(snapshot)
}
