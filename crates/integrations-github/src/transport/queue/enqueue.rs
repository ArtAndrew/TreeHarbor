//! Exact-head ordinary-PR admission. clientMutationId is not host idempotency.
use super::*;
use uuid::Uuid;

pub(super) const MUTATION: &str = r"mutation TreeHarborEnqueue($id:ID!,$head:GitObjectID!,$request:String!){
 enqueuePullRequest(input:{pullRequestId:$id,expectedHeadOid:$head,jump:false,clientMutationId:$request}){
 clientMutationId mergeQueueEntry{id mergeQueue{id repository{databaseId}}
 pullRequest{id number headRefOid baseRepository{databaseId} headRepository{databaseId}}}
 }}";

#[derive(Deserialize)]
pub(super) struct Graph {
    data: Option<Data>,
    errors: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    enqueue_pull_request: Payload,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Payload {
    client_mutation_id: String,
    merge_queue_entry: Entry,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    merge_queue: EntryQueue,
    pull_request: Pull,
}
#[derive(Deserialize)]
struct EntryQueue {
    id: String,
    repository: DatabaseId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Pull {
    id: String,
    number: u64,
    head_ref_oid: String,
    base_repository: DatabaseId,
    head_repository: DatabaseId,
}

pub(super) fn receipt(
    g: Graph,
    t: &QueueTarget,
    request: Uuid,
    queue: &str,
) -> Result<EnqueueReceipt> {
    if g.errors
        .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Error::Queue);
    }
    let p = g.data.ok_or(Error::Queue)?.enqueue_pull_request;
    let e = p.merge_queue_entry;
    if e.merge_queue.id != queue
        || e.pull_request.base_repository.id != t.pull.repository_id
        || e.pull_request.head_repository.id != t.pull.repository_id
    {
        return Err(Error::Queue);
    }
    let receipt = EnqueueReceipt {
        request_id: p.client_mutation_id,
        repository_id: e.merge_queue.repository.id,
        pull_node: e.pull_request.id,
        pull_number: e.pull_request.number,
        queue_id: e.merge_queue.id,
        entry_id: e.id,
        head_revision: format!("git:sha1:{}", e.pull_request.head_ref_oid),
    };
    receipt.validate(t, request).map_err(|_| Error::Queue)?;
    Ok(receipt)
}

impl Connection {
    /// Only the invocation holding a fresh durable grant may call this. Missing,
    /// malformed or lost responses remain uncertain and never trigger a retry.
    pub async fn enqueue_pull_graphql_attempt<G, F>(
        &self,
        b: &RepositoryBinding,
        target: &QueueTarget,
        attempt: Uuid,
        eligible: G,
    ) -> std::result::Result<ObservedQueue, MutationFailure>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        let sent = AtomicBool::new(false);
        self.queue_operation(b, target, true, Some(attempt), eligible, &sent)
            .await
            .map_err(|error| MutationFailure {
                error,
                mutation_not_sent: !sent.load(Ordering::SeqCst),
            })
    }
}
