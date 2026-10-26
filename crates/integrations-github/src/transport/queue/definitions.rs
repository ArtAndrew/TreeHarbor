use super::*;
use treeharbor_github_contract::workflow_definitions as wd;

const QUERY: &str = r"query TreeHarborWorkflowFile($owner:String!,$name:String!,$revision:String!,$path:String!){
 repository(owner:$owner,name:$name){databaseId object(expression:$revision){__typename
 ... on Commit{oid tree{oid} file(path:$path){path mode oid size object{__typename
 ... on Blob{oid byteSize isBinary isTruncated text}}}}}}}";
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Blob {
    #[serde(rename = "__typename")]
    kind: String,
    oid: String,
    byte_size: usize,
    is_binary: bool,
    is_truncated: bool,
    text: String,
}
#[derive(Deserialize)]
struct Entry {
    path: String,
    mode: u32,
    oid: String,
    size: usize,
    object: Blob,
}
#[derive(Deserialize)]
struct FileCommit {
    #[serde(rename = "__typename")]
    kind: String,
    oid: String,
    tree: Tree,
    file: Entry,
}
#[derive(Deserialize)]
struct Repository {
    #[serde(rename = "databaseId")]
    id: u64,
    object: FileCommit,
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
    async fn workflow_file(
        &self,
        b: &RepositoryBinding,
        selected: &wd::Target,
        token: &wire::Token,
    ) -> Result<wd::File> {
        let source = selected.source().map_err(|_| Error::Evidence)?;
        let sha = source
            .revision
            .strip_prefix("git:sha1:")
            .ok_or(Error::Evidence)?;
        let g:Graph=self.json(Stage::CheckEvidence,Method::POST,"graphql",token.token.header()?,
            Some(json!({"query":QUERY,"variables":{"owner":b.owner,"name":b.name,"revision":sha,"path":source.path}})),StatusCode::OK).await?;
        if g.errors
            .is_some_and(|e| e.as_array().is_none_or(|a| !a.is_empty()))
        {
            return Err(Error::Evidence);
        }
        let r = g.data.ok_or(Error::Evidence)?.repository;
        let c = r.object;
        let e = c.file;
        let blob = e.object;
        if r.id != b.repository_id
            || c.kind != "Commit"
            || c.oid != sha
            || e.path != source.path
            || e.oid != blob.oid
            || e.size != blob.byte_size
            || blob.kind != "Blob"
            || blob.is_binary
            || blob.is_truncated
            || blob.byte_size != blob.text.len()
            || blob.byte_size > wd::MAX_FILE_BYTES
        {
            return Err(Error::Evidence);
        }
        Ok(wd::File {
            repository_id: r.id,
            source,
            tree: format!("git:sha1:{}", c.tree.oid),
            mode: e.mode,
            blob: format!("git:sha1:{}", blob.oid),
            byte_size: blob.byte_size,
            content_sha256: wd::content_digest(&blob.text).to_string(),
            content: blob.text,
        })
    }
    /// Read immutable executed root bytes. This neither approves them nor proves runtime context.
    pub async fn observe_workflow_definition(
        &self,
        b: &RepositoryBinding,
        t: &QueueTarget,
        selected: &wd::Target,
    ) -> Result<ObservedQueue> {
        b.validate()?;
        t.validate().map_err(|_| Error::Evidence)?;
        selected.validate(t).map_err(|_| Error::Evidence)?;
        if b.repository_id != t.pull.repository_id
            || !selected
                .source()
                .map_err(|_| Error::Evidence)?
                .repository_name
                .eq_ignore_ascii_case(&format!("{}/{}", b.owner, b.name))
        {
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
            self.workflow_file(b, selected, &token).await
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = self.revoke(&token).await.is_ok();
        let report = wd::Report {
            target: selected.clone(),
            file: result?,
        };
        let observation = QueueObservation {
            version: 7,
            started_at_unix_ms: started,
            finished_at_unix_ms: now()? as i64 * 1000,
            workflow_definition: Some(report),
            publication: None,
            host_policy: None,
            group_attempt: None,
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
