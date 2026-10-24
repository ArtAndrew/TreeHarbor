use super::*;
use treeharbor_github_contract::dequeue::{DequeueObservation, DequeueReceipt};
use uuid::Uuid;

pub struct ObservedDequeue {
    pub binding: RepositoryBinding,
    pub observation: DequeueObservation,
    pub token_expires_at: String,
}
const MUTATION: &str = r"mutation TreeHarborDequeue($id:ID!,$request:String!){
 dequeuePullRequest(input:{id:$id,clientMutationId:$request}){
 clientMutationId mergeQueueEntry{id mergeQueue{id repository{databaseId}}
 pullRequest{id number headRefOid baseRepository{databaseId} headRepository{databaseId}}}
 }}";

#[derive(Deserialize)]
struct Graph {
    data: Option<Data>,
    errors: Option<Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Data {
    dequeue_pull_request: Payload,
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

fn receipt(graph: Graph, target: &QueueTarget, request: Uuid) -> Result<DequeueReceipt> {
    if graph
        .errors
        .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Error::Queue);
    }
    let p = graph.data.ok_or(Error::Queue)?.dequeue_pull_request;
    let e = p.merge_queue_entry;
    if e.pull_request.base_repository.id != target.pull.repository_id
        || e.pull_request.head_repository.id != target.pull.repository_id
    {
        return Err(Error::Queue);
    }
    let r = DequeueReceipt {
        request_id: p.client_mutation_id,
        repository_id: e.merge_queue.repository.id,
        pull_node: e.pull_request.id,
        pull_number: e.pull_request.number,
        queue_id: e.merge_queue.id,
        entry_id: e.id,
        head_revision: format!("git:sha1:{}", e.pull_request.head_ref_oid),
    };
    r.validate(target, request).map_err(|_| Error::Queue)?;
    Ok(r)
}
impl Connection {
    /// Requires explicit PR-scoped owner authorization, settled enqueue history
    /// and one fresh durable dispatch grant. clientMutationId is correlation,
    /// not provider idempotency. The callback must recheck those local fences.
    pub async fn dequeue_pull<G, F>(
        &self,
        b: &RepositoryBinding,
        target: &QueueTarget,
        request: Uuid,
        eligible: G,
    ) -> Result<ObservedDequeue>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        self.dequeue_pull_attempt(b, target, request, eligible)
            .await
            .map_err(|e| e.error)
    }
    pub async fn dequeue_pull_attempt<G, F>(
        &self,
        b: &RepositoryBinding,
        target: &QueueTarget,
        request: Uuid,
        eligible: G,
    ) -> std::result::Result<ObservedDequeue, MutationFailure>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        let sent = AtomicBool::new(false);
        self.dequeue_operation(b, target, request, eligible, &sent)
            .await
            .map_err(|error| MutationFailure {
                error,
                mutation_not_sent: !sent.load(Ordering::SeqCst),
            })
    }
    async fn dequeue_operation<G, F>(
        &self,
        b: &RepositoryBinding,
        target: &QueueTarget,
        request: Uuid,
        mut eligible: G,
        sent: &AtomicBool,
    ) -> Result<ObservedDequeue>
    where
        G: FnMut() -> F,
        F: std::future::Future<Output = Result<()>>,
    {
        b.validate()?;
        target.validate().map_err(|_| Error::Queue)?;
        if request.is_nil()
            || b.repository_id != target.pull.repository_id
            || target.pull.pull_number > i32::MAX as u64
        {
            return Err(Error::Queue);
        }
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let started = now()? as i64 * 1000;
        let capability = wire::Capability::QueueRemove;
        let (installation, token) = tokio::time::timeout(PROBE_TIMEOUT, async {
            eligible().await?;
            self.mint_for(b, capability).await
        })
        .await
        .map_err(|_| Error::Deadline)??;
        let result = tokio::time::timeout(PROBE_TIMEOUT, async {
            let repository = self.observe_for(b, &installation, &token, capability).await?;
            if repository.owner_type != OwnerType::Organization || repository.archived || repository.disabled { return Err(Error::Queue); }
            let before = self.queue_snapshot(b, target, &token, true).await?;
            if before.pull.merged || before.entry.is_none() { return Ok((None, None, Some(before))); }
            eligible().await?;
            if now()?.saturating_add(30) >= token.check_for(b.repository_id, now()?, capability)? { return Err(Error::TokenLifetime); }
            let header=token.token.header()?;
            sent.store(true,Ordering::SeqCst);
            let graph: Result<Graph> = self.json(Stage::Queue, Method::POST, "graphql", header,
                Some(json!({"query":MUTATION,"variables":{"id":target.pull.pull_node,"request":request.to_string()}})), StatusCode::OK).await;
            // Return immediately for journal fsync; do not risk losing the receipt
            // by making another provider read first. Unknown outcomes never retry.
            let acknowledged = graph.ok().and_then(|g| receipt(g, target, request).ok());
            Ok((Some(before), acknowledged, None))
        }).await.map_err(|_| Error::Deadline).and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let (before, receipt, after) = result?;
        let observation = DequeueObservation {
            version: 1,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            before,
            receipt,
            mutation_not_sent: after.is_some(),
            after,
            token_revoked: revoked,
        };
        observation
            .validate(target, request)
            .map_err(|_| Error::Queue)?;
        Ok(ObservedDequeue {
            binding: b.clone(),
            observation,
            token_expires_at: token.expires_at.clone(),
        })
    }
    /// Observation only; never repeats the earlier dequeue mutation.
    pub async fn observe_dequeue(
        &self,
        b: &RepositoryBinding,
        target: &QueueTarget,
        request: Uuid,
    ) -> Result<ObservedDequeue> {
        if request.is_nil() {
            return Err(Error::Queue);
        }
        let r = self.observe_queue(b, target).await?;
        let o = r.observation;
        let observation = DequeueObservation {
            version: 2,
            started_at_unix_ms: o.started_at_unix_ms,
            finished_at_unix_ms: o.finished_at_unix_ms,
            before: None,
            receipt: None,
            after: o.snapshot,
            mutation_not_sent: false,
            token_revoked: o.token_revoked,
        };
        observation
            .validate(target, request)
            .map_err(|_| Error::Queue)?;
        Ok(ObservedDequeue {
            binding: r.binding,
            observation,
            token_expires_at: r.token_expires_at,
        })
    }
}
