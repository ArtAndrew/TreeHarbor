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

fn admission_fixture() -> (Target, Report, Value) {
    // Expected bytes and hashes were produced by an independent Python stdlib
    // reference, rather than by the Rust implementation under test.
    let v: Value =
        serde_json::from_str(include_str!("fixtures/check-publication-v2.json")).unwrap();
    (
        serde_json::from_value(v["target"].clone()).unwrap(),
        serde_json::from_value(v["report"].clone()).unwrap(),
        v,
    )
}

#[test]
fn version_one_keeps_its_exact_encoding_and_legacy_name() {
    let (t, _, v) = fixture();
    let (_, _, admission) = admission_fixture();
    assert_eq!(
        serde_json::to_string(&t).unwrap(),
        admission["legacy_target_json"]
    );
    assert_eq!(t.name().unwrap(), v["name"]);
    assert!(!json!(t).as_object().unwrap().contains_key("admission"));

    // A null optional value carries no policy authority and normalizes to the
    // same omitted canonical field as a historical v1 target.
    let mut null = v["target"].clone();
    null["admission"] = Value::Null;
    let normalized: Target = serde_json::from_value(null).unwrap();
    normalized.validate().unwrap();
    assert_eq!(normalized, t);
    assert_eq!(json!(normalized), v["target"]);

    let mut other_base = t.clone();
    let Source::Pull { target } = &mut other_base.source else {
        unreachable!()
    };
    target.base_ref = "release/2026.10".into();
    assert_eq!(other_base.name().unwrap(), v["name"]);
}

#[test]
fn version_two_matches_independent_admission_name_and_wire_vector() {
    let (t, r, v) = admission_fixture();
    t.validate().unwrap();
    r.validate(&t).unwrap();
    assert!(r.preflight.as_ref().unwrap().eligible(&t));
    assert_eq!(t.name().unwrap(), v["name"]);
    assert_eq!(t.external_id().unwrap(), v["external_id"]);
    assert_eq!(serde_json::to_string(&t).unwrap(), v["target_json"]);
    assert_eq!(json!(t), v["target"]);
    assert_eq!(json!(r), v["report"]);
    let a = t.admission.as_ref().unwrap();
    assert_eq!(
        json!([
            t.epoch,
            a.binding.to_string(),
            t.source.pull().base_ref,
            a.generation,
            t.program
        ])
        .to_string(),
        v["context_json"]
    );
    assert_eq!(
        t.name().unwrap(),
        format!(
            "TreeHarbor/e{}/a{}",
            t.epoch,
            v["context_sha256"].as_str().unwrap()
        )
    );
}

#[test]
fn admission_context_rotates_scoped_identity_and_retains_policy_without_hashing_it() {
    let (original, report, v) = admission_fixture();
    let original_name = original.name().unwrap();
    let mut names = std::collections::BTreeSet::new();
    for vector in v["context_vectors"].as_array().unwrap() {
        let label = vector["label"].as_str().unwrap();
        let mut t = original.clone();
        t.epoch = vector["epoch"].as_i64().unwrap();
        t.program = vector["program"].as_str().unwrap().into();
        t.admission = Some(serde_json::from_value(vector["admission"].clone()).unwrap());
        let Source::Pull { target } = &mut t.source else {
            unreachable!()
        };
        target.base_ref = vector["base_ref"].as_str().unwrap().into();
        t.validate().unwrap();
        let name = t.name().unwrap();
        assert_eq!(name, vector["name"], "{label}");
        assert_eq!(
            context_name(
                t.epoch,
                &t.program,
                &t.source.pull().base_ref,
                t.admission.as_ref()
            )
            .unwrap(),
            name,
            "{label}"
        );
        assert!(name.len() < 100, "{label}");
        assert_eq!(name.rsplit_once('/').unwrap().1.len(), 65, "{label}");
        if matches!(label, "original" | "policy") {
            assert_eq!(name, original_name, "{label}");
            report.validate(&t).unwrap();
        } else {
            assert_ne!(name, original_name, "{label}");
            assert!(
                names.insert(name),
                "distinct rotations must not alias: {label}"
            );
            assert!(report.validate(&t).is_err(), "{label}");
            assert!(!report.preflight.as_ref().unwrap().eligible(&t), "{label}");
        }
        assert_eq!(json!(t)["admission"], vector["admission"], "{label}");
    }
}

#[test]
fn target_versions_cannot_omit_or_inherit_admission_authority() {
    let (legacy, _, _) = fixture();
    let (admitted, _, _) = admission_fixture();
    let mut t = legacy;
    t.admission = admitted.admission.clone();
    assert!(t.validate().is_err());
    assert!(t.name().is_err());
    let mut t = admitted.clone();
    t.admission = None;
    assert!(t.validate().is_err());
    assert!(t.name().is_err());
    for version in [0, 3, u8::MAX] {
        t.version = version;
        assert!(t.validate().is_err(), "version {version} without admission");
        t.admission = admitted.admission.clone();
        assert!(t.validate().is_err(), "version {version} with admission");
        t.admission = None;
    }
    let (_, _, v) = admission_fixture();
    for null in [false, true] {
        let mut raw = v["target"].clone();
        if null {
            raw["admission"] = Value::Null;
        } else {
            raw.as_object_mut().unwrap().remove("admission");
        }
        assert!(
            serde_json::from_value::<Target>(raw)
                .unwrap()
                .validate()
                .is_err()
        );
    }
}

#[test]
fn admission_fields_are_strict_and_invalid_identities_fail_closed() {
    let (_, _, v) = admission_fixture();
    for field in ["binding", "generation", "policy"] {
        let mut missing = v["target"].clone();
        missing["admission"].as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<Target>(missing).is_err(),
            "missing {field}"
        );
    }
    for (field, value) in [
        ("binding", Value::Null),
        ("binding", json!(17)),
        ("binding", json!("not-a-uuid")),
        ("generation", Value::Null),
        ("generation", json!("42")),
        ("generation", json!(1.5)),
        ("generation", json!(i64::MAX as u64 + 1)),
        ("policy", Value::Null),
        ("policy", json!(42)),
    ] {
        let mut raw = v["target"].clone();
        raw["admission"][field] = value.clone();
        assert!(
            serde_json::from_value::<Target>(raw).is_err(),
            "{field}: {value}"
        );
    }
    for scope in ["target", "admission"] {
        let mut raw = v["target"].clone();
        if scope == "target" {
            raw["unexpected"] = json!(true);
        } else {
            raw["admission"]["unexpected"] = json!(true);
        }
        assert!(serde_json::from_value::<Target>(raw).is_err(), "{scope}");
    }
    for (field, value) in [
        ("binding", json!("00000000-0000-0000-0000-000000000000")),
        ("generation", json!(0)),
        ("generation", json!(-1)),
        ("generation", json!(i64::MIN)),
        ("policy", json!("ef".repeat(32))),
        ("policy", json!(format!("sha256:{}", "EF".repeat(32)))),
        ("policy", json!(format!("sha256:{}", "ef".repeat(31)))),
        ("policy", json!(format!("sha256:{}", "gg".repeat(32)))),
    ] {
        let mut raw = v["target"].clone();
        raw["admission"][field] = value.clone();
        let t: Target = serde_json::from_value(raw).unwrap();
        assert!(t.validate().is_err(), "{field}: {value}");
        assert!(t.name().is_err(), "{field}: {value}");
    }
    let mut duplicate = v["target_json"].as_str().unwrap().to_owned();
    duplicate = duplicate.replacen(
        "\"generation\":42",
        "\"generation\":42,\"generation\":43",
        1,
    );
    assert!(serde_json::from_str::<Target>(&duplicate).is_err());
}

#[test]
fn context_name_checks_bounds_before_constructing_a_required_name() {
    let (t, _, _) = admission_fixture();
    let admission = t.admission.as_ref().unwrap();
    for epoch in [0, -1, i64::MIN] {
        assert!(context_name(epoch, &t.program, "main", Some(admission)).is_err());
        assert!(context_name(epoch, &t.program, "main", None).is_err());
    }
    for generation in [0, -1, i64::MIN] {
        let mut changed = admission.clone();
        changed.generation = generation;
        assert!(context_name(t.epoch, &t.program, "main", Some(&changed)).is_err());
    }
    for base in ["".to_owned(), " ".into(), "x".repeat(129), "main\n".into()] {
        assert!(context_name(t.epoch, &t.program, &base, Some(admission)).is_err());
    }
    for program in [
        "aa".repeat(32),
        format!("sha256:{}", "AA".repeat(32)),
        format!("sha256:{}", "aa".repeat(31)),
    ] {
        assert!(context_name(t.epoch, &program, "main", Some(admission)).is_err());
        assert!(context_name(t.epoch, &program, "main", None).is_err());
    }
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
