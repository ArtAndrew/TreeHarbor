use super::repository_review::{capture, verify_base};
use super::*;
use treeharbor_metadata::github::{
    Observation, RestoreActivateSpec, RestoreReconnectProbe, RestoreReconnectSpec,
    RestoreReviewSpec,
};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}
async fn setup() -> (Projection, RestoreContext, RestoreReconnectSpec) {
    let mut p = Projection::new().await;
    p.f.db
        .revoke_github(&p.owner, &cmd("revoke"), p.binding)
        .await
        .unwrap();
    journal(&p, &"91".repeat(32), None).await;
    let ctx: RestoreContext = serde_json::from_value(
        p.f.db
            .github_restore_context(&p.reader, 1, p.binding, &"91".repeat(32))
            .await
            .unwrap(),
    )
    .unwrap();
    let spec = RestoreReconnectSpec {
        version: 1,
        manifest_sha256: ctx.manifest_sha256.clone(),
        source_binding: p.binding,
        new_binding: Uuid::new_v4(),
        binding: ctx.specification.clone(),
        ttl_seconds: 300,
        evidence_sha256: "92".repeat(32),
        old_writers_and_runtimes_fenced: true,
        old_credentials_cannot_mint: true,
        old_tokens_revoked_or_expired: true,
        prior_host_mutations_settled: true,
        host_writes_controlled: true,
        host_policy_reviewed: true,
    };
    (p, ctx, spec)
}
fn probe(plan: &Value, s: &RestoreReconnectSpec) -> RestoreReconnectProbe {
    let n = now();
    RestoreReconnectProbe {
        version: 1,
        plan_key: plan["key"].as_str().unwrap().into(),
        plan_sha256: restore_digest(plan).unwrap(),
        started_at_unix: n,
        finished_at_unix: n,
        observation: Observation {
            version: 1,
            api_version: "2026-03-10".into(),
            observed_at_unix: n,
            app_id: s.binding.app_id,
            binding: s.binding.upstream.clone(),
            repository_node_id: "R_fixture".into(),
            owner_type: "Organization".into(),
            private: false,
            default_branch: "main".into(),
            archived: false,
            disabled: false,
            read_connection_verified: true,
            probe_token_revoked: true,
            queue_eligibility: "public_organization_eligible".into(),
            queue_configuration_verified: false,
            publication_ready: false,
        },
    }
}
async fn ready(
    p: &mut Projection,
    ctx: &RestoreContext,
    s: &RestoreReconnectSpec,
    reporter: &Authorization,
) -> (Value, RestoreActivateSpec) {
    capture(p, ctx, "before-plan-inventory").await;
    verify_base(p, ctx, "base-pin").await;
    let plan =
        p.f.db
            .plan_github_restore_reconnect(&p.owner, &cmd("plan"), s)
            .await
            .unwrap();
    let after = plan["observe_after_unix"].as_u64().unwrap();
    if now() < after {
        tokio::time::sleep(std::time::Duration::from_secs(after - now())).await;
    }
    let proof = probe(&plan, s);
    p.f.db
        .record_github_restore_reconnect_probe(reporter, &cmd("probe"), &proof)
        .await
        .unwrap();
    capture(p, ctx, "inventory").await;
    let review = RestoreReviewSpec {
        version: 1,
        manifest_sha256: ctx.manifest_sha256.clone(),
        inventory_keys: vec!["inventory".into()],
    };
    let r =
        p.f.db
            .github_restore_review(&p.owner, 1, &review)
            .await
            .unwrap();
    assert_eq!(r["assessment"]["local_review_complete"], true);
    (
        plan,
        RestoreActivateSpec {
            version: 1,
            plan_key: "plan".into(),
            probe_key: "probe".into(),
            review,
            assessment_sha256: r["assessment_sha256"].as_str().unwrap().into(),
            external_assertions_still_valid: true,
        },
    )
}
#[tokio::test]
#[ignore = "requires PostgreSQL; concurrent reconnect admission, durable retry and restored-scope fences; external assertions are fixtures"]
async fn restore_reconnect_has_one_winner_and_never_revives_old_or_revoked_bindings() {
    let (mut p, ctx, mut s) = setup().await;
    let reporter = actor(&mut p.f, "new-bridge", K::Bridge, &[P::HostRead]).await;
    s.binding.bridge = "new-bridge".into();
    let (plan, activation) = ready(&mut p, &ctx, &s, &reporter).await;
    assert_eq!(plan["external_fencing_verified"], false);
    for (label, auth) in [("agent", &p.reader), ("bridge", &p.bridge)] {
        assert!(matches!(
            p.f.db
                .activate_github_restore_reconnect(auth, &cmd(label), &activation)
                .await,
            Err(MetaError::Forbidden)
        ));
    }
    let mut old = activation.clone();
    old.review.inventory_keys = vec!["before-plan-inventory".into()];
    let old_review =
        p.f.db
            .github_restore_review(&p.owner, 1, &old.review)
            .await
            .unwrap();
    assert_eq!(old_review["assessment"]["local_review_complete"], true);
    old.assessment_sha256 = old_review["assessment_sha256"].as_str().unwrap().into();
    assert!(
        p.f.db
            .activate_github_restore_reconnect(&p.owner, &cmd("before-plan-inventory"), &old)
            .await
            .is_err()
    );
    let mut bad = activation.clone();
    bad.assessment_sha256 = "00".repeat(32);
    assert!(
        p.f.db
            .activate_github_restore_reconnect(&p.owner, &cmd("bad-hash"), &bad)
            .await
            .is_err()
    );
    bad = activation.clone();
    bad.external_assertions_still_valid = false;
    assert!(
        p.f.db
            .activate_github_restore_reconnect(&p.owner, &cmd("no-ack"), &bad)
            .await
            .is_err()
    );
    assert!(matches!(
        p.f.db
            .bind_github(&p.owner, &cmd("normal-before"), Uuid::new_v4(), &s.binding)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    let mut peer = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let left = cmd("activate-left");
    let right = cmd("activate-right");
    let (a, b) = tokio::join!(
        p.f.db
            .activate_github_restore_reconnect(&p.owner, &left, &activation),
        peer.activate_github_restore_reconnect(&p.owner, &right, &activation)
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let (key, accepted) = if let Ok(v) = a {
        ("activate-left", v)
    } else {
        ("activate-right", b.unwrap())
    };
    assert_eq!(accepted["external_fencing_verified"], false);
    assert_eq!(accepted["old_operations_replayed"], false);
    assert_eq!(accepted["merge_authorized"], false);
    let sql = connection(&p.f.schema).await;
    assert_eq!(
        sql.query_one(
            "SELECT count(*) FROM th_github_bindings WHERE state='active'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        1
    );
    assert_eq!(
        sql.query_one(
            "SELECT state FROM th_github_bindings WHERE binding=$1",
            &[&p.binding]
        )
        .await
        .unwrap()
        .get::<_, String>(0),
        "revoked"
    );
    let status = peer
        .github_restore_reconnect_status(&p.owner, 1, "plan")
        .await
        .unwrap();
    assert_eq!(status["reconciliation_required"], false);
    assert_eq!(status["current_binding_state"], "active");
    assert!(
        peer.github_restore_reconnect_context(&p.reader, 1, "plan")
            .await
            .is_err()
    );
    assert!(
        peer.cancel_github_restore_reconnect(&p.owner, &cmd("cancel-after"), "plan")
            .await
            .is_err()
    );
    assert_eq!(
        peer.activate_github_restore_reconnect(&p.owner, &cmd(key), &activation)
            .await
            .unwrap(),
        accepted
    );
    peer.revoke_github(&p.owner, &cmd("revoke-new"), s.new_binding)
        .await
        .unwrap();
    assert_eq!(
        peer.activate_github_restore_reconnect(&p.owner, &cmd(key), &activation)
            .await
            .unwrap(),
        accepted
    );
    assert_eq!(
        peer.github_restore_reconnect_status(&p.owner, 1, "plan")
            .await
            .unwrap()["current_binding_state"],
        "revoked"
    );
    // Normal binding rotation can proceed after an admitted restore, but never
    // reuse a revoked ID or inherit admission across another quarantine.
    assert!(
        peer.bind_github(&p.owner, &cmd("reuse-id"), s.new_binding, &s.binding)
            .await
            .is_err()
    );
    let another = Uuid::new_v4();
    peer.bind_github(&p.owner, &cmd("normal-after"), another, &s.binding)
        .await
        .unwrap();
    peer.revoke_github(&p.owner, &cmd("revoke-another"), another)
        .await
        .unwrap();
    journal(&p, &"93".repeat(32), None).await;
    assert_eq!(
        peer.github_restore_reconnect_status(&p.owner, 1, "plan")
            .await
            .unwrap()["reconciliation_required"],
        true
    );
    assert!(matches!(
        peer.bind_github(&p.owner, &cmd("new-restore"), Uuid::new_v4(), &s.binding)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    assert_eq!(
        peer.activate_github_restore_reconnect(&p.owner, &cmd(key), &activation)
            .await
            .unwrap(),
        accepted
    );
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
#[tokio::test]
#[ignore = "requires PostgreSQL; fresh plan/probe identities, pin loss after passing review and cancellation"]
async fn restore_reconnect_rechecks_source_pins_and_operator_plan() {
    let (mut p, ctx, s) = setup().await;
    let wrong = actor(&mut p.f, "other-reader", K::Bridge, &[P::HostRead]).await;
    let reporter = p.reader.clone();
    let (plan, activation) = ready(&mut p, &ctx, &s, &reporter).await;
    assert!(
        p.f.db
            .github_restore_reconnect_context(&wrong, 1, "plan")
            .await
            .is_err()
    );
    let original = probe(&plan, &s);
    for case in [
        "before-plan",
        "wrong-plan",
        "archived",
        "not-revoked",
        "old",
        "wrong-app",
    ] {
        let mut proof = original.clone();
        match case {
            "before-plan" => {
                proof.started_at_unix = plan["created_at_unix"].as_u64().unwrap();
            }
            "wrong-plan" => proof.plan_sha256 = "00".repeat(32),
            "archived" => proof.observation.archived = true,
            "not-revoked" => proof.observation.probe_token_revoked = false,
            "old" => {
                proof.started_at_unix -= 300;
                proof.finished_at_unix -= 300;
                proof.observation.observed_at_unix -= 300;
            }
            _ => proof.observation.app_id += 1,
        }
        assert!(
            p.f.db
                .record_github_restore_reconnect_probe(&p.reader, &cmd(case), &proof)
                .await
                .is_err(),
            "{case}"
        );
    }
    let sql = connection(&p.f.schema).await;
    sql.execute(
        "DELETE FROM th_object_pins WHERE holder LIKE 'restore-decision:%'",
        &[],
    )
    .await
    .unwrap();
    assert!(matches!(
        p.f.db
            .activate_github_restore_reconnect(&p.owner, &cmd("missing-pin"), &activation)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
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
    let cancelled =
        p.f.db
            .cancel_github_restore_reconnect(&p.owner, &cmd("cancel"), "plan")
            .await
            .unwrap();
    assert_eq!(cancelled["outcome"], "cancelled");
    assert!(
        p.f.db
            .github_restore_reconnect_context(&p.reader, 1, "plan")
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .activate_github_restore_reconnect(&p.owner, &cmd("cancelled"), &activation)
            .await
            .is_err()
    );
    // Historical receipts remain readable/replayable, not new authority.
    assert_eq!(
        p.f.db
            .plan_github_restore_reconnect(&p.owner, &cmd("plan"), &s)
            .await
            .unwrap(),
        plan
    );
    assert!(matches!(
        p.f.db
            .bind_github(&p.owner, &cmd("still-blocked"), Uuid::new_v4(), &s.binding)
            .await,
        Err(MetaError::HostRecoveryRequired)
    ));
    cleanup(&p.f.schema).await;
}
