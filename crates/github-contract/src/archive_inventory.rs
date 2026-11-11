//! Typed GitHub inventory shared by transport and local archive verification.
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct InventoryObject {
    #[serde(rename = "__typename")]
    pub kind: String,
    pub oid: String,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct InventoryHead {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub target: InventoryObject,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct InventoryTag {
    pub id: String,
    pub name: String,
    pub prefix: String,
    pub target: InventoryObject,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InventoryRepositoryId {
    pub database_id: u64,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct InventoryCommit {
    pub oid: String,
}
#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
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
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedBranches<B> {
    pub version: u8,
    pub binding: B,
    pub app_id: u64,
    pub repository_node_id: String,
    pub default_branch: String,
    pub started_at_unix: u64,
    pub finished_at_unix: u64,
    pub heads: Vec<InventoryHead>,
    pub token_revoked: bool,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ArchivedRepository<B> {
    pub version: u8,
    pub archive: ArchivedBranches<B>,
    pub tags: Vec<InventoryTag>,
    pub open_pulls: Vec<InventoryPull>,
}

fn required_nullable<'de, D, T>(d: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d)
}
