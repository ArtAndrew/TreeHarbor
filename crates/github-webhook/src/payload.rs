use super::{Error, Result};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GroupAction {
    ChecksRequested,
    Destroyed,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DestroyReason {
    Merged,
    Invalidated,
    Dequeued,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct MergeGroupEvent {
    pub version: u8,
    pub installation_id: u64,
    pub repository_id: u64,
    pub owner_id: u64,
    pub action: GroupAction,
    pub reason: Option<DestroyReason>,
    pub head_revision: String,
    pub head_tree: String,
    pub head_ref: String,
    pub base_revision: String,
    pub base_ref: String,
}
impl MergeGroupEvent {
    pub fn validate(&self) -> Result<()> {
        if self.version != 1
            || (self.action == GroupAction::ChecksRequested && self.reason.is_some())
        {
            return Err(Error::Payload);
        }
        for id in [self.installation_id, self.repository_id, self.owner_id] {
            positive(id)?;
        }
        for revision in [&self.head_revision, &self.head_tree, &self.base_revision] {
            sha(revision.strip_prefix("git:sha1:").ok_or(Error::Payload)?)?;
        }
        full_ref(&self.head_ref)?;
        full_ref(&self.base_ref)?;
        if self.head_ref == self.base_ref {
            return Err(Error::Payload);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum WebhookEvent {
    MergeGroup(MergeGroupEvent),
    Ping { hook_id: u64 },
}

// External objects intentionally tolerate extension fields, but typed consumed
// fields reject duplicates. Never parse into a map that silently keeps the last.
#[derive(Deserialize)]
struct Id {
    id: u64,
}
#[derive(Deserialize)]
struct Repository {
    id: u64,
    owner: Id,
}
#[derive(Deserialize)]
struct Commit {
    id: String,
    tree_id: String,
}
#[derive(Deserialize)]
struct Group {
    head_sha: String,
    head_ref: String,
    base_sha: String,
    base_ref: String,
    head_commit: Commit,
}
#[derive(Deserialize)]
struct Hook {
    id: u64,
    app_id: u64,
}
#[derive(Deserialize)]
struct Payload {
    action: Option<String>,
    reason: Option<String>,
    installation: Option<Id>,
    repository: Option<Repository>,
    merge_group: Option<Group>,
    hook_id: Option<u64>,
    hook: Option<Hook>,
}
fn positive(n: u64) -> Result<()> {
    if n == 0 || n > i64::MAX as u64 {
        Err(Error::Payload)
    } else {
        Ok(())
    }
}
fn sha(s: &str) -> Result<()> {
    if s.len() != 40
        || s.bytes().all(|b| b == b'0')
        || !s
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Err(Error::Payload)
    } else {
        Ok(())
    }
}
fn full_ref(s: &str) -> Result<()> {
    if s.len() > 1024
        || !s.starts_with("refs/heads/")
        || s.ends_with('.')
        || s.contains("..")
        || s.contains("@{")
        || s.chars()
            .any(|c| c.is_control() || c == ' ' || "~^:?*[\\".contains(c))
        || s.split('/')
            .any(|p| p.is_empty() || p.starts_with('.') || p.ends_with(".lock"))
    {
        Err(Error::Payload)
    } else {
        Ok(())
    }
}
pub(super) fn decode(app_id: u64, header: &str, body: &[u8]) -> Result<WebhookEvent> {
    let p: Payload = serde_json::from_slice(body).map_err(|_| Error::Payload)?;
    match header {
        "merge_group" => {
            if p.hook.is_some() || p.hook_id.is_some() {
                return Err(Error::Payload);
            }
            let action = match p.action.as_deref() {
                Some("checks_requested") => GroupAction::ChecksRequested,
                Some("destroyed") => GroupAction::Destroyed,
                _ => return Err(Error::Unsupported),
            };
            let reason = match p.reason.as_deref() {
                None => None,
                Some("merged") => Some(DestroyReason::Merged),
                Some("invalidated") => Some(DestroyReason::Invalidated),
                Some("dequeued") => Some(DestroyReason::Dequeued),
                _ => return Err(Error::Unsupported),
            };
            let i = p.installation.ok_or(Error::Payload)?;
            let r = p.repository.ok_or(Error::Payload)?;
            let g = p.merge_group.ok_or(Error::Payload)?;
            if g.head_sha != g.head_commit.id {
                return Err(Error::Payload);
            }
            let result = MergeGroupEvent {
                version: 1,
                installation_id: i.id,
                repository_id: r.id,
                owner_id: r.owner.id,
                action,
                reason,
                head_revision: format!("git:sha1:{}", g.head_sha),
                head_tree: format!("git:sha1:{}", g.head_commit.tree_id),
                head_ref: g.head_ref,
                base_revision: format!("git:sha1:{}", g.base_sha),
                base_ref: g.base_ref,
            };
            result.validate()?;
            Ok(WebhookEvent::MergeGroup(result))
        }
        "ping" => {
            if p.action.is_some() || p.reason.is_some() || p.merge_group.is_some() {
                return Err(Error::Payload);
            }
            let id = p.hook_id.ok_or(Error::Payload)?;
            let hook = p.hook.ok_or(Error::Payload)?;
            positive(id)?;
            if hook.id != id || hook.app_id != app_id {
                return Err(Error::Payload);
            }
            Ok(WebhookEvent::Ping { hook_id: id })
        }
        _ => Err(Error::Unsupported),
    }
}
