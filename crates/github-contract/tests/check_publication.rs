use serde_json::{Value, json};
use treeharbor_github_contract::{check_publication::*, publication, queue::*};

fn fixture() -> (Target, Report, Value) {
    let v: Value =
        serde_json::from_str(include_str!("fixtures/check-publication-v1.json")).unwrap();
    (
        serde_json::from_value(v["target"].clone()).unwrap(),
        serde_json::from_value(v["report"].clone()).unwrap(),
        v,
    )
}

#[test]
fn exact_publication_vector_binds_program_epoch_and_execution() {
    let (t, r, v) = fixture();
    t.validate().unwrap();
    r.validate(&t).unwrap();
    assert!(r.preflight.as_ref().unwrap().eligible(&t));
    assert_eq!(t.name().unwrap(), v["name"]);
    assert_eq!(t.external_id().unwrap(), v["external_id"]);
    assert_eq!(json!(t), v["target"]);
    assert_eq!(json!(r), v["report"]);
    let mut changed = t.clone();
    changed.epoch += 1;
    assert_ne!(changed.name().unwrap(), t.name().unwrap());
    assert!(!r.preflight.as_ref().unwrap().eligible(&changed));
    assert!(r.validate(&changed).is_err());
    changed = t.clone();
    changed.program = format!("sha256:{}", "cc".repeat(32));
    assert_ne!(changed.name().unwrap(), t.name().unwrap());
    assert!(r.validate(&changed).is_err());
    changed = t.clone();
    changed.evidence = format!("sha256:{}", "cc".repeat(32));
    assert_eq!(changed.name().unwrap(), t.name().unwrap());
    assert_ne!(changed.external_id().unwrap(), t.external_id().unwrap());
    assert!(r.validate(&changed).is_err());
}

#[test]
fn preflight_blocks_drift_weak_policy_and_wrong_issuer() {
    let (t, r, _) = fixture();
    let original = r.preflight.unwrap();
    for case in [
        "head",
        "base",
        "closed",
        "merged",
        "policy",
        "issuer",
        "name",
        "admins",
        "force",
        "delete",
        "bypass",
        "queue",
        "duplicate",
    ] {
        let mut p = original.clone();
        let SourceView::Pull(after) = &mut p.after else {
            unreachable!()
        };
        match case {
            "head" => after.head_revision = format!("git:sha1:{}", "cc".repeat(20)),
            "base" => after.base_revision = format!("git:sha1:{}", "cc".repeat(20)),
            "closed" => after.state = "closed".into(),
            "merged" => {
                after.state = "closed".into();
                after.merged = true;
            }
            "policy" => p.policy_after.protection.sha256 = format!("sha256:{}", "cc".repeat(32)),
            "issuer" => p.policy_after.protection.app_bound_checks[0].app_id = 8,
            "name" => p.policy_after.protection.app_bound_checks[0].name = "TreeHarbor".into(),
            "admins" => p.policy_after.protection.enforce_admins = false,
            "force" => p.policy_after.protection.allow_force_pushes = true,
            "delete" => p.policy_after.protection.allow_deletions = true,
            "bypass" => p.policy_after.protection.has_bypass = true,
            "queue" => p.policy_after.rules.merge_queue = false,
            "duplicate" => p
                .policy_after
                .protection
                .app_bound_checks
                .push(p.policy_after.protection.app_bound_checks[0].clone()),
            _ => unreachable!(),
        }
        assert!(!p.eligible(&t), "{case}");
        // Stable-but-wrong policy/head also cannot pass.
        if case != "policy" && case != "base" {
            p.before = p.after.clone();
            p.policy_before = p.policy_after.clone();
            assert!(!p.eligible(&t), "stable {case}");
        }
    }
    let mut draft = original;
    let SourceView::Pull(p) = &mut draft.after else {
        unreachable!()
    };
    p.draft = true;
    draft.before = draft.after.clone();
    assert!(draft.eligible(&t)); // PR preflight is allowed before ready-for-review.
}

#[test]
fn report_cannot_promote_failure_or_mix_identity_and_unknown_states() {
    let (t, original, _) = fixture();
    for case in [
        "app",
        "head",
        "external",
        "name",
        "id",
        "status",
        "conclusion",
        "unknown",
        "not_sent",
        "missing",
        "time",
        "overflow",
    ] {
        let mut r = original.clone();
        let c = r.check.as_mut().unwrap();
        match case {
            "app" => c.app_id = 8,
            "head" => c.head_revision = format!("git:sha1:{}", "cc".repeat(20)),
            "external" => c.external_id.push('x'),
            "name" => c.name.push('x'),
            "id" => c.id = 0,
            "status" => c.status = "in_progress".into(),
            "conclusion" => c.conclusion = Conclusion::Failure,
            "unknown" => r.outcome = Outcome::Unknown,
            "not_sent" => r.outcome = Outcome::NotSent,
            "missing" => r.check = None,
            "time" => r.finished_at_unix_ms = 0,
            "overflow" => {
                r.started_at_unix_ms = 1;
                r.finished_at_unix_ms = i64::MAX;
            }
            _ => unreachable!(),
        }
        assert!(r.validate(&t).is_err(), "{case}");
    }
    for conclusion in [
        Conclusion::Failure,
        Conclusion::Cancelled,
        Conclusion::ActionRequired,
    ] {
        let mut negative = t.clone();
        negative.conclusion = conclusion;
        assert!(original.validate(&negative).is_err());
        let mut r = original.clone();
        r.check.as_mut().unwrap().conclusion = conclusion;
        r.validate(&negative).unwrap();
    }
    for conclusion in ["neutral", "skipped", "timed_out", "unknown"] {
        assert!(serde_json::from_value::<Conclusion>(json!(conclusion)).is_err());
    }
}

#[test]
fn group_dispatch_checks_current_membership_and_exact_candidate() {
    let (mut t, r, _) = fixture();
    let mut p = r.preflight.unwrap();
    let g: QueueObservation =
        serde_json::from_str(include_str!("fixtures/queue-groups-v4.json")).unwrap();
    let capture = g.group_capture.unwrap();
    let queue = QueueTarget {
        pull: t.source.pull().clone(),
        expected_base_revision: format!("git:sha1:{}", "22".repeat(20)),
        expected_rules: p.policy_before.rules.clone(),
    };
    let selected = publication::Target {
        membership_sequence: 1,
        member: capture.before.members[0].clone(),
    };
    t.source = Source::Group {
        queue: Box::new(queue),
        target: Box::new(selected),
    };
    p.before = SourceView::Group(Box::new(capture.before.clone()));
    p.after = SourceView::Group(Box::new(capture.after));
    assert!(p.eligible(&t));
    assert_ne!(t.source.head().unwrap(), t.source.pull().head_revision);
    for case in ["main", "member", "removed", "draft", "rules"] {
        let mut changed = p.clone();
        let SourceView::Group(g) = &mut changed.after else {
            unreachable!()
        };
        match case {
            "main" => g.main.revision = format!("git:sha1:{}", "cc".repeat(20)),
            "member" => g.members[0].entry.state = "MERGEABLE".into(),
            "removed" => {
                g.members.clear();
                g.target.entry = None;
            }
            "draft" => g.target.pull.draft = true,
            "rules" => {
                changed.policy_after.rules.sha256 = "cc".repeat(32);
                changed.policy_before = changed.policy_after.clone();
            }
            _ => unreachable!(),
        }
        assert!(!changed.eligible(&t), "{case}");
        changed.before = changed.after.clone();
        // A stable changed main alone need not invalidate immutable candidate
        // identity; all other mismatches remain ineligible.
        if case != "main" {
            assert!(!changed.eligible(&t), "stable {case}");
        }
    }
}
