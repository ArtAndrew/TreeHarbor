use serde_json::{Value, json};
use treeharbor_github_contract::{groups::GroupCapture, queue::*};
fn fixture() -> (QueueTarget, QueueObservation, GroupCapture) {
    let v: Value = serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap();
    let g: QueueObservation =
        serde_json::from_str(include_str!("fixtures/queue-groups-v4.json")).unwrap();
    (
        QueueTarget {
            pull: serde_json::from_value(v["target"].clone()).unwrap(),
            expected_base_revision: format!("git:sha1:{}", "22".repeat(20)),
            expected_rules: serde_json::from_value(v["report"]["rules_before"].clone()).unwrap(),
        },
        serde_json::from_str(include_str!("fixtures/publication-v8.json")).unwrap(),
        g.group_capture.unwrap(),
    )
}
fn commit(byte: &str) -> GitCommitObservation {
    GitCommitObservation {
        revision: format!("git:sha1:{}", byte.repeat(20)),
        tree: format!("git:sha1:{}", byte.repeat(20)),
    }
}
#[test]
fn published_exact_candidate_and_later_descendants_do_not_transfer_ci() {
    let (t, r, g) = fixture();
    r.validate(&t).unwrap();
    assert_eq!(
        json!(r),
        serde_json::from_str::<Value>(include_str!("fixtures/publication-v8.json")).unwrap()
    );
    let p = r.publication.as_ref().unwrap();
    p.target.validate_source(&t, &g).unwrap();
    assert!(p.exact_candidate_published(&t));
    assert_eq!(r.state(&t), "uncertain");
    let mut p = p.clone();
    p.before.main = commit("ab");
    p.after = p.before.clone();
    p.comparison.head_revision = p.before.main.revision.clone();
    p.comparison.status = "ahead".into();
    p.comparison.ahead_by = 9;
    p.comparison.total_commits = 9;
    p.validate(&t).unwrap();
    assert!(p.exact_candidate_published(&t));
    // A changed PR source, final identity, target read or ancestry never inherits proof.
    for case in [
        "head",
        "unmerged",
        "drift",
        "same-tree",
        "behind",
        "diverged",
    ] {
        let mut q = p.clone();
        match case {
            "head" => {
                q.before.pull.head_revision = commit("cd").revision;
                q.after = q.before.clone();
            }
            "unmerged" => {
                q.before.pull.merged = false;
                q.before.pull.state = "open".into();
                q.before.final_commit = None;
                q.after = q.before.clone();
            }
            "drift" => q.after.main = commit("cd"),
            "same-tree" => {
                q.before.final_commit.as_mut().unwrap().commit.revision = commit("cd").revision;
                q.after = q.before.clone();
            }
            "behind" => {
                q.comparison.status = "behind".into();
                q.comparison.ahead_by = 0;
                q.comparison.total_commits = 0;
                q.comparison.behind_by = 1;
                q.comparison.merge_base = q.before.main.clone();
            }
            "diverged" => {
                q.comparison.status = "diverged".into();
                q.comparison.behind_by = 1;
                q.comparison.merge_base = commit("cd");
            }
            _ => unreachable!(),
        }
        q.validate(&t).unwrap();
        assert!(!q.exact_candidate_published(&t), "{case}");
    }
}
#[test]
fn malformed_or_contradictory_publication_never_becomes_evidence() {
    let (t, r, g) = fixture();
    for case in [
        "version",
        "mixed",
        "no-payload",
        "sequence",
        "member-id",
        "source",
        "parent",
        "tree",
        "same-oid-parents",
        "main-tree",
        "comparison-head",
        "comparison-base",
        "status",
        "ahead",
        "behind",
        "total",
        "merge-base",
        "unmerged-final",
        "missing-final",
        "unknown",
    ] {
        let mut v = json!(r);
        match case {
            "version" => v["version"] = json!(7),
            "mixed" => v["snapshot"] = json!(g.before.target),
            "no-payload" => {
                v.as_object_mut().unwrap().remove("publication");
            }
            "unknown" => v["publication"]["surprise"] = json!(true),
            other => {
                let p = &mut v["publication"];
                match other {
                    "sequence" => p["target"]["membership_sequence"] = json!(0),
                    "member-id" => p["target"]["member"]["pull_node"] = json!("other"),
                    "source" => {
                        p["target"]["member"]["source_revision"] = json!(commit("cd").revision)
                    }
                    "parent" => {
                        p["target"]["member"]["candidate"]["parents"][1]["revision"] =
                            json!(commit("cd").revision)
                    }
                    "tree" => {
                        p["before"]["final_commit"]["commit"]["tree"] = json!(commit("cd").tree)
                    }
                    "same-oid-parents" => p["before"]["final_commit"]["parents"] = json!([]),
                    "main-tree" => p["after"]["main"]["tree"] = json!(commit("cd").tree),
                    "comparison-head" => {
                        p["comparison"]["head_revision"] = json!(commit("cd").revision)
                    }
                    "comparison-base" => p["comparison"]["base"] = json!(commit("cd")),
                    "status" => p["comparison"]["status"] = json!("unknown"),
                    "ahead" => p["comparison"]["ahead_by"] = json!(1),
                    "behind" => p["comparison"]["behind_by"] = json!(1),
                    "total" => p["comparison"]["total_commits"] = json!(1),
                    "merge-base" => p["comparison"]["merge_base"] = json!(commit("cd")),
                    "unmerged-final" => {
                        p["before"]["pull"]["merged"] = json!(false);
                        p["before"]["pull"]["state"] = json!("open");
                    }
                    "missing-final" => p["before"]["final_commit"] = Value::Null,
                    _ => unreachable!(),
                }
            }
        }
        assert!(
            serde_json::from_value::<QueueObservation>(v).map_or(true, |r| r.validate(&t).is_err()),
            "{case}"
        );
    }
    let p = r.publication.unwrap();
    let mut changed = g.clone();
    changed.after.members[0].position += 1;
    changed.validate(&t).unwrap();
    assert!(p.target.validate_source(&t, &changed).is_err());
    let mut p = p;
    p.target.member.entry.id = "other-entry".into();
    p.target.validate(&t).unwrap();
    assert!(p.target.validate_source(&t, &g).is_err());
}
