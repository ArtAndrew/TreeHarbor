use super::*;
use treeharbor_metadata::github::{BindingSpec, PullTarget, RestoreEffect};

fn context_fixture() -> RestoreContext {
    let spec:BindingSpec=serde_json::from_value(json!({"version":1,"app_id":7,"client_id":"Iv1_test","bridge":"bridge",
        "upstream":{"installation_id":9,"repository_id":11,"owner_id":13,"owner":"example-org","name":"demo"}})).unwrap();
    RestoreContext {
        version: 1,
        tenant: "test".into(),
        repository: Uuid::from_bytes([1; 16]),
        epoch: 1,
        binding: Uuid::from_bytes([2; 16]),
        specification: spec,
        manifest_sha256: "aa".repeat(32),
        restored_database: "restored".into(),
        quarantine_sha256: "bb".repeat(32),
        other_bindings: vec![],
        effects: vec![],
        reconciliation_complete: false,
    }
}
fn effect(kind: RestoreEffectKind, id: u8) -> RestoreEffect {
    RestoreEffect {
        kind,
        id: Uuid::from_bytes([id; 16]),
        branch: Some(Uuid::from_bytes([3; 16])),
        native_object_hex: Some("22".repeat(32)),
        expected_git: Some(format!("git:sha1:{}", "33".repeat(20))),
        prepared_git: Some(format!("git:sha1:{}", "44".repeat(20))),
        target: None,
        pull_number: None,
        pull_node: None,
        attempt: None,
        state: Some("prepared".into()),
        cancel_requested: false,
        row_sha256: "55".repeat(32),
    }
}
fn target() -> PullTarget {
    PullTarget {
        operation: Uuid::from_bytes([4; 16]),
        branch_id: Uuid::from_bytes([3; 16]),
        git_revision: format!("git:sha1:{}", "44".repeat(20)),
        base: "main".into(),
        title: "Change".into(),
        body: "Retained original marker".into(),
        draft: true,
        previous: None,
    }
}
fn inventory() -> RestoreInventory {
    RestoreInventory {
        version: 1,
        binding: serde_json::from_value(json!(context_fixture().specification.upstream)).unwrap(),
        app_id: 7,
        repository_node_id: "R_test".into(),
        default_branch: "main".into(),
        started_at_unix: 10,
        finished_at_unix: 11,
        heads: vec![],
        pulls: vec![],
        token_revoked: true,
        token_expires_at: "later".into(),
        reconciliation_complete: false,
    }
}
fn host_pull(number: u64) -> treeharbor_github::InventoryPull {
    serde_json::from_value(json!({"id":format!("PR_{number}"),"number":number,"state":"MERGED","merged":true,"isDraft":false,
        "headRefName":format!("treeharbor/{}",target().branch_id.simple()),"headRefOid":"44".repeat(20),
        "baseRefName":"main","baseRefOid":"66".repeat(20),"headRepository":{"databaseId":11},"baseRepository":{"databaseId":11},
        "mergeCommit":{"oid":"77".repeat(20)}})).unwrap()
}

#[test]
fn restore_assessment_keeps_missing_attempt_and_distinguishes_each_current_branch_state() {
    for (oid, expected) in [
        (None, "branch_absent"),
        (Some("44"), "head_matches_prepared"),
        (Some("33"), "head_matches_expected"),
        (Some("99"), "head_differs"),
    ] {
        let mut c = context_fixture();
        c.effects.push(effect(RestoreEffectKind::Branch, 5));
        let mut h = inventory();
        if let Some(oid) = oid {
            h.heads.push(serde_json::from_value(json!({"id":"REF_test","name":format!("treeharbor/{}",target().branch_id.simple()),
            "prefix":"refs/heads/","target":{"__typename":"Commit","oid":oid.repeat(20)}})).unwrap());
        }
        let report = assess(&c, &c, &h).unwrap();
        assert_eq!(report["findings"][0]["classification"], expected);
        assert!(report["findings"][0]["retained_attempt"].is_null());
        assert_eq!(report["publication_authorized"], false);
        assert_eq!(report["reconciliation_complete"], false);
    }
}
#[test]
fn restore_assessment_never_adopts_matching_closed_prs_with_unknown_operation_markers() {
    let mut c = context_fixture();
    let mut p = effect(RestoreEffectKind::Pull, 4);
    p.target = Some(target());
    p.state = Some("requested".into());
    c.effects.push(p);
    let mut h = inventory();
    h.pulls = vec![host_pull(8), host_pull(9)];
    let mut foreign = host_pull(10);
    foreign.head_repository.as_mut().unwrap().database_id = 99;
    h.pulls.push(foreign);
    let report = assess(&c, &c, &h).unwrap();
    assert_eq!(
        report["findings"][0]["classification"],
        "pull_identity_unresolved"
    );
    assert_eq!(report["findings"][0]["candidate_numbers"], json!([8, 9]));
    assert_eq!(
        report["unmapped_to_selected_binding"]["pulls"],
        json!([8, 9])
    );
    assert_eq!(report["findings"][0]["metadata_and_marker_verified"], false);
}
#[test]
fn restore_assessment_checks_known_pr_identity_code_and_keeps_queue_and_checks_unresolved() {
    let mut c = context_fixture();
    let mut p = effect(RestoreEffectKind::Pull, 4);
    p.target = Some(target());
    p.pull_number = Some(8);
    p.pull_node = Some("PR_8".into());
    c.effects.push(p);
    c.effects.push(effect(RestoreEffectKind::Queue, 6));
    c.effects.push(effect(RestoreEffectKind::Check, 7));
    for (kind, expected) in [
        ("matching", "known_pull_code_matches"),
        ("missing", "known_pull_absent"),
        ("foreign", "known_pull_identity_differs"),
        ("node", "known_pull_identity_differs"),
        ("moved", "known_pull_code_differs"),
    ] {
        let mut h = inventory();
        let mut p = host_pull(8);
        match kind {
            "foreign" => p.head_repository = None,
            "node" => p.id = "PR_other".into(),
            "moved" => p.head_ref_oid = "88".repeat(20),
            _ => {}
        }
        if kind != "missing" {
            h.pulls.push(p);
        }
        let r = assess(&c, &c, &h).unwrap();
        assert_eq!(r["findings"][0]["classification"], expected);
        for i in [1, 2] {
            assert_eq!(
                r["findings"][i]["classification"],
                "external_inspection_required"
            );
        }
        assert_eq!(r["publication_authorized"], false);
    }
}
#[test]
fn restore_assessment_rejects_changed_retained_context_host_identity_and_false_completion() {
    let c = context_fixture();
    for kind in [
        "digest",
        "manifest",
        "epoch",
        "other-binding",
        "host",
        "app",
        "complete",
    ] {
        let mut after = c.clone();
        let mut h = inventory();
        match kind {
            "digest" => after.quarantine_sha256 = "cc".repeat(32),
            "manifest" => after.manifest_sha256 = "cc".repeat(32),
            "epoch" => after.epoch = 2,
            "other-binding" => after.other_bindings.push(Uuid::from_bytes([10; 16])),
            "host" => h.binding.repository_id = 88,
            "app" => h.app_id = 88,
            "complete" => h.reconciliation_complete = true,
            _ => {}
        }
        assert!(
            matches!(assess(&c, &after, &h), Err(Error::RestoreAssessment)),
            "{kind}"
        );
    }
}
