//! Discovery after a database restore. A completed traversal is not an atomic
//! host snapshot, external-writer fencing, or permission to resume publication.
use super::*;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const PAGE_SIZE: usize = 50;
const MAX_PAGES: usize = 100;
const INVENTORY_TIMEOUT: Duration = Duration::from_secs(120);
const HEADS: &str = r#"query TreeHarborRestoreHeads($owner:String!,$name:String!,$after:String){
 repository(owner:$owner,name:$name,followRenames:false){id databaseId nameWithOwner
 inventory:refs(refPrefix:"refs/heads/",first:50,after:$after,orderBy:{field:ALPHABETICAL,direction:ASC}){
 totalCount pageInfo{endCursor hasNextPage} nodes{id name prefix target{__typename oid}}}}}"#;
const PULLS: &str = r#"query TreeHarborRestorePulls($owner:String!,$name:String!,$after:String){
 repository(owner:$owner,name:$name,followRenames:false){id databaseId nameWithOwner
 inventory:pullRequests(first:50,after:$after,states:[OPEN,CLOSED,MERGED],orderBy:{field:CREATED_AT,direction:ASC}){
 totalCount pageInfo{endCursor hasNextPage} nodes{id number state isDraft merged
 headRefName headRefOid baseRefName baseRefOid headRepository{databaseId}
 baseRepository{databaseId} mergeCommit{oid}}}}}"#;

#[derive(Debug, Deserialize, Serialize)]
pub struct InventoryObject {
    #[serde(rename = "__typename")]
    pub kind: String,
    pub oid: String,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct InventoryHead {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub target: InventoryObject,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryRepositoryId {
    pub database_id: u64,
}
#[derive(Debug, Deserialize, Serialize)]
pub struct InventoryCommit {
    pub oid: String,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InventoryPull {
    pub id: String,
    pub number: u64,
    pub state: String,
    pub is_draft: bool,
    pub merged: bool,
    pub head_ref_name: String,
    pub head_ref_oid: String,
    pub base_ref_name: String,
    pub base_ref_oid: String,
    // Deleted forks remain unknown; never silently substitute the base repo.
    #[serde(deserialize_with = "required_nullable")]
    pub head_repository: Option<InventoryRepositoryId>,
    pub base_repository: InventoryRepositoryId,
    #[serde(deserialize_with = "required_nullable")]
    pub merge_commit: Option<InventoryCommit>,
}
#[derive(Debug, Serialize)]
pub struct RestoreInventory {
    pub version: u8,
    pub binding: RepositoryBinding,
    pub app_id: u64,
    pub repository_node_id: String,
    pub default_branch: String,
    pub started_at_unix: u64,
    pub finished_at_unix: u64,
    pub heads: Vec<InventoryHead>,
    pub pulls: Vec<InventoryPull>,
    pub token_revoked: bool,
    pub token_expires_at: String,
    pub reconciliation_complete: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    end_cursor: Option<String>,
    has_next_page: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page<T> {
    total_count: usize,
    page_info: PageInfo,
    nodes: Vec<T>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository<T> {
    id: String,
    database_id: u64,
    name_with_owner: String,
    inventory: Page<T>,
}
#[derive(Deserialize)]
struct Data<T> {
    repository: Repository<T>,
}
#[derive(Deserialize)]
struct Graph<T> {
    data: Option<Data<T>>,
    errors: Option<Value>,
}

fn text_field(value: &str, limit: usize) -> bool {
    !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control)
}
fn required_nullable<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}
fn oid(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && value.bytes().any(|c| c != b'0')
}
trait Item {
    fn validate(&self, binding: &RepositoryBinding) -> bool;
    fn id(&self) -> &str;
    fn key(&self) -> String;
}
impl Item for InventoryHead {
    fn validate(&self, _: &RepositoryBinding) -> bool {
        text_field(&self.id, 256)
            && text_field(&self.name, 1024)
            && self.prefix == "refs/heads/"
            && self.target.kind == "Commit"
            && oid(&self.target.oid)
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn key(&self) -> String {
        self.name.clone()
    }
}
impl Item for InventoryPull {
    fn validate(&self, b: &RepositoryBinding) -> bool {
        text_field(&self.id, 256)
            && self.number > 0
            && self.number <= i32::MAX as u64
            && matches!(self.state.as_str(), "OPEN" | "CLOSED" | "MERGED")
            && self.merged == (self.state == "MERGED")
            && self.base_repository.database_id == b.repository_id
            && self
                .head_repository
                .as_ref()
                .is_none_or(|r| r.database_id != 0)
            && text_field(&self.head_ref_name, 1024)
            && text_field(&self.base_ref_name, 1024)
            && oid(&self.head_ref_oid)
            && oid(&self.base_ref_oid)
            && self.merge_commit.as_ref().is_none_or(|c| oid(&c.oid))
    }
    fn id(&self) -> &str {
        &self.id
    }
    fn key(&self) -> String {
        self.number.to_string()
    }
}

impl Connection {
    async fn inventory_pages<T: DeserializeOwned + Item>(
        &self,
        b: &RepositoryBinding,
        node: &str,
        token: &wire::Token,
        query: &str,
    ) -> Result<Vec<T>> {
        let mut after: Option<String> = None;
        let mut cursors = BTreeSet::new();
        let mut ids = BTreeSet::new();
        let mut keys = BTreeSet::new();
        let mut total = None;
        let mut items = Vec::new();
        for _ in 0..MAX_PAGES {
            let graph: Graph<T> = self
                .json(
                    Stage::RestoreInventory,
                    Method::POST,
                    "graphql",
                    token.token.header()?,
                    Some(json!({"query":query,"variables":{
                        "owner":b.owner,"name":b.name,"after":after
                    }})),
                    StatusCode::OK,
                )
                .await?;
            if graph.errors.is_some() {
                return Err(Error::RestoreInventory);
            }
            let repo = graph.data.ok_or(Error::RestoreInventory)?.repository;
            if repo.id != node
                || repo.database_id != b.repository_id
                || repo.name_with_owner != format!("{}/{}", b.owner, b.name)
            {
                return Err(Error::Identity);
            }
            let page = repo.inventory;
            if page.nodes.len() > PAGE_SIZE
                || page.total_count > PAGE_SIZE * MAX_PAGES
                || (page.nodes.is_empty() && page.page_info.has_next_page)
                || total.is_some_and(|n| n != page.total_count)
            {
                return Err(Error::RestoreInventory);
            }
            total = Some(page.total_count);
            for item in page.nodes {
                if !item.validate(b)
                    || !ids.insert(item.id().to_owned())
                    || !keys.insert(item.key())
                {
                    return Err(Error::RestoreInventory);
                }
                items.push(item);
            }
            if items.len() > page.total_count {
                return Err(Error::RestoreInventory);
            }
            if !page.page_info.has_next_page {
                return if items.len() == page.total_count {
                    Ok(items)
                } else {
                    Err(Error::RestoreInventory)
                };
            }
            let cursor = page.page_info.end_cursor.ok_or(Error::RestoreInventory)?;
            if items.len() >= page.total_count
                || !text_field(&cursor, 2048)
                || !cursors.insert(cursor.clone())
            {
                return Err(Error::RestoreInventory);
            }
            after = Some(cursor);
        }
        Err(Error::RestoreInventory)
    }

    /// Enumerate heads and all PR states, including identities absent from a
    /// restored database. Never writes host state or clears the restore fence.
    /// Results span an interval, not a transaction: callers must reconcile and
    /// reobserve selected effects before authorizing any fresh publication.
    pub async fn observe_restore_inventory(
        &self,
        b: &RepositoryBinding,
    ) -> Result<RestoreInventory> {
        b.validate()?;
        let _slot = tokio::time::timeout(PROBE_TIMEOUT, self.slots.acquire())
            .await
            .map_err(|_| Error::Deadline)?
            .map_err(|_| Error::Configuration)?;
        let (installation, token) =
            tokio::time::timeout(PROBE_TIMEOUT, self.mint_for(b, wire::Capability::PullRead))
                .await
                .map_err(|_| Error::Deadline)??;
        let observed = tokio::time::timeout(INVENTORY_TIMEOUT, async {
            let started = now()?;
            let probe = self
                .observe_for(b, &installation, &token, wire::Capability::PullRead)
                .await?;
            let expiry = token.check_for(b.repository_id, now()?, wire::Capability::PullRead)?;
            if now()?.saturating_add(INVENTORY_TIMEOUT.as_secs() + 30) >= expiry {
                return Err(Error::TokenLifetime);
            }
            let heads = self
                .inventory_pages(b, &probe.repository_node_id, &token, HEADS)
                .await?;
            let pulls = self
                .inventory_pages(b, &probe.repository_node_id, &token, PULLS)
                .await?;
            Ok(RestoreInventory {
                version: 1,
                binding: b.clone(),
                app_id: probe.app_id,
                repository_node_id: probe.repository_node_id,
                default_branch: probe.default_branch,
                started_at_unix: started,
                finished_at_unix: now()?,
                heads,
                pulls,
                token_revoked: false,
                token_expires_at: token.expires_at.clone(),
                reconciliation_complete: false,
            })
        })
        .await
        .map_err(|_| Error::Deadline)
        .and_then(|r| r);
        let revoked = tokio::time::timeout(PROBE_TIMEOUT, self.revoke(&token))
            .await
            .is_ok_and(|r| r.is_ok());
        let mut result = observed?;
        result.token_revoked = revoked;
        Ok(result)
    }
}
