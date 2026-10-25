use serde_json::{Value, json};
use treeharbor_github_contract::{queue::*, workflow_definitions::*};
fn fixture() -> (QueueTarget, QueueObservation, Policy) {
    let v: Value = serde_json::from_str(include_str!("fixtures/inspection-v1.json")).unwrap();
    (
        QueueTarget {
            pull: serde_json::from_value(v["target"].clone()).unwrap(),
            expected_base_revision: format!("git:sha1:{}", "22".repeat(20)),
            expected_rules: serde_json::from_value(v["report"]["rules_before"].clone()).unwrap(),
        },
        serde_json::from_str(include_str!("fixtures/workflow-definition-v7.json")).unwrap(),
        serde_json::from_str(include_str!("fixtures/workflow-policy-v1.json")).unwrap(),
    )
}
#[test]
fn independent_hash_vectors_bind_root_bytes_without_approving_runtime() {
    let (t, r, p) = fixture();
    r.validate(&t).unwrap();
    assert_eq!(
        json!(r),
        serde_json::from_str::<Value>(include_str!("fixtures/workflow-definition-v7.json"))
            .unwrap()
    );
    let bytes = include_bytes!("fixtures/workflow-policy-v1.json");
    assert_eq!(p.canonical().unwrap(), bytes);
    assert_eq!(
        p.digest().unwrap().to_string(),
        include_str!("fixtures/workflow-policy-v1.sha256").trim()
    );
    assert_eq!(Policy::decode(bytes, p.digest().unwrap()).unwrap(), p);
    let d = r.workflow_definition.as_ref().unwrap();
    assert!(d.workflow_file_approved(&t, &p));
    let source: QueueObservation =
        serde_json::from_str(include_str!("fixtures/group-attempt-v6.json")).unwrap();
    d.target
        .validate_source(
            &t,
            &source.group_attempt.unwrap(),
            source.finished_at_unix_ms,
        )
        .unwrap();
    assert_eq!(r.state(&t), "uncertain");
    let mut changed = d.clone();
    changed.file.content.push('\n');
    changed.file.byte_size = changed.file.content.len();
    changed.file.blob = blob_oid(&changed.file.content).unwrap();
    changed.file.content_sha256 = content_digest(&changed.file.content).to_string();
    changed.validate(&t).unwrap();
    assert!(!changed.workflow_file_approved(&t, &p));
}
#[test]
fn rejects_wrong_file_identity_unsafe_sources_mixed_versions_and_bounded_content() {
    let (t, r, _) = fixture();
    for case in [
        "version", "mixed", "bytes", "blob", "hash", "mode", "repo", "path", "nul", "large",
        "empty", "sequence", "url", "mutable", "query", "encoded", "foreign",
    ] {
        let mut v = json!(r);
        match case {
            "version" => v["version"] = json!(6),
            "mixed" => v["mutation_not_sent"] = json!(true),
            other => {
                let d = &mut v["workflow_definition"];
                match other {
                    "bytes" => d["file"]["byte_size"] = json!(1),
                    "blob" => d["file"]["blob"] = json!(format!("git:sha1:{}", "ab".repeat(20))),
                    "hash" => {
                        d["file"]["content_sha256"] = json!(format!("sha256:{}", "ab".repeat(32)))
                    }
                    "mode" => d["file"]["mode"] = json!(0o120000),
                    "repo" => d["file"]["repository_id"] = json!(12),
                    "path" => d["file"]["source"]["path"] = json!("README.md"),
                    "nul" => d["file"]["content"] = json!("x\0y"),
                    "large" => d["file"]["content"] = json!("x".repeat(MAX_FILE_BYTES + 1)),
                    "empty" => d["file"]["content"] = json!(""),
                    "sequence" => d["target"]["attempt_sequence"] = json!(0),
                    other => {
                        let f = &mut d["target"]["check"]["workflow"]["file"];
                        let old = f["repository_file_url"].as_str().unwrap().to_owned();
                        f["repository_file_url"] = json!(match other {
                            "url" => old.replace("https://github.com/", "https://example.com/"),
                            "mutable" => old.replace(&"33".repeat(20), "main"),
                            "query" => format!("{old}?x=1"),
                            "encoded" => old.replace("check.yml", "check%2Eyml"),
                            "foreign" => old.replace("example-org/demo", "other/demo"),
                            _ => unreachable!(),
                        });
                    }
                }
            }
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
fn canonical_policy_checks_exact_required_set_context_and_noncanonical_input() {
    let (t, _, p) = fixture();
    let mut duplicate = p.clone();
    duplicate.checks.push(p.checks[0].clone());
    assert!(duplicate.canonical().is_err());
    let mut other = p.clone();
    other.checks[0].required.app_id = 8;
    assert!(other.matches_required(&t).is_err());
    let mut other = p.clone();
    other.checks[0].expected_context.environment = "ubuntu-latest".into();
    assert!(other.canonical().is_err());
    let mut other = p.clone();
    other.checks[0].path = ".github/workflows/../../secrets.yml".into();
    assert!(other.canonical().is_err());
    assert!(Policy::decode(&serde_json::to_vec_pretty(&p).unwrap(), p.digest().unwrap()).is_err());
    let mut ordered = p.clone();
    let mut second = p.checks[0].clone();
    second.required.name = "AAA".into();
    ordered.checks.push(second);
    let mut reverse = ordered.clone();
    reverse.checks.reverse();
    assert_eq!(ordered.digest().unwrap(), reverse.digest().unwrap());
}
