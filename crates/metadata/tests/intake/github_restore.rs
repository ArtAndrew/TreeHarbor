use super::*;
use sha2::{Digest, Sha256};
use treeharbor_metadata::github::{RestoreContext, RestoreEffectKind};

// A durable journal fixture, not evidence of pg_dump/restore or live GitHub recovery.
pub(super) async fn journal(p: &Projection, manifest: &str, database_override: Option<&str>) {
    let sql = connection(&p.f.schema).await;
    let current: String = sql
        .query_one("SELECT current_database()::text", &[])
        .await
        .unwrap()
        .get(0);
    let database = database_override.unwrap_or(&current);
    let digest = Sha256::digest(format!(
        "treeharbor-restore-quarantine\0v1\0{manifest}\0{database}"
    ))
    .to_vec();
    let result =
        json!({"version":1,"status":"quarantined","manifest_sha256":manifest,"database":database});
    sql.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)",
        &[&p.f.scope.tenant(),&p.f.scope.repository(),&format!("restore-quarantine:{manifest}"),&digest,&result]).await.unwrap();
}
pub(super) async fn retained(p: &Projection) -> Vec<Value> {
    let sql = connection(&p.f.schema).await;
    let mut rows = Vec::new();
    for table in [
        "th_operations",
        "th_outbox",
        "th_github_bindings",
        "th_github_publications",
        "th_github_pulls",
        "th_github_queue",
        "th_github_dequeues",
        "th_github_event_channels",
        "th_github_event_recoveries",
        "th_github_event_recovery_pages",
        "th_github_event_recovery_attempts",
        "th_github_check_publications",
        "th_object_pins",
        "th_security_audit",
        "th_access_tokens",
    ] {
        rows.push(sql.query_one(&format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t"),&[]).await.unwrap().get(0));
    }
    rows
}

#[tokio::test]
#[ignore = "requires PostgreSQL; scoped restored context retains exact pre-attempt history without mutation"]
async fn restore_context_requires_assigned_read_access_current_manifest_and_revoked_bindings() {
    let mut p = Projection::new().await;
    let (id, _, value) = publication::requested(&mut p).await;
    let mapping = publication::mapping(&value);
    p.f.db
        .prepare_github_publication(&p.bridge, &cmd("restore-prepare"), id, &mapping)
        .await
        .unwrap();
    let manifest = "11".repeat(32);
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await,
        Err(MetaError::HostBindingState)
    ));
    p.f.db
        .revoke_github(&p.owner, &cmd("restore-revoke"), p.binding)
        .await
        .unwrap();
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    journal(&p, &manifest, None).await;
    let before = retained(&p).await;
    let first =
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await
            .unwrap();
    let second =
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await
            .unwrap();
    assert_eq!(first, second);
    let alternative = connection(&p.f.schema).await;
    alternative
        .batch_execute("SET TimeZone='Pacific/Auckland'; SET bytea_output='escape'")
        .await
        .unwrap();
    let mut alternative = Metadata::new(alternative).await.unwrap();
    assert_eq!(
        first,
        alternative
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await
            .unwrap()
    );
    let context: RestoreContext = serde_json::from_value(first.clone()).unwrap();
    assert_eq!(context.effects.len(), 1);
    let effect = &context.effects[0];
    assert_eq!(effect.kind, RestoreEffectKind::Branch);
    assert_eq!(effect.id, id);
    assert_eq!(effect.prepared_git, Some(mapping.git_revision));
    assert!(effect.attempt.is_none());
    assert_eq!(effect.state.as_deref(), Some("prepared"));
    assert_eq!(effect.native_object_hex.as_ref().unwrap().len(), 64);
    assert!(!context.reconciliation_complete);
    assert_eq!(context.manifest_sha256, manifest);
    for invalid in ["", "AB", &"A".repeat(64)] {
        assert!(matches!(
            p.f.db
                .github_restore_context(&p.reader, 1, p.binding, invalid)
                .await,
            Err(MetaError::Invalid(_))
        ));
    }
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.owner, 1, p.binding, &manifest)
            .await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 2, p.binding, &manifest)
            .await,
        Err(MetaError::Fenced)
    ));
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 1, Uuid::new_v4(), &manifest)
            .await,
        Err(MetaError::NotFound)
    ));
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &"22".repeat(32))
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    assert_eq!(retained(&p).await, before);
    let wrong_bridge = actor(&mut p.f, "other-bridge", K::Bridge, &[P::HostRead]).await;
    assert!(matches!(
        p.f.db
            .github_restore_context(&wrong_bridge, 1, p.binding, &manifest)
            .await,
        Err(MetaError::Forbidden)
    ));
    let stale = "33".repeat(32);
    journal(&p, &stale, Some("old_restored_database")).await;
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &stale)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    p.f.db
        .cancel_github_publication(&p.owner, &cmd("restore-cancel"), id)
        .await
        .unwrap();
    let changed =
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await
            .unwrap();
    assert_ne!(
        first["effects"][0]["row_sha256"],
        changed["effects"][0]["row_sha256"]
    );
    assert_eq!(changed["effects"][0]["state"], "cancelled");
    assert!(changed["effects"][0]["attempt"].is_null());
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; refuse oversized retained history without truncating context"]
async fn restore_context_rejects_oversized_effect_history() {
    let mut p = Projection::new().await;
    let (id, _, _) = publication::requested(&mut p).await;
    p.f.db
        .revoke_github(&p.owner, &cmd("restore-revoke"), p.binding)
        .await
        .unwrap();
    let manifest = "44".repeat(32);
    journal(&p, &manifest, None).await;
    // Storage-bound fixture copies, not claims of executed publications.
    connection(&p.f.schema).await.execute("INSERT INTO th_github_publications SELECT (jsonb_populate_record(NULL::th_github_publications,to_jsonb(p)||jsonb_build_object('publication',gen_random_uuid(),'branch',gen_random_uuid()))).* FROM th_github_publications p CROSS JOIN generate_series(1,256) WHERE publication=$1",&[&id]).await.unwrap();
    let before = retained(&p).await;
    assert!(matches!(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await,
        Err(MetaError::RestoreContextLimit)
    ));
    assert_eq!(before, retained(&p).await);
    cleanup(&p.f.schema).await;
}

use treeharbor_metadata::github::{
    RestoreDecisionSpec, RestoreDecisionSubject, RestoreDisposition, restore_digest,
};

async fn decision_fixture() -> (Projection, RestoreDecisionSpec, String) {
    let mut p = Projection::new().await;
    let (operation, _, value) = publication::requested(&mut p).await;
    p.f.db
        .revoke_github(&p.owner, &cmd("decision-revoke"), p.binding)
        .await
        .unwrap();
    let manifest = "55".repeat(32);
    journal(&p, &manifest, None).await;
    let context =
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &manifest)
            .await
            .unwrap();
    let spec = RestoreDecisionSpec {
        version: 1,
        binding: p.binding,
        manifest_sha256: manifest,
        context_sha256: restore_digest(&context).unwrap(),
        evidence_sha256: "66".repeat(32),
        subject: RestoreDecisionSubject::Retained {
            effect_kind: RestoreEffectKind::Branch,
            operation,
        },
        disposition: RestoreDisposition::AwaitEvidence,
        previous_key: None,
        note: "Inspect post-backup host state before any replay.".into(),
    };
    (p, spec, value["native_revision"].as_str().unwrap().into())
}

#[tokio::test]
#[ignore = "requires PostgreSQL; durable owner decisions, stale-context refusal and historical replay"]
async fn restore_decision_retains_history_and_replays_after_reconnect() {
    let (mut p, spec, _) = decision_fixture().await;
    let before = retained(&p).await;
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&p.reader, &cmd("denied"), &spec)
            .await,
        Err(MetaError::Forbidden)
    ));
    let read_only = actor(&mut p.f, "readonly-human", K::Human, &[P::Read]).await;
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&read_only, &cmd("denied"), &spec)
            .await,
        Err(MetaError::Forbidden)
    ));
    // Provisioning the read-only actor changes audit/token tables, so snapshot again.
    let before_record = retained(&p).await;
    let receipt =
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("decision-1"), &spec)
            .await
            .unwrap();
    assert_eq!(receipt["sequence"], 1);
    assert_eq!(receipt["recorded_by"], "human");
    assert_eq!(receipt["publication_authorized"], false);
    assert_eq!(receipt["external_facts_verified"], false);
    let after_record = retained(&p).await;
    // Only the operation receipt was inserted: no outbox, old operations, pins or credentials changed.
    assert_eq!(&before_record[1..], &after_record[1..]);
    assert_eq!(&before[2..8], &after_record[2..8]);
    let mut restarted = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    assert_eq!(
        receipt,
        restarted
            .record_github_restore_decision(&p.owner, &cmd("decision-1"), &spec)
            .await
            .unwrap()
    );
    assert_eq!(
        receipt,
        restarted
            .github_restore_decision(&read_only, "decision-1")
            .await
            .unwrap()
    );
    assert!(matches!(
        restarted.github_restore_decision(&read_only, "bind").await,
        Err(MetaError::NotFound)
    ));
    let mut changed = spec.clone();
    changed.note.push_str(" Changed.");
    assert!(matches!(
        restarted
            .record_github_restore_decision(&p.owner, &cmd("decision-1"), &changed)
            .await,
        Err(MetaError::IdempotencyConflict)
    ));
    let RestoreDecisionSubject::Retained { operation, .. } = spec.subject else {
        unreachable!()
    };
    p.f.db
        .cancel_github_publication(&p.owner, &cmd("cancel-after-review"), operation)
        .await
        .unwrap();
    changed.previous_key = Some("decision-1".into());
    assert!(matches!(
        restarted
            .record_github_restore_decision(&p.owner, &cmd("decision-2"), &changed)
            .await,
        Err(MetaError::RestoreContextChanged)
    ));
    // A lost response remains recoverable after context movement; it grants no new effect.
    assert_eq!(
        receipt,
        restarted
            .record_github_restore_decision(&p.owner, &cmd("decision-1"), &spec)
            .await
            .unwrap()
    );
    let context =
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &spec.manifest_sha256)
            .await
            .unwrap();
    changed.context_sha256 = restore_digest(&context).unwrap();
    changed.disposition = RestoreDisposition::DoNotReplay;
    let second = restarted
        .record_github_restore_decision(&p.owner, &cmd("decision-2"), &changed)
        .await
        .unwrap();
    assert_eq!(second["sequence"], 2);
    assert_eq!(second["specification"]["previous_key"], "decision-1");
    assert_eq!(
        receipt,
        restarted
            .github_restore_decision(&read_only, "decision-1")
            .await
            .unwrap()
    );
    let binding_spec = serde_json::from_value(context["specification"].clone()).unwrap();
    assert!(matches!(
        p.f.db
            .bind_github(
                &p.owner,
                &cmd("still-blocked"),
                Uuid::new_v4(),
                &binding_spec
            )
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; competing decisions cannot replace one another and failed pin copies roll back"]
async fn restore_decision_serializes_reviewers_and_preserves_source_independently() {
    let (mut p, mut spec, _) = decision_fixture().await;
    // An accepted graph with a separate owner pin, not a projection pin whose
    // owning publication intentionally keeps its foreign-key reference forever.
    let RevisionRef::Native(base) = p.f.revision else {
        unreachable!()
    };
    let Object::NativeCommit(mut commit) = p.f.stage.get(base).unwrap() else {
        unreachable!()
    };
    commit.message = "Recovered source fixture".into();
    let root = put(&p.f.stage, Object::NativeCommit(commit));
    let source_lease = upload_graph(&mut p.f.db, &p.f.depot, &p.f.stage, root).await;
    let source_holder = "recovery-upload-fixture".to_owned();
    let request = treeharbor_metadata::Request::new(
        p.f.scope.clone(),
        "fixture-source",
        "fixture",
        "pin",
        Authority::Connected,
        1,
        b"fixture-source",
    )
    .unwrap();
    let treeharbor_metadata::Begin::Fresh(m) = p.f.db.begin(request).await.unwrap() else {
        panic!()
    };
    m.pin_object(&source_holder, &source_lease).await.unwrap();
    m.complete(json!({"fixture":true}), vec![]).await.unwrap();
    let native = RevisionRef::Native(root).to_string();
    let read_only = actor(&mut p.f, "decision-reader", K::Human, &[P::Read]).await;
    spec.disposition = RestoreDisposition::PreserveSource {
        source_holder: source_holder.clone(),
        native_revision: native.clone(),
    };
    let before = retained(&p).await;
    let mut wrong = spec.clone();
    wrong.disposition = RestoreDisposition::PreserveSource {
        source_holder: source_holder.clone(),
        native_revision: format!("native:sha256:{}", "ab".repeat(32)),
    };
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("bad-source"), &wrong)
            .await,
        Err(MetaError::ReceiptMismatch)
    ));
    assert_eq!(before, retained(&p).await);
    wrong.disposition = RestoreDisposition::PreserveSource {
        source_holder: "missing-source".into(),
        native_revision: native,
    };
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("missing-source"), &wrong)
            .await,
        Err(MetaError::NotFound)
    ));
    assert_eq!(before, retained(&p).await);
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let c1 = cmd("concurrent-1");
    let c2 = cmd("concurrent-2");
    let (a, b) = tokio::join!(
        p.f.db.record_github_restore_decision(&p.owner, &c1, &spec),
        other.record_github_restore_decision(&p.owner, &c2, &spec)
    );
    let (winner, loser) = if a.is_ok() {
        (a.unwrap(), b)
    } else {
        (b.unwrap(), a)
    };
    assert!(matches!(loser, Err(MetaError::RestoreDecisionConflict)));
    let pin = winner["source_preservation"]["holder"].as_str().unwrap();
    let sql = connection(&p.f.schema).await;
    let n: i64 = sql
        .query_one(
            "SELECT count(*) FROM th_object_pins WHERE holder LIKE 'restore-decision:%'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(n, 1);
    let lease =
        p.f.db
            .lease_pinned_root(&p.f.scope, pin, Uuid::new_v4(), 60)
            .await
            .unwrap();
    // Explicit fixture removal of original owners/holds isolates the new pin's
    // GC protection. This is not a claim of complete node/workspace cleanup.
    sql.execute(
        "DELETE FROM th_object_pins WHERE tenant=$1 AND digest=$2 AND holder<>$3",
        &[
            &p.f.scope.tenant(),
            &lease.key().id().as_bytes().to_vec(),
            &pin,
        ],
    )
    .await
    .unwrap();
    sql.execute(
        "DELETE FROM th_object_holds WHERE tenant=$1 AND digest=$2",
        &[&p.f.scope.tenant(), &lease.key().id().as_bytes().to_vec()],
    )
    .await
    .unwrap();
    assert!(
        p.f.db
            .try_retire_object(lease.key(), 0)
            .await
            .unwrap()
            .is_none()
    );
    let mut reconnect = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    assert_eq!(
        winner,
        reconnect
            .github_restore_decision(&read_only, winner["key"].as_str().unwrap())
            .await
            .unwrap()
    );
    assert!(
        reconnect
            .lease_pinned_root(&p.f.scope, pin, Uuid::new_v4(), 60)
            .await
            .is_ok()
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; foreign roots, non-commit roots and unknown retained effects cannot become recovery evidence"]
async fn restore_decision_refuses_foreign_and_wrong_kind_source_without_partial_writes() {
    let (mut p, mut spec, native) = decision_fixture().await;
    let other = Scope::new(p.f.scope.tenant(), Uuid::new_v4()).unwrap();
    p.f.db
        .create_repository(&other, Authority::Connected)
        .await
        .unwrap();
    p.f.db
        .set_repository_access(&other, "human", &[P::Read], "test")
        .await
        .unwrap();
    let foreign_token =
        p.f.db
            .issue_repository_token(&other, "human", &[P::Read], 3600, "test")
            .await
            .unwrap();
    let foreign_reader =
        p.f.db
            .authorize(&foreign_token.secret, &other, P::Read)
            .await
            .unwrap();
    let sql = connection(&p.f.schema).await;
    sql.execute("INSERT INTO th_object_pins(tenant,repository,holder,digest,generation) SELECT tenant,$1,'foreign-only-root',digest,generation FROM th_object_pins WHERE tenant=$2 AND repository=$3 AND holder=$4",
        &[&other.repository(),&p.f.scope.tenant(),&p.f.scope.repository(),&format!("github-projection:{}",p.id)]).await.unwrap();
    let snapshot = upload_graph(&mut p.f.db, &p.f.depot, &p.f.stage, p.f.base).await;
    let request = treeharbor_metadata::Request::new(
        p.f.scope.clone(),
        "fixture-snapshot",
        "fixture",
        "pin",
        Authority::Connected,
        1,
        b"snapshot",
    )
    .unwrap();
    let treeharbor_metadata::Begin::Fresh(m) = p.f.db.begin(request).await.unwrap() else {
        panic!()
    };
    m.pin_object("snapshot-only-root", &snapshot).await.unwrap();
    m.complete(json!({"fixture":true}), vec![]).await.unwrap();
    let before = retained(&p).await;
    spec.disposition = RestoreDisposition::PreserveSource {
        source_holder: "foreign-only-root".into(),
        native_revision: native,
    };
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("foreign-root"), &spec)
            .await,
        Err(MetaError::NotFound)
    ));
    assert_eq!(before, retained(&p).await);
    spec.disposition = RestoreDisposition::PreserveSource {
        source_holder: "snapshot-only-root".into(),
        native_revision: RevisionRef::Native(p.f.base).to_string(),
    };
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("snapshot-root"), &spec)
            .await,
        Err(MetaError::ReceiptMismatch)
    ));
    assert_eq!(before, retained(&p).await);
    let mut missing = spec.clone();
    missing.disposition = RestoreDisposition::AwaitEvidence;
    missing.subject = RestoreDecisionSubject::Retained {
        effect_kind: RestoreEffectKind::Branch,
        operation: Uuid::new_v4(),
    };
    assert!(matches!(
        p.f.db
            .record_github_restore_decision(&p.owner, &cmd("missing-effect"), &missing)
            .await,
        Err(MetaError::NotFound)
    ));
    assert_eq!(before, retained(&p).await);
    spec.disposition = RestoreDisposition::AwaitEvidence;
    p.f.db
        .record_github_restore_decision(&p.owner, &cmd("local-decision"), &spec)
        .await
        .unwrap();
    assert!(matches!(
        p.f.db
            .github_restore_decision(&foreign_reader, "local-decision")
            .await,
        Err(MetaError::NotFound)
    ));
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact projector proof, post-verification context race and permission revocation"]
async fn restore_mapping_rechecks_context_and_authority_before_committing_proof() {
    let (mut p, mut spec, _) = decision_fixture().await;
    let RevisionRef::Native(root) = p.f.revision else {
        unreachable!()
    };
    spec.disposition = RestoreDisposition::PreserveSource {
        source_holder: format!("base:{}", root.hex()),
        native_revision: p.f.revision.to_string(),
    };
    p.f.db
        .record_github_restore_decision(&p.owner, &cmd("source-decision"), &spec)
        .await
        .unwrap();
    // Independent SHA-1 of Git's commit header + the fixture's native projection:
    // empty Git tree; Fixture <fixture@example.invalid> 0 +0000 as author/committer;
    // message "base" without a trailing newline. Not taken from the verifier.
    let expected = "git:sha1:2acbe39a88a8dfe59d8370733d81f21ec8a76d04";
    let RevisionRef::Git(git) = expected.parse().unwrap() else {
        unreachable!()
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
    let before = retained(&p).await;
    assert!(matches!(
        p.f.db
            .open_restore_source_export(&p.reader, 1, "source-decision", 60)
            .await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        p.f.db
            .record_github_restore_mapping(
                &p.owner,
                &cmd("bad-proof"),
                "source-decision",
                &format!("git:sha1:{}", "00".repeat(20)),
                &proof
            )
            .await,
        Err(MetaError::ReceiptMismatch)
    ));
    let Object::NativeCommit(mut other_commit) = p.f.stage.get(root).unwrap() else {
        unreachable!()
    };
    other_commit.message = "other".into();
    let other_root = put(&p.f.stage, Object::NativeCommit(other_commit));
    let other_expected = "git:sha1:5ecc92ddd78d6b01fd04343542c04e2ffaff90dd";
    let RevisionRef::Git(other_git) = other_expected.parse().unwrap() else {
        unreachable!()
    };
    let other_proof = treeharbor_git_bridge::verify_mapping(
        &p.f.stage,
        other_root,
        other_git,
        scratch.path(),
        Default::default(),
    )
    .unwrap();
    assert!(matches!(
        p.f.db
            .record_github_restore_mapping(
                &p.owner,
                &cmd("wrong-root-proof"),
                "source-decision",
                other_expected,
                &other_proof
            )
            .await,
        Err(MetaError::ReceiptMismatch)
    ));
    assert_eq!(before, retained(&p).await);
    let receipt =
        p.f.db
            .record_github_restore_mapping(
                &p.owner,
                &cmd("mapping"),
                "source-decision",
                expected,
                &proof,
            )
            .await
            .unwrap();
    assert_eq!(receipt["git_mapping_verified"], true);
    assert_eq!(receipt["host_state_verified"], false);
    assert_eq!(receipt["publication_authorized"], false);
    let after = retained(&p).await;
    assert_eq!(&before[1..], &after[1..]);
    let RestoreDecisionSubject::Retained { operation, .. } = spec.subject else {
        unreachable!()
    };
    p.f.db
        .cancel_github_publication(&p.owner, &cmd("move-context"), operation)
        .await
        .unwrap();
    let changed = retained(&p).await;
    assert!(matches!(
        p.f.db
            .record_github_restore_mapping(
                &p.owner,
                &cmd("stale-proof"),
                "source-decision",
                expected,
                &proof
            )
            .await,
        Err(MetaError::RestoreContextChanged)
    ));
    assert_eq!(changed, retained(&p).await);
    assert_eq!(
        Some(receipt),
        p.f.db
            .github_restore_mapping_replay(&p.owner, &cmd("mapping"), "source-decision", expected)
            .await
            .unwrap()
    );
    p.f.db
        .set_repository_access(&p.f.scope, "human", &[P::Read], "test")
        .await
        .unwrap();
    assert!(matches!(
        p.f.db
            .github_restore_mapping_replay(&p.owner, &cmd("mapping"), "source-decision", expected)
            .await,
        Err(MetaError::Forbidden)
    ));
    cleanup(&p.f.schema).await;
}
