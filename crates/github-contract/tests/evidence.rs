use serde_json::{Value, json};
use treeharbor_github_contract::{InspectionReport, InspectionTarget};
fn fixture() -> (InspectionTarget, InspectionReport) {
    let v: Value = serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap();
    (
        serde_json::from_value(v["target"].clone()).unwrap(),
        serde_json::from_value(v["report"].clone()).unwrap(),
    )
}
#[test]
fn exact_issuer_and_revision_cannot_be_replaced_by_a_passing_label() {
    let (t, r) = fixture();
    assert!(r.selected_checks_passed(&t));
    for field in ["app", "sha", "name", "id", "count", "extra"] {
        let mut wrong = r.clone();
        let run = &mut wrong.checks[0].runs[0];
        match field {
            "app" => run.app_id += 1,
            "sha" => run.head_revision = format!("git:sha1:{}", "33".repeat(20)),
            "name" => run.name = "same looking name".into(),
            "id" => run.id = 0,
            "count" => wrong.checks[0].total_count = 0,
            _ => wrong.checks.push(wrong.checks[0].clone()),
        };
        assert!(wrong.validate(&t).is_err(), "{field}");
        assert!(!wrong.selected_checks_passed(&t));
    }
}
#[test]
fn missing_ambiguous_rerunning_and_non_success_checks_are_not_green() {
    let (t, r) = fixture();
    for status in [
        "queued",
        "in_progress",
        "pending",
        "waiting",
        "new_unknown_status",
    ] {
        let mut r = r.clone();
        r.checks[0].runs[0].status = status.into();
        assert!(!r.selected_checks_passed(&t));
    }
    for conclusion in [
        None,
        Some("failure"),
        Some("skipped"),
        Some("neutral"),
        Some("cancelled"),
        Some("timed_out"),
        Some("stale"),
        Some("unknown"),
    ] {
        let mut r = r.clone();
        r.checks[0].runs[0].conclusion = conclusion.map(str::to_owned);
        assert!(!r.selected_checks_passed(&t));
    }
    let mut missing = r.clone();
    missing.checks[0].runs.clear();
    missing.checks[0].total_count = 0;
    missing.validate(&t).unwrap();
    assert!(!missing.selected_checks_passed(&t));
    let mut ambiguous = r;
    let mut second = ambiguous.checks[0].runs[0].clone();
    second.id += 1;
    ambiguous.checks[0].runs.push(second);
    ambiguous.checks[0].total_count = 2;
    ambiguous.validate(&t).unwrap();
    assert!(!ambiguous.selected_checks_passed(&t));
}
#[test]
fn head_base_and_policy_changes_invalidate_the_capture() {
    let (t, r) = fixture();
    for field in ["head", "base", "policy", "draft", "closed"] {
        let mut changed = r.clone();
        match field {
            "head" => changed.after.head_revision = format!("git:sha1:{}", "33".repeat(20)),
            "base" => changed.after.base_revision = format!("git:sha1:{}", "33".repeat(20)),
            "policy" => changed.rules_after.sha256 = "33".repeat(32),
            "draft" => changed.after.draft = true,
            _ => changed.after.state = "closed".into(),
        };
        changed.validate(&t).unwrap();
        assert!(!changed.consistent(&t));
        assert!(!changed.selected_checks_passed(&t));
    }
    let mut r = r;
    r.before.head_revision = format!("git:sha1:{}", "44".repeat(20));
    r.after = r.before.clone();
    assert!(!r.consistent(&t));
}
#[test]
fn target_and_report_bounds_and_unknown_fields_are_rejected() {
    let (t, r) = fixture();
    let mut wrong = t.clone();
    wrong.required.clear();
    assert!(wrong.validate().is_err());
    let mut wrong = t.clone();
    wrong.required.push(wrong.required[0].clone());
    assert!(wrong.validate().is_err());
    for base in ["../main", "main?x=1", "main.lock", "main//x"] {
        let mut wrong = t.clone();
        wrong.base_ref = base.into();
        assert!(wrong.validate().is_err());
    }
    let mut wrong = r.clone();
    wrong.finished_at_unix_ms = wrong.started_at_unix_ms + 120001;
    assert!(wrong.validate(&t).is_err());
    let mut v = json!(r);
    v["passed"] = json!(true);
    assert!(serde_json::from_value::<InspectionReport>(v).is_err());
}
