//! Pure evidence evaluation. A satisfied check set is NOT permission to merge.
//! Authenticated collectors, review, policy reads and the final ref CAS must be
//! implemented in the service and checked in the publication transaction.

use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use treeharbor_core::{ObjectId, RevisionRef};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// Canonical service scope, including tenant and repository identity.
    pub repository_scope: String,
    pub authority_epoch: u64,
    pub revision: RevisionRef,
    pub snapshot: ObjectId,
    pub target_ref: String,
    pub expected_head: RevisionRef,
    pub expected_generation: u64,
    pub policy_digest: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentTarget {
    pub repository_scope: String,
    pub authority_epoch: u64,
    pub target_ref: String,
    pub head: RevisionRef,
    pub generation: u64,
    pub policy_digest: [u8; 32],
    pub active: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: String,
    /// The trusted scheduler's current run. Starting another run invalidates
    /// evidence from the previous one, even if the source tree is unchanged.
    pub run_id: String,
    pub definition_digest: [u8; 32],
    pub environment_digest: [u8; 32],
    pub toolchain_digest: [u8; 32],
    pub dependency_digest: [u8; 32],
    pub trusted_validators: BTreeSet<String>,
}

/// An evidence-context digest is deliberately not a source ObjectId or Git OID.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContextDigest([u8; 32]);
impl ContextDigest {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

pub fn context_digest(candidate: &Candidate, check: &Check) -> ContextDigest {
    let mut digest = Sha256::new();
    digest.update(b"treeharbor-validation-context\0v1\0");
    fn field(digest: &mut Sha256, bytes: &[u8]) {
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    field(&mut digest, candidate.repository_scope.as_bytes());
    field(&mut digest, &candidate.authority_epoch.to_be_bytes());
    field(&mut digest, candidate.revision.to_string().as_bytes());
    field(&mut digest, candidate.snapshot.as_bytes());
    field(&mut digest, candidate.target_ref.as_bytes());
    field(&mut digest, candidate.expected_head.to_string().as_bytes());
    field(&mut digest, &candidate.expected_generation.to_be_bytes());
    field(&mut digest, &candidate.policy_digest);
    field(&mut digest, check.name.as_bytes());
    field(&mut digest, check.run_id.as_bytes());
    field(&mut digest, &check.definition_digest);
    field(&mut digest, &check.environment_digest);
    field(&mut digest, &check.toolchain_digest);
    field(&mut digest, &check.dependency_digest);
    // Bind the allowed issuer set too; a policy digest should cover it, but a
    // misconfigured caller must not accidentally retain the old evidence.
    field(
        &mut digest,
        &(check.trusted_validators.len() as u64).to_be_bytes(),
    );
    for validator in &check.trusted_validators {
        field(&mut digest, validator.as_bytes());
    }
    ContextDigest(digest.finalize().into())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Passed,
    Failed,
    Error,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub id: String,
    pub context: ContextDigest,
    /// Must originate from the authenticated collector, not an agent JSON field.
    pub validator: String,
    pub outcome: Outcome,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Blocker {
    InvalidPolicy,
    StaleTarget,
    Missing { check: String },
    Untrusted { check: String },
    Revoked { check: String },
    Conflicting { check: String },
    Unsuccessful { check: String, outcome: Outcome },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckDecision {
    /// Only the check predicate is satisfied. Review/permissions/atomic CAS are
    /// separate mandatory admission conditions.
    Satisfied,
    Blocked(Vec<Blocker>),
}

/// Inputs must be a consistent, trusted policy/evidence snapshot. No wall-clock
/// ordering or "any old green run" rule is used. Unknown/ambiguous results block.
pub fn evaluate_checks(
    candidate: &Candidate,
    target: &CurrentTarget,
    checks: &[Check],
    receipts: &[Receipt],
    revoked_receipts: &BTreeSet<String>,
) -> CheckDecision {
    if !valid_policy(candidate, checks) || receipts.len() > 4096 || revoked_receipts.len() > 4096 {
        return CheckDecision::Blocked(vec![Blocker::InvalidPolicy]);
    }
    if !target.active
        || candidate.repository_scope != target.repository_scope
        || candidate.authority_epoch != target.authority_epoch
        || candidate.target_ref != target.target_ref
        || candidate.expected_head != target.head
        || candidate.expected_generation != target.generation
        || candidate.policy_digest != target.policy_digest
    {
        return CheckDecision::Blocked(vec![Blocker::StaleTarget]);
    }
    let mut blockers = Vec::new();
    for check in checks {
        let context = context_digest(candidate, check);
        let relevant: Vec<_> = receipts.iter().filter(|r| r.context == context).collect();
        let mut unique = BTreeMap::new();
        let mut conflicting = false;
        let mut saw_revoked = false;
        let mut saw_untrusted = false;
        for receipt in relevant {
            if !check.trusted_validators.contains(&receipt.validator) {
                saw_untrusted = true;
                continue;
            }
            if revoked_receipts.contains(&receipt.id) {
                saw_revoked = true;
                continue;
            }
            if !valid_identifier(&receipt.id) {
                conflicting = true;
                continue;
            }
            if let Some(previous) = unique.insert(&receipt.id, receipt) {
                conflicting |= previous != receipt;
            }
        }
        // One definitive receipt per current scheduled check run. Identical
        // delivery retries deduplicate; competing authoritative outcomes block.
        let blocker = if conflicting || unique.len() > 1 {
            Some(Blocker::Conflicting {
                check: check.name.clone(),
            })
        } else if let Some(receipt) = unique.values().next() {
            match receipt.outcome {
                Outcome::Passed => None,
                outcome => Some(Blocker::Unsuccessful {
                    check: check.name.clone(),
                    outcome,
                }),
            }
        } else if saw_revoked {
            Some(Blocker::Revoked {
                check: check.name.clone(),
            })
        } else if saw_untrusted {
            Some(Blocker::Untrusted {
                check: check.name.clone(),
            })
        } else {
            Some(Blocker::Missing {
                check: check.name.clone(),
            })
        };
        if let Some(blocker) = blocker {
            blockers.push(blocker);
        }
    }
    if blockers.is_empty() {
        CheckDecision::Satisfied
    } else {
        CheckDecision::Blocked(blockers)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.bytes().all(|b| b.is_ascii_graphic())
}
fn valid_policy(candidate: &Candidate, checks: &[Check]) -> bool {
    if !valid_identifier(&candidate.repository_scope)
        || !valid_identifier(&candidate.target_ref)
        || candidate.authority_epoch == 0
        || checks.is_empty()
        || checks.len() > 128
    {
        return false;
    }
    let mut names = BTreeSet::new();
    checks.iter().all(|check| {
        valid_identifier(&check.name)
            && valid_identifier(&check.run_id)
            && names.insert(&check.name)
            && !check.trusted_validators.is_empty()
            && check.trusted_validators.len() <= 64
            && check.trusted_validators.iter().all(|p| valid_identifier(p))
    })
}
