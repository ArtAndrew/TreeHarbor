use super::super::restore::{journal, retained};
use super::*;
use treeharbor_metadata::github::{
    DequeueSpec, RestoreContext, RestoreDecisionSpec, RestoreDecisionSubject, RestoreDisposition,
    restore_digest,
};

// Synthetic restored rows exercise real constraints, not a live webhook replay.
async fn recoveries(f: &QueueFixture, binding: Uuid) -> Vec<Uuid> {
    let sql = connection(&f.p.f.schema).await;
    let s = &f.p.f.scope;
    let channel = Uuid::new_v4();
    sql.execute(r#"INSERT INTO th_github_event_channels(tenant,repository,channel,binding,epoch,app_id,installation_id,repository_id,owner_id,principal,state,max_events,max_bytes,max_aliases,revoked_at) VALUES($1,$2,$3,$4,1,7,9,11,13,'owner','revoked',1000,1000000,2,clock_timestamp())"#,
        &[&s.tenant(), &s.repository(), &channel, &binding]).await.unwrap();
    let delivery = Uuid::new_v4();
    let mut ids = Vec::new();
    for _ in 0..3 {
        let recovery = Uuid::new_v4();
        sql.execute(r#"INSERT INTO th_github_event_recoveries(tenant,repository,recovery,channel,binding,epoch,principal,bridge,callback_url,since_unix_ms,max_pages,max_deliveries,expires_at,cancelled) VALUES($1,$2,$3,$4,$5,1,'owner','bridge','https://example.test/hook',1,100,100,clock_timestamp()+interval '1 hour',true)"#,
            &[&s.tenant(), &s.repository(), &recovery, &channel, &binding]).await.unwrap();
        // Provider attempt 41 in two recoveries must not be collapsed. A shared
        // delivery GUID across attempts is also not a unique operation key.
        for provider in [41i64, 42] {
            sql.execute(r#"INSERT INTO th_github_event_recovery_attempts(tenant,repository,recovery,provider_id,delivery,observation) VALUES($1,$2,$3,$4,$5,'{"fixture":true}')"#,
                &[&s.tenant(), &s.repository(), &recovery, &provider, &delivery]).await.unwrap();
        }
        sql.execute(r#"INSERT INTO th_github_event_recovery_pages(tenant,repository,recovery,sequence,report) VALUES($1,$2,$3,1,'{"cursor":null}')"#,
            &[&s.tenant(), &s.repository(), &recovery]).await.unwrap();
        ids.push(recovery);
    }
    ids.sort();
    ids
}

#[tokio::test]
#[ignore = "requires PostgreSQL; retained dequeue/redelivery identities, stale decisions and strict limits"]
async fn restore_extended_context_preserves_composite_attempts_and_invalidates_old_decisions() {
    let mut f = QueueFixture::new().await;
    f.request().await;
    let dequeue = Uuid::new_v4();
    f.p.f
        .db
        .request_github_dequeue(
            &f.owner,
            &cmd("remove"),
            dequeue,
            &DequeueSpec {
                queue: f.id.to_string(),
                ttl_seconds: 60,
                scope: "pull_request".into(),
            },
        )
        .await
        .unwrap();
    f.p.f
        .db
        .revoke_github(&f.p.owner, &cmd("revoke"), f.p.binding)
        .await
        .unwrap();
    let manifest = "71".repeat(32);
    journal(&f.p, &manifest, None).await;
    let recovery_ids = recoveries(&f, f.p.binding).await;
    let foreign_binding = Uuid::new_v4();
    let sql = connection(&f.p.f.schema).await;
    sql.execute("INSERT INTO th_github_bindings(tenant,repository,binding,epoch,bridge,specification,state,principal,revoked_at) SELECT tenant,repository,$2,epoch,bridge,specification,'revoked',principal,clock_timestamp() FROM th_github_bindings WHERE binding=$1",&[&f.p.binding,&foreign_binding]).await.unwrap();
    let foreign_recoveries = recoveries(&f, foreign_binding).await;
    let before = retained(&f.p).await;
    let value =
        f.p.f
            .db
            .github_restore_context(&f.p.reader, 1, f.p.binding, &manifest)
            .await
            .unwrap();
    assert_eq!(before, retained(&f.p).await);
    let context: RestoreContext = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(context.version, 2);
    assert_eq!(context.dequeues.len(), 1);
    assert_eq!(context.dequeues[0].id, dequeue);
    assert_eq!(context.dequeues[0].queue, f.id);
    assert!(context.dequeues[0].attempt.is_none());
    assert_eq!(context.recoveries.len(), 3);
    assert_eq!(context.other_bindings, vec![foreign_binding]);
    assert_eq!(context.recovery_pages.len(), 3);
    assert_eq!(context.redeliveries.len(), 6);
    for (i, r) in context.redeliveries.iter().enumerate() {
        assert_eq!(r.recovery, recovery_ids[i / 2]);
        assert_eq!(r.provider_id, 41 + (i % 2) as i64);
        assert_eq!(r.delivery, context.redeliveries[0].delivery);
        assert!(r.dispatch.is_none());
    }
    let mut spec = RestoreDecisionSpec {
        version: 2,
        binding: f.p.binding,
        manifest_sha256: manifest.clone(),
        context_sha256: restore_digest(&context).unwrap(),
        evidence_sha256: "72".repeat(32),
        subject: RestoreDecisionSubject::RetainedDequeue { operation: dequeue },
        disposition: RestoreDisposition::AwaitEvidence,
        previous_key: None,
        note: "Retain all effects; this is not permission to repeat a remote call.".into(),
    };
    // New subjects cannot masquerade as the old wire version or another operation.
    spec.version = 1;
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_decision(&f.p.owner, &cmd("bad-version"), &spec)
            .await,
        Err(MetaError::Invalid(_))
    ));
    spec.version = 2;
    for (i, subject) in [
        spec.subject.clone(),
        RestoreDecisionSubject::RetainedRecovery {
            recovery: recovery_ids[0],
        },
        RestoreDecisionSubject::RetainedRedelivery {
            recovery: recovery_ids[0],
            provider_id: 41,
        },
        RestoreDecisionSubject::RetainedRedelivery {
            recovery: recovery_ids[1],
            provider_id: 41,
        },
    ]
    .into_iter()
    .enumerate()
    {
        spec.subject = subject;
        let receipt =
            f.p.f
                .db
                .record_github_restore_decision(&f.p.owner, &cmd(&format!("review-{i}")), &spec)
                .await
                .unwrap();
        assert_eq!(receipt["sequence"], 1);
        assert_eq!(receipt["publication_authorized"], false);
    }
    spec.subject = RestoreDecisionSubject::RetainedRedelivery {
        recovery: foreign_recoveries[0],
        provider_id: 41,
    };
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_decision(&f.p.owner, &cmd("foreign-binding-attempt"), &spec)
            .await,
        Err(MetaError::NotFound)
    ));
    spec.subject = RestoreDecisionSubject::RetainedRedelivery {
        recovery: recovery_ids[0],
        provider_id: 999,
    };
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_decision(&f.p.owner, &cmd("missing-attempt"), &spec)
            .await,
        Err(MetaError::NotFound)
    ));
    spec.subject = RestoreDecisionSubject::RetainedRedelivery {
        recovery: recovery_ids[0],
        provider_id: 42,
    };
    let sql = connection(&f.p.f.schema).await;
    sql.execute(r#"UPDATE th_github_event_recovery_attempts SET details='{"fixture":"late-detail"}',dispatch=$2,started_at=clock_timestamp() WHERE recovery=$1 AND provider_id=42"#,&[&recovery_ids[0],&Uuid::new_v4()]).await.unwrap();
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_decision(&f.p.owner, &cmd("stale-attempt"), &spec)
            .await,
        Err(MetaError::RestoreContextChanged)
    ));
    let changed =
        f.p.f
            .db
            .github_restore_context(&f.p.reader, 1, f.p.binding, &manifest)
            .await
            .unwrap();
    assert_ne!(restore_digest(&changed).unwrap(), spec.context_sha256);
    spec.context_sha256 = restore_digest(&changed).unwrap();
    let receipt =
        f.p.f
            .db
            .record_github_restore_decision(&f.p.owner, &cmd("fresh-attempt"), &spec)
            .await
            .unwrap();
    // Loss of the newly covered fields produces the old context hash; no silent upgrade.
    let mut v1 = changed.clone();
    v1["version"] = json!(1);
    for key in ["dequeues", "recoveries", "recovery_pages", "redeliveries"] {
        v1.as_object_mut().unwrap().remove(key);
    }
    spec.context_sha256 = restore_digest(&v1).unwrap();
    assert!(matches!(
        f.p.f
            .db
            .record_github_restore_decision(&f.p.owner, &cmd("old-context"), &spec)
            .await,
        Err(MetaError::RestoreContextChanged)
    ));
    // Changing a dequeue attempt also changes context even without a new queue row.
    sql.execute(
        r#"UPDATE th_github_dequeues SET attempt=$2,started_at=clock_timestamp() WHERE dequeue=$1"#,
        &[&dequeue, &Uuid::new_v4()],
    )
    .await
    .unwrap();
    let after =
        f.p.f
            .db
            .github_restore_context(&f.p.reader, 1, f.p.binding, &manifest)
            .await
            .unwrap();
    assert_ne!(
        restore_digest(&changed).unwrap(),
        restore_digest(&after).unwrap()
    );
    // Bound the full attempt list across recovery requests, not per parent.
    for recovery in &recovery_ids {
        sql.execute(r#"INSERT INTO th_github_event_recovery_attempts(tenant,repository,recovery,provider_id,delivery,observation) SELECT tenant,repository,recovery,n,$2,'{}'::jsonb FROM th_github_event_recoveries CROSS JOIN generate_series(100,197) n WHERE recovery=$1"#,&[recovery,&Uuid::new_v4()]).await.unwrap();
    }
    let before_limit = retained(&f.p).await;
    assert!(matches!(
        f.p.f
            .db
            .github_restore_context(&f.p.reader, 1, f.p.binding, &manifest)
            .await,
        Err(MetaError::RestoreContextLimit)
    ));
    assert_eq!(before_limit, retained(&f.p).await);
    // Immutable historical receipts remain readable even when a fresh scan is blocked.
    let reader = actor(&mut f.p.f, "restore-reader", K::Human, &[P::Read]).await;
    assert_eq!(
        f.p.f
            .db
            .github_restore_decision(&reader, "fresh-attempt")
            .await
            .unwrap(),
        receipt
    );
    cleanup(&f.p.f.schema).await;
}
