//! PR-independent configuration inspection; no owner approval or merge authority.
use crate::{
    host_policy_evaluation::{Analysis, Profile},
    queue::node,
    *,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_BYTES: usize = 48 * 1024;
pub const FLAGS: &[&str] = &[
    "allowsDeletions",
    "allowsForcePushes",
    "blocksCreations",
    "dismissesStaleReviews",
    "isAdminEnforced",
    "lockAllowsFetchAndMerge",
    "lockBranch",
    "requireLastPushApproval",
    "requiresApprovingReviews",
    "requiresCodeOwnerReviews",
    "requiresCommitSignatures",
    "requiresConversationResolution",
    "requiresDeployments",
    "requiresLinearHistory",
    "requiresStatusChecks",
    "requiresStrictStatusChecks",
    "restrictsPushes",
    "restrictsReviewDismissals",
];

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub repository_id: u64,
    pub repository_name: String,
    pub base_ref: String,
}
impl Target {
    pub fn validate(&self) -> Result<()> {
        positive(self.repository_id)?;
        branch(&self.base_ref)?;
        let parts: Vec<_> = self.repository_name.split('/').collect();
        if self.repository_name.len() > 201
            || parts.len() != 2
            || parts.iter().any(|p| {
                p.is_empty()
                    || !p
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Allowances {
    pub total_count: u32,
    pub nodes: Vec<String>,
    pub has_next_page: bool,
}
impl Allowances {
    fn validate(&self) -> Result<()> {
        if self.total_count > 100
            || self.nodes.len() != self.total_count as usize
            || self.has_next_page
        {
            return Err(InvalidEvidence);
        }
        let mut seen = BTreeSet::new();
        for id in &self.nodes {
            node(id)?;
            if !seen.insert(id) {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Legacy {
    pub id: String,
    pub repository_id: u64,
    pub flags: BTreeMap<String, bool>,
    pub approving_reviews: Option<u32>,
    pub required_contexts: Option<Vec<String>>,
    pub deployment_environments: Option<Vec<String>>,
    pub bypass_pull_requests: Allowances,
    pub bypass_force_pushes: Allowances,
}
impl Legacy {
    fn validate(&self, target: &Target) -> Result<()> {
        node(&self.id)?;
        if self.repository_id != target.repository_id
            || self
                .flags
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                != FLAGS.iter().copied().collect()
            || self.approving_reviews.is_some_and(|n| n > 100)
        {
            return Err(InvalidEvidence);
        }
        for list in [&self.required_contexts, &self.deployment_environments]
            .into_iter()
            .flatten()
        {
            if list.len() > 100 {
                return Err(InvalidEvidence);
            }
            let mut seen = BTreeSet::new();
            for value in list {
                text(value, 256)?;
                if !seen.insert(value) {
                    return Err(InvalidEvidence);
                }
            }
        }
        self.bypass_pull_requests.validate()?;
        self.bypass_force_pushes.validate()
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Visibility {
    pub target: Target,
    pub owner_kind: String,
    pub queue_id: Option<String>,
    pub legacy: Option<Legacy>,
}
impl Visibility {
    pub fn validate(&self, t: &Target) -> Result<()> {
        t.validate()?;
        if self.target != *t || !matches!(self.owner_kind.as_str(), "User" | "Organization") {
            return Err(InvalidEvidence);
        }
        if let Some(q) = &self.queue_id {
            node(q)?;
        }
        if let Some(l) = &self.legacy {
            l.validate(t)?;
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub version: u8,
    pub profile: Profile,
    pub visibility: Visibility,
}
impl Configuration {
    pub fn validate(&self, t: &Target) -> Result<()> {
        self.visibility.validate(t)?;
        self.profile.validate()?;
        if self.version != 1
            || self.visibility.target != *t
            || self.profile.repository_id != t.repository_id
            || self.profile.repository_name != t.repository_name
            || self.profile.base_ref != t.base_ref
            || !matches!(self.visibility.owner_kind.as_str(), "User" | "Organization")
        {
            return Err(InvalidEvidence);
        }
        if self
            .profile
            .queue
            .as_ref()
            .is_some_and(|q| self.visibility.queue_id.as_ref() != Some(&q.id))
        {
            return Err(InvalidEvidence);
        }
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > MAX_BYTES {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    /// Binds supplementary visibility without changing published Profile v1 bytes.
    pub fn canonical(&self, t: &Target) -> Result<Vec<u8>> {
        self.validate(t)?;
        let mut c = self.clone();
        c.profile =
            serde_json::from_slice(&self.profile.canonical()?).map_err(|_| InvalidEvidence)?;
        if let Some(l) = &mut c.visibility.legacy {
            l.bypass_pull_requests.nodes.sort();
            l.bypass_force_pushes.nodes.sort();
            if let Some(v) = &mut l.required_contexts {
                v.sort();
            }
            if let Some(v) = &mut l.deployment_environments {
                v.sort();
            }
        }
        serde_json::to_vec(&serde_json::to_value(c).map_err(|_| InvalidEvidence)?)
            .map_err(|_| InvalidEvidence)
    }
    pub fn digest(&self, t: &Target) -> Result<String> {
        let mut h = Sha256::new();
        h.update(b"treeharbor-github-host-configuration\0v1\0");
        h.update(self.canonical(t)?);
        Ok(treeharbor_core::ObjectId::from_bytes(h.finalize().into()).to_string())
    }
    pub fn analyze(&self, t: &Target) -> Result<Analysis> {
        self.validate(t)?;
        let mut a = self.profile.analyze()?;
        let reasons = &mut a.configuration_reasons;
        if self.visibility.owner_kind != "Organization" {
            reasons.insert("personal_repository_queue_unsupported".into());
        }
        let Some(l) = &self.visibility.legacy else {
            reasons.insert("legacy_visibility_unavailable".into());
            return Ok(a);
        };
        let Some(p) = &self.profile.protection.value else {
            return Ok(a);
        };
        let mut mismatch = false;
        for (rest, graph) in [
            ("allow_deletions", "allowsDeletions"),
            ("allow_force_pushes", "allowsForcePushes"),
            ("block_creations", "blocksCreations"),
            ("enforce_admins", "isAdminEnforced"),
            ("allow_fork_syncing", "lockAllowsFetchAndMerge"),
            ("lock_branch", "lockBranch"),
            ("required_signatures", "requiresCommitSignatures"),
            (
                "required_conversation_resolution",
                "requiresConversationResolution",
            ),
            ("required_linear_history", "requiresLinearHistory"),
        ] {
            if let Some(v) = p.get(rest) {
                mismatch |= v["enabled"].as_bool() != Some(l.flags[graph]);
            }
        }
        let reviews = &p["required_pull_request_reviews"];
        if p.get("required_pull_request_reviews").is_some() {
            mismatch |= reviews.is_object() != l.flags["requiresApprovingReviews"];
        }
        if reviews.is_object() {
            for (rest, graph) in [
                ("dismiss_stale_reviews", "dismissesStaleReviews"),
                ("require_code_owner_reviews", "requiresCodeOwnerReviews"),
                ("require_last_push_approval", "requireLastPushApproval"),
            ] {
                mismatch |= reviews[rest].as_bool() != Some(l.flags[graph]);
            }
            mismatch |= reviews["required_approving_review_count"].as_u64()
                != l.approving_reviews.map(u64::from);
        }
        let checks = &p["required_status_checks"];
        if p.get("required_status_checks").is_some() {
            mismatch |= checks.is_object() != l.flags["requiresStatusChecks"];
        }
        if checks.is_object() {
            mismatch |= checks["strict"].as_bool() != Some(l.flags["requiresStrictStatusChecks"]);
            let contexts = checks["contexts"]
                .as_array()
                .and_then(|v| v.iter().map(Value::as_str).collect::<Option<BTreeSet<_>>>());
            let graph = l
                .required_contexts
                .as_ref()
                .map(|v| v.iter().map(String::as_str).collect::<BTreeSet<_>>());
            mismatch |= contexts.is_none() || contexts != graph;
        }
        for (value, flag) in [
            (p.get("restrictions"), "restrictsPushes"),
            (
                reviews.get("dismissal_restrictions"),
                "restrictsReviewDismissals",
            ),
        ] {
            if let Some(v) = value {
                // An empty actor object does not establish whether the setting is
                // enabled. Only null and explicitly populated lists contradict
                // the corresponding GraphQL flag; malformed REST stays unproven.
                mismatch |= (v.is_null() && l.flags[flag])
                    || (!l.flags[flag]
                        && ["users", "teams", "apps"]
                            .iter()
                            .any(|k| v[*k].as_array().is_some_and(|a| !a.is_empty())));
            }
        }
        if let Some(b) = reviews.get("bypass_pull_request_allowances") {
            let count: Option<usize> = ["users", "teams", "apps"]
                .into_iter()
                .map(|k| b[k].as_array().map(Vec::len))
                .sum();
            mismatch |= count != Some(l.bypass_pull_requests.total_count as usize);
        }
        if mismatch {
            reasons.insert("legacy_rest_graphql_disagree".into());
        }
        if l.bypass_pull_requests.total_count > 0 {
            reasons.insert("legacy_pr_bypass_present".into());
        }
        if l.bypass_force_pushes.total_count > 0 {
            reasons.insert("legacy_force_push_bypass_present".into());
        }
        if l.flags["restrictsPushes"] {
            reasons.insert("legacy_push_restrictions_unproven".into());
        }
        if l.flags["restrictsReviewDismissals"] {
            reasons.insert("legacy_review_dismissal_restrictions_unproven".into());
        }
        if l.flags["requiresDeployments"] {
            a.candidate_requirements
                .insert("required_deployments".into());
            if l.deployment_environments.as_ref().is_none_or(Vec::is_empty) {
                reasons.insert("legacy_deployment_environments_unavailable".into());
            }
        }
        // Only explicitly observed absence can discharge these specific missing
        // fields. Present malformed data and unrelated/unknown clauses stay blocked.
        if !mismatch {
            if p.get("restrictions").is_none() && !l.flags["restrictsPushes"] {
                reasons.remove("legacy/restrictions_unavailable");
            }
            if reviews.is_object()
                && reviews.get("bypass_pull_request_allowances").is_none()
                && l.bypass_pull_requests.total_count == 0
            {
                for suffix in [
                    "object_unproven",
                    "users/actors_unproven",
                    "teams/actors_unproven",
                    "apps/actors_unproven",
                ] {
                    reasons.remove(&format!("legacy/reviews/bypass/{suffix}"));
                }
            }
        }
        Ok(a)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub version: u8,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub before: Box<Configuration>,
    pub after: Box<Configuration>,
    pub token_revoked: bool,
}
impl Inspection {
    pub fn validate(&self, t: &Target) -> Result<()> {
        self.before.validate(t)?;
        self.after.validate(t)?;
        if self.version != 1
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 120_000
            || serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > MAX_BYTES
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn canonical(&self, t: &Target) -> Result<Vec<u8>> {
        self.validate(t)?;
        let mut value = serde_json::to_value(self).map_err(|_| InvalidEvidence)?;
        value["before"] =
            serde_json::from_slice(&self.before.canonical(t)?).map_err(|_| InvalidEvidence)?;
        value["after"] =
            serde_json::from_slice(&self.after.canonical(t)?).map_err(|_| InvalidEvidence)?;
        serde_json::to_vec(&value).map_err(|_| InvalidEvidence)
    }
    pub fn digest(&self, t: &Target) -> Result<String> {
        let mut h = Sha256::new();
        h.update(b"treeharbor-github-host-configuration-inspection\0v1\0");
        h.update(self.canonical(t)?);
        Ok(treeharbor_core::ObjectId::from_bytes(h.finalize().into()).to_string())
    }
    pub fn reasons(&self, t: &Target) -> Result<BTreeSet<String>> {
        self.validate(t)?;
        let mut r = self.before.analyze(t)?.configuration_reasons;
        r.extend(self.after.analyze(t)?.configuration_reasons);
        if self.before.digest(t)? != self.after.digest(t)? {
            r.insert("configuration_changed".into());
        }
        if !self.token_revoked {
            r.insert("token_revocation_unproven".into());
        }
        Ok(r)
    }
}
