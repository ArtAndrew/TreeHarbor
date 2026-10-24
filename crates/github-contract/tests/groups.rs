use serde_json::{Value, json};
use treeharbor_github_contract::{groups::*, queue::*};
fn fixture() -> (QueueTarget, Value) {
    let f: Value = serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap();
    (
        QueueTarget {
            pull: serde_json::from_value(f["target"].clone()).unwrap(),
            expected_base_revision: format!("git:sha1:{}", "22".repeat(20)),
            expected_rules: serde_json::from_value(f["report"]["rules_before"].clone()).unwrap(),
        },
        serde_json::from_str(include_str!("fixtures/queue-groups-v4.json")).unwrap(),
    )
}
#[test]
fn membership_vector_preserves_actual_main_and_exact_parent_relationship() {
    let (t, v) = fixture();
    let r: QueueObservation = serde_json::from_value(v.clone()).unwrap();
    r.validate(&t).unwrap();
    assert_eq!(serde_json::to_value(&r).unwrap(), v);
    let g = r.group_capture.unwrap();
    assert!(g.consistent(&t));
    assert_ne!(g.after.main.revision, g.after.target.pull.base_revision);
    assert!(g.after.selected(&t).unwrap().has_exact_merge_parents());
}
#[test]
fn membership_rejects_mixed_versions_aliases_incomplete_identity_and_bad_parents() {
    let (t, original) = fixture();
    for case in [
        "old",
        "snapshot",
        "duplicate",
        "position",
        "foreign",
        "pull-alias",
        "head",
        "missing",
        "self-parent",
        "parent-limit",
        "mixed",
    ] {
        let mut v = original.clone();
        match case {
            "old" => v["version"] = json!(3),
            "snapshot" => v["snapshot"]["entry"] = Value::Null,
            "duplicate" => {
                let m = v["group_capture"]["before"]["members"][0].clone();
                v["group_capture"]["before"]["members"]
                    .as_array_mut()
                    .unwrap()
                    .push(m);
            }
            "position" => v["group_capture"]["before"]["members"][0]["position"] = json!(0),
            "foreign" => {
                v["group_capture"]["before"]["members"][0]["source_repository_id"] = json!(12)
            }
            "pull-alias" => {
                v["group_capture"]["before"]["members"][0]["pull_node"] = json!("PR_other")
            }
            "head" => {
                v["group_capture"]["before"]["members"][0]["source_revision"] =
                    json!(format!("git:sha1:{}", "aa".repeat(20)))
            }
            "missing" => v["group_capture"]["before"]["members"] = json!([]),
            "self-parent" => {
                v["group_capture"]["before"]["members"][0]["candidate"]["parents"][0] =
                    v["group_capture"]["before"]["members"][0]["candidate"]["commit"].clone();
            }
            "parent-limit" => {
                let p =
                    v["group_capture"]["before"]["members"][0]["candidate"]["parents"][0].clone();
                v["group_capture"]["before"]["members"][0]["candidate"]["parents"] =
                    json!(vec![p; MAX_PARENTS + 1]);
            }
            "mixed" => v["mutation_not_sent"] = json!(true),
            _ => unreachable!(),
        }
        assert!(
            serde_json::from_value::<QueueObservation>(v)
                .unwrap()
                .validate(&t)
                .is_err(),
            "{case}"
        );
    }
}
#[test]
fn drift_and_unknown_parent_layout_are_retained_without_inheriting_consistency() {
    let (t, v) = fixture();
    let mut r: QueueObservation = serde_json::from_value(v).unwrap();
    let g = r.group_capture.as_mut().unwrap();
    g.after.main.revision = format!("git:sha1:{}", "ab".repeat(20));
    g.before.members[0]
        .candidate
        .as_mut()
        .unwrap()
        .parents
        .reverse();
    g.after.members[0]
        .candidate
        .as_mut()
        .unwrap()
        .parents
        .reverse();
    assert!(!g.consistent(&t));
    assert!(!g.after.members[0].has_exact_merge_parents());
    r.validate(&t).unwrap();
}

#[test]
fn immutable_commit_identity_cannot_change_tree_or_parent_order_between_reads() {
    let (t, v) = fixture();
    let original: QueueObservation = serde_json::from_value(v).unwrap();
    for case in ["main-tree", "parent-tree", "parents", "final-tree"] {
        let mut r = original.clone();
        let g = &mut r.group_capture.as_mut().unwrap().after;
        match case {
            "main-tree" => g.main.tree = format!("git:sha1:{}", "ab".repeat(20)),
            "parent-tree" => {
                g.members[0].candidate.as_mut().unwrap().parents[1].tree =
                    format!("git:sha1:{}", "ab".repeat(20));
            }
            "parents" => g.members[0].candidate.as_mut().unwrap().parents.reverse(),
            "final-tree" => {
                let mut c = g.members[0].candidate.as_ref().unwrap().commit.clone();
                c.tree = format!("git:sha1:{}", "ab".repeat(20));
                g.target.pull.merged = true;
                g.target.pull.state = "closed".into();
                g.target.final_commit = Some(c);
                g.target.entry = None;
                g.members.clear();
            }
            _ => unreachable!(),
        }
        g.validate(&t).unwrap();
        r.snapshot = Some(g.target.clone());
        assert!(r.validate(&t).is_err(), "{case}");
    }
}

#[test]
fn multiple_members_retain_chain_and_fork_source_without_claiming_target_membership() {
    let (t, v) = fixture();
    let mut r: QueueObservation = serde_json::from_value(v).unwrap();
    let g = r.group_capture.as_mut().unwrap();
    let mut next = g.before.members[0].clone();
    next.entry.id = "MQE_next".into();
    next.position = 2;
    next.pull_node = "PR_next".into();
    next.pull_number = 18;
    next.source_repository_id = 12;
    next.source_revision = format!("git:sha1:{}", "ab".repeat(20));
    next.entry.base = next.entry.head.clone();
    let c = next.candidate.as_mut().unwrap();
    c.parents[0] = next.entry.base.clone().unwrap();
    c.parents[1].revision = next.source_revision.clone();
    c.commit.revision = format!("git:sha1:{}", "cd".repeat(20));
    next.entry.head = Some(c.commit.clone());
    assert!(next.has_exact_merge_parents());
    g.before.members.push(next);
    g.after = g.before.clone();
    assert!(g.consistent(&t));
    assert_eq!(g.after.selected(&t).unwrap().pull_number, 17);
    g.after.members[1].position = 1;
    assert!(r.validate(&t).is_err());
}

#[test]
fn capture_size_is_bounded_even_with_individually_valid_members() {
    let (t, v) = fixture();
    let mut r: QueueObservation = serde_json::from_value(v).unwrap();
    let g = r.group_capture.as_mut().unwrap();
    let template = g.before.members[0].clone();
    for i in 1..MAX_MEMBERS {
        let mut m = template.clone();
        m.entry.id = format!("MQE_{i}");
        m.pull_node = format!("PR_{i}");
        m.pull_number = 1000 + i as u64;
        m.position = 1 + i as u32;
        g.before.members.push(m);
    }
    g.after = g.before.clone();
    g.before.validate(&t).unwrap();
    assert!(serde_json::to_vec(&g).unwrap().len() > 48 * 1024);
    assert!(g.validate(&t).is_err());
}
