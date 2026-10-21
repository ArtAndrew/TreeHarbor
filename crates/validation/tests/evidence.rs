use std::collections::BTreeSet;
use treeharbor_core::{GitOid, ObjectId, RevisionRef};
use treeharbor_validation::*;

fn native(n: u8) -> RevisionRef {
    RevisionRef::Native(ObjectId::from_bytes([n; 32]))
}
fn fixture() -> (Candidate, CurrentTarget, Check, Receipt) {
    let candidate = Candidate {
        repository_scope: "tenant/repo".into(),
        authority_epoch: 1,
        revision: native(1),
        snapshot: ObjectId::from_bytes([2; 32]),
        target_ref: "main".into(),
        expected_head: native(3),
        expected_generation: 4,
        policy_digest: [5; 32],
    };
    let target = current(&candidate);
    let check = Check {
        name: "build".into(),
        run_id: "run-1".into(),
        definition_digest: [6; 32],
        environment_digest: [7; 32],
        toolchain_digest: [8; 32],
        dependency_digest: [9; 32],
        trusted_validators: BTreeSet::from(["ci-service".into()]),
    };
    let receipt = Receipt {
        id: "receipt-1".into(),
        context: context_digest(&candidate, &check),
        validator: "ci-service".into(),
        outcome: Outcome::Passed,
    };
    (candidate, target, check, receipt)
}
fn current(c: &Candidate) -> CurrentTarget {
    CurrentTarget {
        repository_scope: c.repository_scope.clone(),
        authority_epoch: c.authority_epoch,
        target_ref: c.target_ref.clone(),
        head: c.expected_head,
        generation: c.expected_generation,
        policy_digest: c.policy_digest,
        active: true,
    }
}
fn check(c: &Candidate, t: &CurrentTarget, k: &Check, r: &[Receipt]) -> CheckDecision {
    evaluate_checks(c, t, std::slice::from_ref(k), r, &BTreeSet::new())
}

#[test]
fn exact_result_passes_and_identical_delivery_retries_deduplicate() {
    let (c, t, k, r) = fixture();
    assert_eq!(check(&c, &t, &k, &[r.clone(), r]), CheckDecision::Satisfied);
}

#[test]
fn canonical_context_matches_independent_hashlib_vector() {
    let (candidate, _, check, _) = fixture();
    // Python hashlib.sha256 over domain + big-endian u64 length-prefixed fields.
    let expected: ObjectId =
        "sha256:0bbda841c38c1d76ba0a1de6307e91bfe0d13c488a7bd6fbdf112710c650e888"
            .parse()
            .unwrap();
    assert_eq!(
        context_digest(&candidate, &check).as_bytes(),
        expected.as_bytes()
    );
}
#[test]
fn every_candidate_context_field_invalidates_old_evidence_even_when_target_is_updated() {
    let (c, _, k, r) = fixture();
    let mut variants = Vec::new();
    let mut x = c.clone();
    x.repository_scope = "other/repo".into();
    variants.push(x);
    let mut x = c.clone();
    x.authority_epoch += 1;
    variants.push(x);
    let mut x = c.clone();
    x.revision = native(10);
    variants.push(x);
    let mut x = c.clone();
    x.snapshot = ObjectId::from_bytes([10; 32]);
    variants.push(x);
    let mut x = c.clone();
    x.target_ref = "release".into();
    variants.push(x);
    let mut x = c.clone();
    x.expected_head = native(10);
    variants.push(x);
    let mut x = c.clone();
    x.expected_generation += 1;
    variants.push(x);
    let mut x = c.clone();
    x.policy_digest = [10; 32];
    variants.push(x);
    for changed in variants {
        assert_ne!(context_digest(&changed, &k), r.context);
        assert_eq!(
            check(&changed, &current(&changed), &k, std::slice::from_ref(&r)),
            CheckDecision::Blocked(vec![Blocker::Missing {
                check: "build".into()
            }])
        );
    }
}
#[test]
fn reruns_workflow_environment_toolchain_dependencies_and_issuer_policy_do_not_reuse_green() {
    let (c, t, k, r) = fixture();
    let mut variants = Vec::new();
    let mut x = k.clone();
    x.name = "test".into();
    variants.push(x);
    let mut x = k.clone();
    x.run_id = "run-2".into();
    variants.push(x);
    let mut x = k.clone();
    x.definition_digest = [10; 32];
    variants.push(x);
    let mut x = k.clone();
    x.environment_digest = [10; 32];
    variants.push(x);
    let mut x = k.clone();
    x.toolchain_digest = [10; 32];
    variants.push(x);
    let mut x = k.clone();
    x.dependency_digest = [10; 32];
    variants.push(x);
    let mut x = k.clone();
    x.trusted_validators.insert("new-ci".into());
    variants.push(x);
    for changed in variants {
        assert!(matches!(
            check(&c, &t, &changed, std::slice::from_ref(&r)),
            CheckDecision::Blocked(_)
        ));
    }
}
#[test]
fn ref_movement_epoch_policy_scope_and_freeze_block_an_otherwise_valid_result() {
    let (c, t, k, r) = fixture();
    let mut variants = Vec::new();
    let mut x = t.clone();
    x.head = native(10);
    variants.push(x);
    let mut x = t.clone();
    x.generation += 1;
    variants.push(x);
    let mut x = t.clone();
    x.authority_epoch += 1;
    variants.push(x);
    let mut x = t.clone();
    x.policy_digest = [10; 32];
    variants.push(x);
    let mut x = t.clone();
    x.target_ref = "release".into();
    variants.push(x);
    let mut x = t.clone();
    x.repository_scope = "other/repo".into();
    variants.push(x);
    let mut x = t.clone();
    x.active = false;
    variants.push(x);
    for changed in variants {
        assert_eq!(
            check(&c, &changed, &k, std::slice::from_ref(&r)),
            CheckDecision::Blocked(vec![Blocker::StaleTarget])
        );
    }
}
#[test]
fn git_and_native_revisions_are_not_interchangeable_for_an_identical_digest() {
    let (mut c, t, k, r) = fixture();
    c.revision = RevisionRef::Git(GitOid::Sha256([1; 32]));
    assert_ne!(context_digest(&c, &k), r.context);
    assert!(matches!(check(&c, &t, &k, &[r]), CheckDecision::Blocked(_)));
}
#[test]
fn agent_claims_missing_results_failure_cancellation_and_revocation_do_not_pass() {
    let (c, t, k, r) = fixture();
    assert_eq!(
        check(&c, &t, &k, &[]),
        CheckDecision::Blocked(vec![Blocker::Missing {
            check: "build".into()
        }])
    );
    let mut agent = r.clone();
    agent.validator = "agent".into();
    assert_eq!(
        check(&c, &t, &k, &[agent]),
        CheckDecision::Blocked(vec![Blocker::Untrusted {
            check: "build".into()
        }])
    );
    for outcome in [Outcome::Failed, Outcome::Error, Outcome::Cancelled] {
        let mut result = r.clone();
        result.outcome = outcome;
        assert_eq!(
            check(&c, &t, &k, &[result]),
            CheckDecision::Blocked(vec![Blocker::Unsuccessful {
                check: "build".into(),
                outcome
            }])
        );
    }
    assert_eq!(
        evaluate_checks(
            &c,
            &t,
            &[k],
            std::slice::from_ref(&r),
            &BTreeSet::from([r.id.clone()])
        ),
        CheckDecision::Blocked(vec![Blocker::Revoked {
            check: "build".into()
        }])
    );
}
#[test]
fn competing_receipts_or_colliding_ids_fail_closed() {
    let (c, t, k, r) = fixture();
    let mut changed = r.clone();
    changed.outcome = Outcome::Failed;
    assert_eq!(
        check(&c, &t, &k, &[r.clone(), changed]),
        CheckDecision::Blocked(vec![Blocker::Conflicting {
            check: "build".into()
        }])
    );
    let mut other = r.clone();
    other.id = "receipt-2".into();
    assert_eq!(
        check(&c, &t, &k, &[r, other]),
        CheckDecision::Blocked(vec![Blocker::Conflicting {
            check: "build".into()
        }])
    );
}
#[test]
fn empty_or_ambiguous_policy_is_never_implicitly_successful() {
    let (c, t, k, r) = fixture();
    assert_eq!(
        evaluate_checks(&c, &t, &[], std::slice::from_ref(&r), &BTreeSet::new()),
        CheckDecision::Blocked(vec![Blocker::InvalidPolicy])
    );
    assert_eq!(
        evaluate_checks(&c, &t, &[k.clone(), k], &[r], &BTreeSet::new()),
        CheckDecision::Blocked(vec![Blocker::InvalidPolicy])
    );
}
