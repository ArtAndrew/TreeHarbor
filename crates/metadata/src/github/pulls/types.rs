use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullSpec {
    pub publication: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub previous: Option<String>,
    pub ttl_seconds: i32,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullIdentity {
    pub number: u64,
    pub node_id: String,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExpectedPull {
    pub identity: PullIdentity,
    pub title: String,
    pub body: String,
    pub draft: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullTarget {
    pub operation: Uuid,
    pub branch_id: Uuid,
    pub git_revision: String,
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub previous: Option<ExpectedPull>,
}
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PullSnapshot {
    pub identity: PullIdentity,
    pub head_repository: u64,
    pub base_repository: u64,
    pub head_ref: String,
    pub base_ref: String,
    pub head_revision: String,
    pub base_revision: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub state: String,
    pub merged: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PullReport {
    pub attempt: String,
    pub binding: String,
    pub git_revision: String,
    pub pull: Option<PullSnapshot>,
    pub observed_at_unix_ms: i64,
    pub token_revoked: bool,
}
pub(super) fn id(value: &str) -> Result<Uuid> {
    let id: Uuid = value.parse().map_err(|_| Error::Invalid("pull UUID"))?;
    uuid(id)?;
    if id.to_string() != value {
        return Err(Error::Invalid("canonical pull UUID"));
    }
    Ok(id)
}
pub(super) fn git(value: &str) -> Result<()> {
    match value.parse::<treeharbor_core::RevisionRef>() {
        Ok(treeharbor_core::RevisionRef::Git(oid @ treeharbor_core::GitOid::Sha1(b)))
            if b != [0; 20] && treeharbor_core::RevisionRef::Git(oid).to_string() == value =>
        {
            Ok(())
        }
        _ => Err(Error::Invalid("pull Git revision")),
    }
}
pub(super) fn text(title: &str, body: &str) -> Result<()> {
    if title.trim().is_empty()
        || title.len() > 256
        || title.chars().any(char::is_control)
        || body.len() > 9000
        || body.contains('\0')
    {
        return Err(Error::Invalid("pull text"));
    }
    Ok(())
}
impl PullSpec {
    pub(super) fn validate(&self) -> Result<(Uuid, Option<Uuid>)> {
        let publication = id(&self.publication)?;
        let previous = self.previous.as_deref().map(id).transpose()?;
        text(&self.title, &self.body)?;
        if self.body.len() > 8192
            || self.body.contains("treeharbor-pull:")
            || !(1..=3600).contains(&self.ttl_seconds)
            || self.base.is_empty()
            || self.base.len() > 128
            || self.base.split('/').any(|p| {
                p.is_empty()
                    || p.len() > 64
                    || !p.as_bytes()[0].is_ascii_alphanumeric()
                    || p.ends_with('.')
                    || p.ends_with(".lock")
                    || p.contains("..")
                    || !p
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
            })
        {
            return Err(Error::Invalid("pull specification"));
        }
        Ok((publication, previous))
    }
}
impl PullIdentity {
    pub(super) fn validate(&self) -> Result<()> {
        if self.number == 0
            || self.number > i64::MAX as u64
            || self.node_id.is_empty()
            || self.node_id.len() > 256
            || !self
                .node_id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"=_-".contains(&c))
        {
            return Err(Error::Invalid("pull provider identity"));
        }
        Ok(())
    }
}
impl PullTarget {
    pub(super) fn head(&self) -> String {
        format!("treeharbor/{}", self.branch_id.simple())
    }
    pub(super) fn matches(&self, p: &PullSnapshot) -> bool {
        p.head_revision == self.git_revision
            && p.title == self.title
            && p.body == self.body
            && p.draft == self.draft
    }
}
impl PullSnapshot {
    pub(super) fn validate(&self, target: &PullTarget, spec: &BindingSpec) -> Result<()> {
        self.identity.validate()?;
        git(&self.head_revision)?;
        git(&self.base_revision)?;
        text(&self.title, &self.body)?;
        if self.head_repository != spec.upstream.repository_id
            || self.base_repository != spec.upstream.repository_id
            || self.head_ref != target.head()
            || self.base_ref != target.base
            || !matches!(self.state.as_str(), "open" | "closed")
            || (self.merged && self.state != "closed")
            || target
                .previous
                .as_ref()
                .is_some_and(|p| p.identity != self.identity)
        {
            return Err(Error::HostPullState);
        }
        Ok(())
    }
}
