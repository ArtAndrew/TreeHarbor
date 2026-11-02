use super::*;
use treeharbor_metadata::github::{
    RestoreInventoryItem, RestoreInventoryPage, RestoreInventorySpec, RestoreReviewSpec,
};
const BASE_GIT: &str = "git:sha1:2acbe39a88a8dfe59d8370733d81f21ec8a76d04";

async fn capture(p: &mut Projection, ctx: &RestoreContext, key: &str) {
    let page = RestoreInventoryPage {
        version: 1,
        index: 0,
        items: vec![RestoreInventoryItem::Branch {
            node: "REF_main".into(),
            name: "main".into(),
            git_revision: BASE_GIT.into(),
        }],
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let spec = RestoreInventorySpec {
        version: 1,
        binding: ctx.binding,
        manifest_sha256: ctx.manifest_sha256.clone(),
        context_sha256: restore_digest(ctx).unwrap(),
        app_id: ctx.specification.app_id,
        installation_id: ctx.specification.upstream.installation_id,
        repository_id: ctx.specification.upstream.repository_id,
        repository_node: "R_fixture".into(),
        default_branch: "main".into(),
        started_at_unix: now,
        finished_at_unix: now,
        head_count: 1,
        pull_count: 0,
        page_sha256: vec![restore_digest(&page).unwrap()],
        token_revoked: true,
        token_expires_at: "fixture".into(),
    };
    p.f.db
        .start_github_restore_inventory(&p.reader, &cmd(key), &spec)
        .await
        .unwrap();
    p.f.db
        .record_github_restore_inventory_page(&p.reader, 1, key, &page)
        .await
        .unwrap();
    p.f.db
        .seal_github_restore_inventory(&p.reader, 1, key)
        .await
        .unwrap();
}
fn binding(r: &Value, id: Uuid) -> &Value {
    r["assessment"]["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["binding"] == json!(id))
        .unwrap()
}
async fn verify_base(p: &mut Projection, ctx: &RestoreContext, key: &str) {
    let RevisionRef::Native(root) = p.f.revision else {
        panic!()
    };
    let d = RestoreDecisionSpec {
        version: 2,
        binding: ctx.binding,
        manifest_sha256: ctx.manifest_sha256.clone(),
        context_sha256: restore_digest(ctx).unwrap(),
        evidence_sha256: "99".repeat(32),
        subject: RestoreDecisionSubject::ExternalBranch {
            name: "main".into(),
        },
        disposition: RestoreDisposition::PreserveSource {
            source_holder: format!("base:{}", root.hex()),
            native_revision: p.f.revision.to_string(),
        },
        previous_key: None,
        note: "Preserve fixture base".into(),
    };
    p.f.db
        .record_github_restore_decision(&p.owner, &cmd(key), &d)
        .await
        .unwrap();
    let RevisionRef::Git(git) = BASE_GIT.parse().unwrap() else {
        panic!()
    };
    let scratch = private_temp();
    let proof = treeharbor_git_bridge::verify_mapping(
        &p.f.stage,
        root,
        git,
        scratch.path(),
        Default::default(),
    )
    .unwrap();
    p.f.db
        .record_github_restore_mapping(&p.owner, &cmd(&format!("{key}-map")), key, BASE_GIT, &proof)
        .await
        .unwrap();
}

#[tokio::test]
#[ignore = "requires PostgreSQL; all-binding review, real mappings, superseded decisions, independent native retention and no activation"]
async fn repository_review_cannot_omit_a_binding_or_inherit_a_passing_subset() {
    let mut p = Projection::new().await;
    let (operation, _, published) = publication::requested(&mut p).await;
    p.f.db
        .revoke_github(&p.owner, &cmd("first-revoke"), p.binding)
        .await
        .unwrap();
    let second = Uuid::new_v4();
    let spec: BindingSpec =
        serde_json::from_value(
            serde_json::from_str::<Value>(include_str!("../fixtures/github-bind-v1.json")).unwrap()
                ["specification"]
                .clone(),
        )
        .unwrap();
    p.f.db
        .bind_github(&p.owner, &cmd("second-bind"), second, &spec)
        .await
        .unwrap();
    p.f.db
        .revoke_github(&p.owner, &cmd("second-revoke"), second)
        .await
        .unwrap();
    let manifest = "97".repeat(32);
    journal(&p, &manifest, None).await;
    let first_ctx: RestoreContext = serde_json::from_value(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await
            .unwrap(),
    )
    .unwrap();
    let second_ctx: RestoreContext = serde_json::from_value(
        p.f.db
            .github_restore_context(&p.reader, 1, second, &manifest)
            .await
            .unwrap(),
    )
    .unwrap();
    let mut review = RestoreReviewSpec {
        version: 1,
        manifest_sha256: manifest.clone(),
        inventory_keys: vec![],
    };
    let before = retained(&p).await;
    let r =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(r["assessment"]["bindings"].as_array().unwrap().len(), 2);
    assert_eq!(r["assessment"]["incomplete_bindings"], 2);
    assert_eq!(binding(&r, p.binding)["decisions"]["blockers"]["count"], 1);
    assert_eq!(
        binding(&r, p.binding)["sources"]["missing_native"]["count"],
        1
    );
    assert_eq!(before, retained(&p).await);
    assert!(matches!(
        p.f.db.github_restore_review(&p.reader, 1, &review).await,
        Err(MetaError::Forbidden)
    ));
    assert!(
        p.f.db
            .github_restore_review(&p.owner, 2, &review)
            .await
            .is_err()
    );
    capture(&mut p, &first_ctx, "first-inventory").await;
    review.inventory_keys.push("first-inventory".into());
    let r =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(binding(&r, second)["inventory_status"], "missing");
    assert_eq!(binding(&r, p.binding)["sources"]["missing_git"]["count"], 1);
    capture(&mut p, &second_ctx, "second-inventory").await;
    review.inventory_keys.push("second-inventory".into());
    // Same upstream/Git identity is not permission to reuse another binding's
    // restore evidence; each selected scope needs a corresponding retained proof.
    verify_base(&mut p, &first_ctx, "first-base").await;
    let r =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(binding(&r, p.binding)["sources"]["missing_git"]["count"], 0);
    assert_eq!(binding(&r, second)["sources"]["missing_git"]["count"], 1);
    verify_base(&mut p, &second_ctx, "second-base").await;
    let r =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(binding(&r, second)["local_review_complete"], true);
    assert_eq!(binding(&r, p.binding)["local_review_complete"], false);
    let sql = connection(&p.f.schema).await;
    let holder: String = sql
        .query_one(
            "SELECT pin_holder FROM th_github_publications WHERE publication=$1",
            &[&operation],
        )
        .await
        .unwrap()
        .get(0);
    let mut decision = RestoreDecisionSpec {
        version: 2,
        binding: p.binding,
        manifest_sha256: manifest.clone(),
        context_sha256: restore_digest(&first_ctx).unwrap(),
        evidence_sha256: "98".repeat(32),
        subject: RestoreDecisionSubject::Retained {
            effect_kind: RestoreEffectKind::Branch,
            operation,
        },
        disposition: RestoreDisposition::PreserveSource {
            source_holder: holder,
            native_revision: published["native_revision"].as_str().unwrap().into(),
        },
        previous_key: None,
        note: "Retain unpublished native source".into(),
    };
    let preserved =
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("native-source"), &decision)
            .await
            .unwrap();
    let r =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(
        binding(&r, p.binding)["sources"]["missing_native"]["count"],
        0
    );
    assert_eq!(
        binding(&r, p.binding)["decisions"]["blockers"]["sample"][0]["classification"],
        "decision_not_final"
    );
    decision.disposition = RestoreDisposition::DoNotReplay;
    decision.previous_key = Some("native-source".into());
    p.f.db
        .record_github_restore_decision(&p.owner, &cmd("reviewed-branch"), &decision)
        .await
        .unwrap();
    let complete =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(complete["assessment"]["local_review_complete"], true);
    assert_eq!(complete["assessment"]["publication_authorized"], false);
    assert_eq!(
        complete["assessment"]["prior_host_mutations_settled"],
        false
    );
    assert_eq!(
        complete["assessment_sha256"],
        restore_digest(&complete["assessment"]).unwrap()
    );
    assert!(matches!(
        p.f.db
            .bind_github(&p.owner, &cmd("still-no-bind"), Uuid::new_v4(), &spec)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    let mut reopened = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    assert_eq!(
        reopened
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap()["assessment"],
        complete["assessment"]
    );
    // Selection order cannot change the state commitment; omission must change it.
    review.inventory_keys.reverse();
    assert_eq!(
        reopened
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap()["assessment_sha256"],
        complete["assessment_sha256"]
    );
    let mut omitted = review.clone();
    omitted.inventory_keys.pop();
    assert_eq!(
        reopened
            .github_restore_review(&p.owner, 1, &omitted)
            .await
            .unwrap()["assessment"]["local_review_complete"],
        false
    );
    let mut wrong = review.clone();
    wrong.manifest_sha256 = "00".repeat(32);
    assert!(
        reopened
            .github_restore_review(&p.owner, 1, &wrong)
            .await
            .is_err()
    );
    let mut duplicate = review.clone();
    duplicate.inventory_keys.push("first-inventory".into());
    assert!(
        reopened
            .github_restore_review(&p.owner, 1, &duplicate)
            .await
            .is_err()
    );
    // A later unresolved decision must supersede a previously passing review.
    decision.disposition = RestoreDisposition::AwaitEvidence;
    decision.previous_key = Some("reviewed-branch".into());
    reopened
        .record_github_restore_decision(&p.owner, &cmd("reopened-decision"), &decision)
        .await
        .unwrap();
    let r = reopened
        .github_restore_review(&p.owner, 1, &review)
        .await
        .unwrap();
    assert_eq!(r["assessment"]["local_review_complete"], false);
    assert_ne!(r["assessment_sha256"], complete["assessment_sha256"]);
    sql.execute(
        "DELETE FROM th_object_pins WHERE holder=$1",
        &[&preserved["source_preservation"]["holder"].as_str().unwrap()],
    )
    .await
    .unwrap();
    let r = reopened
        .github_restore_review(&p.owner, 1, &review)
        .await
        .unwrap();
    assert_eq!(
        binding(&r, p.binding)["sources"]["missing_native"]["count"],
        1
    );
    reopened
        .cancel_github_publication(&p.owner, &cmd("changed-context"), operation)
        .await
        .unwrap();
    let r = reopened
        .github_restore_review(&p.owner, 1, &review)
        .await
        .unwrap();
    assert_eq!(binding(&r, p.binding)["inventory_status"], "stale_context");
    assert_eq!(
        binding(&r, p.binding)["decisions"]["blockers"]["sample"][0]["classification"],
        "stale_decision"
    );
    // The review reads, hashes and reports only; none of these inspections
    // manufacture dispatches or reactivate either old binding.
    assert_eq!(
        sql.query_one(
            "SELECT count(*) FROM th_github_bindings WHERE state='active'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    cleanup(&p.f.schema).await;
}
