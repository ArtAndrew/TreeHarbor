//! Exact, owner-authorized check publication. A receipt is not a merge proof.
use super::{groups::*, publication, queue::QueueTarget, *};
use sha2::{Digest, Sha256};
use treeharbor_core::ObjectId;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdmissionContext {
    pub binding: Uuid,
    pub generation: i64,
    pub policy: String,
}
impl AdmissionContext {
    fn validate(&self) -> Result<()> {
        if self.binding.is_nil() || self.generation <= 0 {
            return Err(InvalidEvidence);
        }
        digest(&self.policy)
    }
}

/// Policy generation is scoped to binding/epoch/base. Policy digest is retained
/// in evidence, but not hashed into a name that may itself occur in host rules.
pub fn context_name(
    epoch: i64,
    program: &str,
    base: &str,
    admission: Option<&AdmissionContext>,
) -> Result<String> {
    if epoch <= 0 {
        return Err(InvalidEvidence);
    }
    digest(program)?;
    if let Some(a) = admission {
        a.validate()?;
        text(base, 128)?;
        let mut h = Sha256::new();
        h.update(b"treeharbor-github-admission-context\0v1\0");
        h.update(
            serde_json::to_vec(&(epoch, a.binding.to_string(), base, a.generation, program))
                .map_err(|_| InvalidEvidence)?,
        );
        Ok(format!(
            "TreeHarbor/e{epoch}/a{}",
            ObjectId::from_bytes(h.finalize().into()).hex()
        ))
    } else {
        Ok(format!("TreeHarbor/e{epoch}/{}", &program[7..]))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Source {
    Pull {
        target: InspectionTarget,
    },
    Group {
        queue: Box<QueueTarget>,
        target: Box<publication::Target>,
    },
}
impl Source {
    pub fn pull(&self) -> &InspectionTarget {
        match self {
            Self::Pull { target } => target,
            Self::Group { queue, .. } => &queue.pull,
        }
    }
    pub fn head(&self) -> Result<&str> {
        match self {
            Self::Pull { target } => Ok(&target.head_revision),
            Self::Group { target, .. } => Ok(&target.candidate()?.commit.revision),
        }
    }
    pub fn validate(&self) -> Result<()> {
        match self {
            Self::Pull { target } => target.validate(),
            Self::Group { queue, target } => target.validate(queue),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Conclusion {
    Success,
    Failure,
    Cancelled,
    ActionRequired,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Target {
    pub version: u8,
    pub publication: Uuid,
    pub epoch: i64,
    pub app_id: u64,
    pub program: String,
    pub evidence: String,
    pub conclusion: Conclusion,
    pub source: Source,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission: Option<AdmissionContext>,
}
fn digest(value: &str) -> Result<()> {
    let id: ObjectId = value.parse().map_err(|_| InvalidEvidence)?;
    if id.to_string() != value {
        return Err(InvalidEvidence);
    }
    Ok(())
}
impl Target {
    pub fn validate(&self) -> Result<()> {
        if !matches!((self.version, &self.admission), (1, None) | (2, Some(_)))
            || self.publication.is_nil()
            || self.epoch <= 0
        {
            return Err(InvalidEvidence);
        }
        positive(self.app_id)?;
        digest(&self.program)?;
        digest(&self.evidence)?;
        if let Some(a) = &self.admission {
            a.validate()?;
        }
        self.source.validate()?;
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 32768 {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    /// A changed program/authority requires explicit required-context rotation.
    pub fn name(&self) -> Result<String> {
        self.validate()?;
        context_name(
            self.epoch,
            &self.program,
            &self.source.pull().base_ref,
            self.admission.as_ref(),
        )
    }
    pub fn external_id(&self) -> Result<String> {
        self.validate()?;
        Ok(format!(
            "treeharbor:v1:{}:{}",
            self.publication,
            &self.evidence[7..]
        ))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Protection {
    pub sha256: String,
    pub app_bound_checks: Vec<RequiredCheck>,
    pub enforce_admins: bool,
    pub allow_force_pushes: bool,
    pub allow_deletions: bool,
    pub has_bypass: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub protection: Protection,
    pub rules: RuleObservation,
}
impl Policy {
    fn validate(&self) -> Result<()> {
        digest(&self.protection.sha256)?;
        self.rules.validate()?;
        if self.protection.app_bound_checks.len() > 128 {
            return Err(InvalidEvidence);
        }
        let mut seen = BTreeSet::new();
        for c in &self.protection.app_bound_checks {
            c.validate()?;
            if !seen.insert(&c.name) {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
    pub fn enforces(&self, t: &Target) -> bool {
        self.validate().is_ok()
            && t.validate().is_ok()
            && self.protection.enforce_admins
            && !self.protection.allow_force_pushes
            && !self.protection.allow_deletions
            && !self.protection.has_bypass
            && self.rules.merge_queue
            && self
                .protection
                .app_bound_checks
                .iter()
                .any(|c| c.app_id == t.app_id && Some(c.name.as_str()) == t.name().ok().as_deref())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    content = "snapshot",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum SourceView {
    Pull(PullState),
    Group(Box<GroupSnapshot>),
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Preflight {
    pub before: SourceView,
    pub policy_before: Policy,
    pub after: SourceView,
    pub policy_after: Policy,
}
impl Preflight {
    pub fn validate(&self, t: &Target) -> Result<()> {
        t.validate()?;
        self.policy_before.validate()?;
        self.policy_after.validate()?;
        match (&t.source, &self.before, &self.after) {
            (Source::Pull { target }, SourceView::Pull(before), SourceView::Pull(after)) => {
                before.validate(target)?;
                after.validate(target)?;
            }
            (Source::Group { queue, .. }, SourceView::Group(before), SourceView::Group(after)) => {
                GroupCapture {
                    before: *before.clone(),
                    after: *after.clone(),
                }
                .validate(queue)?;
            }
            _ => return Err(InvalidEvidence),
        }
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 128 * 1024 {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn eligible(&self, t: &Target) -> bool {
        if self.validate(t).is_err()
            || self.before != self.after
            || self.policy_before != self.policy_after
            || !self.policy_after.enforces(t)
        {
            return false;
        }
        match (&t.source, &self.after) {
            (Source::Pull { target }, SourceView::Pull(p)) => {
                p.state == "open" && !p.merged && p.head_revision == target.head_revision
            }
            (Source::Group { queue, target }, SourceView::Group(g)) => {
                g.target.pull.state == "open"
                    && !g.target.pull.merged
                    && !g.target.pull.draft
                    && self.policy_after.rules == queue.expected_rules
                    && target
                        .validate_source(
                            queue,
                            &GroupCapture {
                                before: *g.clone(),
                                after: *g.clone(),
                            },
                        )
                        .is_ok()
            }
            _ => false,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PublishedCheck {
    pub id: u64,
    /// Opaque GraphQL identity returned alongside `id` by authenticated REST.
    /// Legacy receipts have no inferred node identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub app_id: u64,
    pub name: String,
    pub head_revision: String,
    pub external_id: String,
    pub status: String,
    pub conclusion: Conclusion,
}
impl PublishedCheck {
    pub fn validate(&self, t: &Target) -> Result<()> {
        positive(self.id)?;
        if let Some(id) = &self.node_id {
            text(id, 256)?;
            if !id.is_ascii() || id.bytes().any(|b| b.is_ascii_whitespace()) {
                return Err(InvalidEvidence);
            }
        }
        t.validate()?;
        if self.app_id != t.app_id
            || self.name != t.name()?
            || self.head_revision != t.source.head()?
            || self.external_id != t.external_id()?
            || self.status != "completed"
            || self.conclusion != t.conclusion
        {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Observed,
    Unknown,
    NotSent,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub version: u8,
    pub started_at_unix_ms: i64,
    pub finished_at_unix_ms: i64,
    pub outcome: Outcome,
    pub preflight: Option<Preflight>,
    pub check: Option<PublishedCheck>,
    pub token_revoked: bool,
}
impl Report {
    pub fn validate(&self, t: &Target) -> Result<()> {
        t.validate()?;
        if !matches!(self.version, 1 | 2)
            || self.started_at_unix_ms <= 0
            || self.finished_at_unix_ms < self.started_at_unix_ms
            || self.finished_at_unix_ms - self.started_at_unix_ms > 300000
            || (self.outcome == Outcome::Observed) != self.check.is_some()
        {
            return Err(InvalidEvidence);
        }
        if let Some(p) = &self.preflight {
            p.validate(t)?;
        }
        if let Some(c) = &self.check {
            if (self.version == 2) != c.node_id.is_some() {
                return Err(InvalidEvidence);
            }
            c.validate(t)?;
        }
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > 192 * 1024 {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}
