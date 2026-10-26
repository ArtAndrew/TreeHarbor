//! Complete bounded provider observations, not policy approval or merge authority.
use crate::{groups::*, publication::Target, queue::*, *};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

// Leave room for the authenticated command envelope under the service's 64 KiB
// request cap. Oversized captures fail; no rule/thread is silently truncated.
pub const MAX_REPORT_BYTES: usize = 48 * 1024;
pub const MAX_RULESETS: usize = 32;
pub const MAX_THREADS: usize = 1000;

/// Missing/denied responses have no inferred empty document. Response bodies from
/// failure pages are deliberately not retained (they may contain diagnostics).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub status: u16,
    pub value: Option<Value>,
}
impl Document {
    fn validate(&self) -> Result<()> {
        match (self.status, &self.value) {
            (200, Some(v)) if v.is_object() => bounded(v, 64 * 1024),
            (403 | 404, None) => Ok(()),
            _ => Err(InvalidEvidence),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct RuleSource {
    pub id: u64,
    pub kind: String,
    pub name: String,
}
impl RuleSource {
    pub fn from_rule(value: &Value, repository: &str) -> Result<Self> {
        let source = source(value)?;
        source.validate(repository)?;
        Ok(source)
    }
    fn validate(&self, repository: &str) -> Result<()> {
        positive(self.id)?;
        text(&self.name, 256)?;
        let owner = repository.split('/').next().ok_or(InvalidEvidence)?;
        match self.kind.as_str() {
            "Repository" if self.name.eq_ignore_ascii_case(repository) => Ok(()),
            "Organization" if self.name.eq_ignore_ascii_case(owner) => Ok(()),
            "Enterprise"
                if self
                    .name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-') =>
            {
                Ok(())
            }
            _ => Err(InvalidEvidence),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Ruleset {
    pub source: RuleSource,
    pub document: Document,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewThread {
    pub id: String,
    pub resolved: bool,
    pub outdated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Review {
    pub pull: PullState,
    pub decision: Option<String>,
    /// Complete paginated connection, not a selected sample of visible threads.
    pub threads: Vec<ReviewThread>,
}
impl Review {
    fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.pull.validate(&t.pull)?;
        if self.decision.as_ref().is_some_and(|v| {
            !matches!(
                v.as_str(),
                "APPROVED" | "CHANGES_REQUESTED" | "REVIEW_REQUIRED"
            )
        }) || self.threads.len() > MAX_THREADS
        {
            return Err(InvalidEvidence);
        }
        let mut seen = BTreeSet::new();
        for thread in &self.threads {
            node(&thread.id)?;
            if !seen.insert(&thread.id) {
                return Err(InvalidEvidence);
            }
        }
        Ok(())
    }
}

/// Raw documents preserve provider fields we do not yet interpret. Their presence
/// alone never establishes support for every rule or review condition.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub group: GroupSnapshot,
    pub protection: Document,
    pub rules: Vec<Value>,
    pub rulesets: Vec<Ruleset>,
    pub review: Review,
}

fn bounded(v: &Value, bytes: usize) -> Result<()> {
    fn depth(v: &Value, remaining: usize, nodes: &mut usize) -> Result<()> {
        if remaining == 0 || *nodes == 0 {
            return Err(InvalidEvidence);
        }
        *nodes -= 1;
        match v {
            Value::Array(a) => {
                for v in a {
                    depth(v, remaining - 1, nodes)?;
                }
            }
            Value::Object(m) => {
                for v in m.values() {
                    depth(v, remaining - 1, nodes)?;
                }
            }
            _ => (),
        }
        Ok(())
    }
    depth(v, 16, &mut 8192)?;
    if serde_json::to_vec(v).map_err(|_| InvalidEvidence)?.len() > bytes {
        return Err(InvalidEvidence);
    }
    Ok(())
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().ok_or(InvalidEvidence)
}
fn kind(v: &Value) -> Result<&str> {
    let k = string(v, "type")?;
    if k.is_empty() || k.len() > 64 || !k.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        return Err(InvalidEvidence);
    }
    Ok(k)
}
fn source(v: &Value) -> Result<RuleSource> {
    Ok(RuleSource {
        id: v["ruleset_id"].as_u64().ok_or(InvalidEvidence)?,
        kind: string(v, "ruleset_source_type")?.to_owned(),
        name: string(v, "ruleset_source")?.to_owned(),
    })
}
/// Same domain/length-prefixed encoding as the existing active-rule observation.
pub fn rules_fingerprint(rules: &[Value]) -> Result<RuleObservation> {
    if rules.len() > 100 {
        return Err(InvalidEvidence);
    }
    let mut rows = Vec::new();
    let mut seen = BTreeSet::new();
    let mut merge_queue = false;
    for r in rules {
        bounded(r, 16384)?;
        let id = r["ruleset_id"].as_u64().ok_or(InvalidEvidence)?;
        positive(id)?;
        let k = kind(r)?;
        if !seen.insert((id, k)) {
            return Err(InvalidEvidence);
        }
        merge_queue |= k == "merge_queue";
        rows.push(serde_json::to_string(r).map_err(|_| InvalidEvidence)?);
    }
    rows.sort();
    let mut h = Sha256::new();
    h.update(b"treeharbor-github-rules-v1\0");
    for row in rows {
        h.update((row.len() as u64).to_be_bytes());
        h.update(row.as_bytes());
    }
    Ok(RuleObservation {
        sha256: h.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        count: rules.len() as u32,
        merge_queue,
    })
}
impl Snapshot {
    pub fn validate(&self, repository: &str, t: &QueueTarget) -> Result<()> {
        self.group.validate(t)?;
        self.protection.validate()?;
        self.review.validate(t)?;
        rules_fingerprint(&self.rules)?;
        if self.rulesets.len() > MAX_RULESETS {
            return Err(InvalidEvidence);
        }
        let mut sources = BTreeMap::new();
        for r in &self.rules {
            let s = source(r)?;
            s.validate(repository)?;
            if sources.insert(s.id, s.clone()).is_some_and(|old| old != s) {
                return Err(InvalidEvidence);
            }
        }
        let mut seen = BTreeSet::new();
        for r in &self.rulesets {
            r.source.validate(repository)?;
            r.document.validate()?;
            if !seen.insert(r.source.id) || sources.get(&r.source.id) != Some(&r.source) {
                return Err(InvalidEvidence);
            }
            if let Some(v) = &r.document.value {
                if v["id"].as_u64() != Some(r.source.id)
                    || string(v, "source_type")? != r.source.kind
                    || !string(v, "source")?.eq_ignore_ascii_case(&r.source.name)
                    || string(v, "target")? != "branch"
                    || !matches!(
                        string(v, "enforcement")?,
                        "active" | "evaluate" | "disabled"
                    )
                {
                    return Err(InvalidEvidence);
                }
                let rules = v["rules"].as_array().ok_or(InvalidEvidence)?;
                if rules.len() > 100 {
                    return Err(InvalidEvidence);
                }
                let mut types = BTreeSet::new();
                for rule in rules {
                    bounded(rule, 16384)?;
                    if !types.insert(kind(rule)?) {
                        return Err(InvalidEvidence);
                    }
                }
                // Redaction is valid captured evidence, but not an empty bypass list.
                if let Some(actors) = v.get("bypass_actors")
                    && actors.as_array().is_none_or(|a| a.len() > 100)
                {
                    return Err(InvalidEvidence);
                }
            }
        }
        if seen != sources.keys().copied().collect() {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub version: u8,
    pub repository_name: String,
    pub target: Target,
    pub before: Snapshot,
    pub after: Snapshot,
}
impl Report {
    pub fn validate(&self, t: &QueueTarget) -> Result<()> {
        self.target.validate(t)?;
        if self.version != 1 || self.repository_name.len() > 201 {
            return Err(InvalidEvidence);
        }
        let parts: Vec<_> = self.repository_name.split('/').collect();
        if parts.len() != 2
            || parts.iter().any(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(InvalidEvidence);
        }
        self.before.validate(&self.repository_name, t)?;
        self.after.validate(&self.repository_name, t)?;
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > MAX_REPORT_BYTES {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    /// Diagnostic capture consistency only. An empty result is NOT an admission
    /// decision: rule semantics, owner pin and complete check proof are separate.
    pub fn capture_reasons(&self, t: &QueueTarget) -> Result<BTreeSet<String>> {
        self.validate(t)?;
        let mut reasons = BTreeSet::new();
        if self.before != self.after {
            reasons.insert("capture_changed".into());
        }
        let g = GroupCapture {
            before: self.before.group.clone(),
            after: self.after.group.clone(),
        };
        if self.target.validate_source(t, &g).is_err() {
            reasons.insert("candidate_changed".into());
        }
        for s in [&self.before, &self.after] {
            if s.review.pull != s.group.target.pull {
                reasons.insert("review_source_changed".into());
            }
            if rules_fingerprint(&s.rules)? != t.expected_rules {
                reasons.insert("rules_changed".into());
            }
            if s.protection.status != 200 {
                reasons.insert("legacy_protection_unavailable".into());
            }
            for rs in &s.rulesets {
                let Some(v) = &rs.document.value else {
                    reasons.insert("ruleset_unavailable".into());
                    continue;
                };
                match v.get("bypass_actors").and_then(Value::as_array) {
                    None => {
                        reasons.insert("ruleset_bypass_redacted".into());
                    }
                    Some(a) if !a.is_empty() => {
                        reasons.insert("ruleset_bypass_present".into());
                    }
                    _ => (),
                }
                if v["enforcement"] != "active" {
                    reasons.insert("ruleset_not_active".into());
                }
                let actual = v["rules"].as_array().ok_or(InvalidEvidence)?;
                let expected: Vec<_> = s
                    .rules
                    .iter()
                    .filter(|r| r["ruleset_id"].as_u64() == Some(rs.source.id))
                    .collect();
                // A branch ruleset's full set must agree with the applicable set.
                // A mixed pagination/read generation cannot silently drop a rule.
                if actual.len() != expected.len()
                    || expected.iter().any(|r| {
                        !actual
                            .iter()
                            .any(|a| a["type"] == r["type"] && a["parameters"] == r["parameters"])
                    })
                {
                    reasons.insert("ruleset_rules_changed".into());
                }
            }
        }
        Ok(reasons)
    }
    pub fn digest(&self, t: &QueueTarget) -> Result<String> {
        self.validate(t)?;
        let mut h = Sha256::new();
        h.update(b"treeharbor-github-host-policy-observation\0v1\0");
        let canonical = serde_json::to_value(self).map_err(|_| InvalidEvidence)?;
        h.update(serde_json::to_vec(&canonical).map_err(|_| InvalidEvidence)?);
        Ok(treeharbor_core::ObjectId::from_bytes(h.finalize().into()).to_string())
    }
}
