//! Versioned configuration identity and conservative host-policy evaluation.
//! Neither configuration support nor candidate diagnostics authorize publication.
use crate::{host_policy as hp, queue::*, *};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

mod clauses;
use clauses::*;

pub const MAX_PROFILE_BYTES: usize = hp::MAX_REPORT_BYTES;

/// Independent of a PR or queue entry so configuration approval can be bootstrapped
/// before the first candidate exists. All raw provider policy fields are retained.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub version: u8,
    pub repository_id: u64,
    pub repository_name: String,
    pub base_ref: String,
    pub protection: hp::Document,
    pub rules: Vec<Value>,
    pub rulesets: Vec<hp::Ruleset>,
    pub queue: Option<QueueConfiguration>,
}
impl Profile {
    pub fn validate(&self) -> Result<()> {
        positive(self.repository_id)?;
        branch(&self.base_ref)?;
        let parts: Vec<_> = self.repository_name.split('/').collect();
        if self.version != 1
            || self.repository_name.len() > 201
            || parts.len() != 2
            || parts.iter().any(|s| {
                s.is_empty()
                    || !s
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
            })
        {
            return Err(InvalidEvidence);
        }
        hp::validate_documents(
            &self.repository_name,
            &self.protection,
            &self.rules,
            &self.rulesets,
        )?;
        if let Some(q) = &self.queue {
            q.validate()?;
        }
        if serde_json::to_vec(self).map_err(|_| InvalidEvidence)?.len() > MAX_PROFILE_BYTES {
            return Err(InvalidEvidence);
        }
        Ok(())
    }
    pub fn from_snapshot(repository: &str, t: &QueueTarget, s: &hp::Snapshot) -> Result<Self> {
        s.validate(repository, t)?;
        let p = Self {
            version: 1,
            repository_id: t.pull.repository_id,
            repository_name: repository.to_owned(),
            base_ref: t.pull.base_ref.clone(),
            protection: s.protection.clone(),
            rules: s.rules.clone(),
            rulesets: s.rulesets.clone(),
            queue: s.group.target.configuration.clone(),
        };
        p.validate()?;
        Ok(p)
    }
    /// Sort only the two explicitly unordered top-level collections. Nested arrays
    /// and all metadata remain significant; JSON object keys use lexical order.
    pub fn canonical(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut p = self.clone();
        p.rules.sort_by_cached_key(Value::to_string);
        p.rulesets.sort_by_key(|r| r.source.id);
        let v = serde_json::to_value(p).map_err(|_| InvalidEvidence)?;
        serde_json::to_vec(&v).map_err(|_| InvalidEvidence)
    }
    pub fn digest(&self) -> Result<String> {
        let mut h = Sha256::new();
        h.update(b"treeharbor-github-host-policy-configuration\0v1\0");
        h.update(self.canonical()?);
        Ok(treeharbor_core::ObjectId::from_bytes(h.finalize().into()).to_string())
    }
    pub fn analyze(&self) -> Result<Analysis> {
        self.validate()?;
        let mut a = Analysis::default();
        if let Some(p) = &self.protection.value {
            legacy(p, &mut a);
        } else {
            a.reason("legacy_protection_unavailable");
        }
        for rs in &self.rulesets {
            let source = format!("ruleset/{}", rs.source.id);
            let Some(v) = &rs.document.value else {
                a.reason(&format!("{source}/unavailable"));
                continue;
            };
            fields(
                v,
                &[
                    "id",
                    "name",
                    "target",
                    "source_type",
                    "source",
                    "enforcement",
                    "bypass_actors",
                    "conditions",
                    "rules",
                    "node_id",
                    "created_at",
                    "updated_at",
                    "_links",
                ],
                &source,
                &mut a,
            );
            if v["enforcement"] != "active" {
                a.reason(&format!("{source}/inactive"));
            }
            match v.get("bypass_actors").and_then(Value::as_array) {
                Some(b) if b.is_empty() => (),
                Some(_) => a.reason(&format!("{source}/bypass_present")),
                None => a.reason(&format!("{source}/bypass_redacted")),
            }
            // Applicability is supplied by the provider's effective branch-rules
            // endpoint, not a local reimplementation of GitHub pattern matching.
            conditions(&v["conditions"], &format!("{source}/conditions"), &mut a);
            let actual = v["rules"].as_array().ok_or(InvalidEvidence)?;
            let expected: Vec<_> = self
                .rules
                .iter()
                .filter(|r| r["ruleset_id"] == rs.source.id)
                .collect();
            if actual.len() != expected.len()
                || expected.iter().any(|r| {
                    !actual
                        .iter()
                        .any(|v| v["type"] == r["type"] && v["parameters"] == r["parameters"])
                })
            {
                a.reason(&format!("{source}/rules_changed"));
            }
            for r in actual {
                fields(
                    r,
                    &["type", "parameters"],
                    &format!("{source}/rule"),
                    &mut a,
                );
            }
        }
        let mut queues = Vec::new();
        for r in &self.rules {
            let source = format!("ruleset/{}", r["ruleset_id"]);
            fields(
                r,
                &[
                    "type",
                    "parameters",
                    "ruleset_id",
                    "ruleset_source",
                    "ruleset_source_type",
                ],
                &source,
                &mut a,
            );
            let kind = r["type"].as_str().ok_or(InvalidEvidence)?;
            let path = format!("{source}/{kind}");
            match kind {
                "merge_queue" => {
                    match serde_json::from_value::<QueueParameters>(r["parameters"].clone()) {
                        Ok(q) => queues.push(q),
                        Err(_) => a.reason(&format!("{path}/parameters_unproven")),
                    }
                }
                "required_status_checks" => status_rule(&r["parameters"], &path, &mut a),
                "pull_request" => review_rule(&r["parameters"], &path, &mut a),
                "non_fast_forward"
                | "deletion"
                | "creation"
                | "required_signatures"
                | "required_linear_history" => {
                    if r.get("parameters")
                        .is_some_and(|p| p.as_object().is_none_or(|m| !m.is_empty()))
                    {
                        a.reason(&format!("{path}/parameters_unproven"));
                    }
                    if matches!(kind, "required_signatures" | "required_linear_history") {
                        a.candidate_requirements.insert(kind.to_owned());
                    }
                }
                _ => a.reason(&format!("{path}/rule_unproven")),
            }
        }
        match (&self.queue, queues.as_slice()) {
            (Some(q), [r]) if r.matches(q) => {
                if q.merge_method != "MERGE" {
                    a.candidate_requirements
                        .insert("non_merge_publication".into());
                }
                if q.merging_strategy != "ALLGREEN" {
                    a.candidate_requirements
                        .insert("headgreen_group_checks".into());
                }
            }
            _ => a.reason("queue_configuration_unproven"),
        }
        let names: BTreeSet<_> = a.required.iter().map(|r| &r.name).collect();
        if names.len() != a.required.len() {
            a.reason("check_name_multiple_issuers");
        }
        if a.required.is_empty() {
            a.reason("required_checks_empty");
        }
        a.reviews.sort_by(|a, b| a.source.cmp(&b.source));
        Ok(a)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Analysis {
    pub required: BTreeSet<RequiredCheck>,
    pub reviews: Vec<ReviewRequirement>,
    pub configuration_reasons: BTreeSet<String>,
    pub candidate_requirements: BTreeSet<String>,
}
impl Analysis {
    fn reason(&mut self, reason: &str) {
        self.configuration_reasons.insert(reason.to_owned());
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequirement {
    pub source: String,
    pub approvals: u32,
    pub code_owners: bool,
    pub last_push: bool,
    pub dismiss_stale: bool,
    pub resolve_threads: bool,
    pub required_teams: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub version: u8,
    pub before_digest: String,
    pub after_digest: String,
    pub before: Analysis,
    pub after: Analysis,
    pub reasons: BTreeSet<String>,
}
/// Historical analysis only. This does not check owner authorization/currentness,
/// trusted CI results, workflow execution context or final publication.
pub fn evaluate(report: &hp::Report, t: &QueueTarget) -> Result<Evaluation> {
    let mut reasons = report.capture_reasons(t)?;
    let p = Profile::from_snapshot(&report.repository_name, t, &report.before)?;
    let q = Profile::from_snapshot(&report.repository_name, t, &report.after)?;
    let analyze = |s: &hp::Snapshot, p: &Profile| -> Result<(String, Analysis)> {
        if let Some(c) = s.configuration(&report.repository_name, t)? {
            Ok((
                c.digest(&c.visibility.target)?,
                c.analyze(&c.visibility.target)?,
            ))
        } else {
            Ok((p.digest()?, p.analyze()?))
        }
    };
    let (before_digest, before) = analyze(&report.before, &p)?;
    let (after_digest, after) = analyze(&report.after, &q)?;
    if before_digest != after_digest {
        reasons.insert("configuration_changed".into());
    }
    for (a, s) in [(&before, &report.before), (&after, &report.after)] {
        reasons.extend(a.configuration_reasons.iter().cloned());
        reasons.extend(
            a.candidate_requirements
                .iter()
                .map(|r| format!("{r}_unproven")),
        );
        if a.required != t.pull.required.iter().cloned().collect() {
            reasons.insert("required_check_set_changed".into());
        }
        if s.group.members.len() != 1 {
            reasons.insert("multi_member_policy_evidence_unproven".into());
        }
        if s.review.pull.head_revision != t.pull.head_revision
            || s.review.pull.state != "open"
            || s.review.pull.merged
            || s.review.pull.draft
        {
            reasons.insert("review_head_not_current_open_pr".into());
        }
        if matches!(
            s.review.decision.as_deref(),
            Some("CHANGES_REQUESTED" | "REVIEW_REQUIRED")
        ) {
            reasons.insert("review_not_approved".into());
        }
        for r in &a.reviews {
            if r.resolve_threads && s.review.threads.iter().any(|t| !t.resolved) {
                reasons.insert("review_threads_unresolved".into());
            }
            // APPROVED does not count distinct approvals or establish code-owner,
            // required-team, last-push or stale-dismissal conditions.
            if r.approvals > 0 || r.code_owners || r.last_push || r.required_teams {
                reasons.insert(format!("{}/review_approvals_unproven", r.source));
            }
        }
    }
    Ok(Evaluation {
        version: report.version,
        before_digest,
        after_digest,
        before,
        after,
        reasons,
    })
}
