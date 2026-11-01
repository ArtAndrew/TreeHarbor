use super::*;
use sha2::{Digest, Sha256};
use treeharbor_metadata::github::{RestoreContext, RestoreEffectKind};

// A durable journal fixture, not evidence of pg_dump/restore or live GitHub recovery.
async fn journal(p: &Projection, manifest: &str, database_override: Option<&str>) {
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
async fn retained(p: &Projection) -> Vec<Value> {
    let sql = connection(&p.f.schema).await;
    let mut rows = Vec::new();
    for table in [
        "th_operations",
        "th_outbox",
        "th_github_bindings",
        "th_github_publications",
        "th_github_pulls",
        "th_github_queue",
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
