use super::*;
use sha2::{Digest, Sha256};
use treeharbor_github_contract::restore_delivery::RestoreDeliveryReport;
use treeharbor_metadata::github::{RestoreDeliveryContext, RestoreDeliveryEvidence};

async fn retained_rows(raw: &tokio_postgres::Client) -> Vec<Value> {
    let mut values = Vec::new();
    for table in [
        "th_github_event_recoveries",
        "th_github_event_recovery_attempts",
        "th_github_event_recovery_pages",
        "th_github_event_channels",
        "th_github_events",
        "th_github_event_deliveries",
        "th_outbox",
    ] {
        let rows = raw.query(&format!("SELECT coalesce(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb) FROM {table} t"), &[]).await.unwrap();
        values.push(rows[0].get(0));
    }
    values
}

#[tokio::test]
#[ignore = "requires PostgreSQL; restore provider status cannot replace signed inbox evidence"]
async fn restore_delivery_compares_signed_inbox_without_settling_old_dispatch() {
    for inbox in ["missing", "received", "payload_conflict", "no_details"] {
        let mut f = Fixture::new().await;
        let channel = events::ready(&mut f).await;
        enable(&mut f).await;
        let id = plan(&mut f, &spec(channel)).await;
        let a = delivery(700);
        let mut d = if inbox == "no_details" {
            let auth = recoverer(&mut f).await;
            f.db.record_github_recovery_page(
                &auth,
                &cmd("page"),
                id,
                &page(std::slice::from_ref(&a), None, None),
            )
            .await
            .unwrap();
            details(&a, "checks")
        } else {
            prepare(&mut f, id, &a, "checks").await
        };
        if inbox == "received" || inbox == "payload_conflict" {
            f.db.accept_github_event(a.guid, &events::verified("checks", 7))
                .await
                .unwrap();
        }
        let owner = f.owner().await;
        f.db.revoke_github(&owner, &cmd("revoke"), f.binding)
            .await
            .unwrap();
        // Explicit quarantine fixture; actual backup staging is covered separately.
        let raw = connection(&f.schema).await;
        let manifest = "b3".repeat(32);
        let database: String = raw
            .query_one("SELECT current_database()::text", &[])
            .await
            .unwrap()
            .get(0);
        let digest = Sha256::digest(format!(
            "treeharbor-restore-quarantine\0v1\0{manifest}\0{database}"
        ))
        .to_vec();
        raw.execute("INSERT INTO th_operations(tenant,repository,key,request_digest,principal,kind,result) VALUES($1,$2,$3,$4,'restore-quarantine','restore.quarantine.v1',$5)", &[&f.scope.tenant(), &f.scope.repository(), &format!("restore-quarantine:{manifest}"), &digest, &json!({"version":1,"status":"quarantined","manifest_sha256":manifest,"database":database})]).await.unwrap();
        let reader = f.bridge().await;
        // A revoked binding alone cannot make an active event channel safe.
        assert!(matches!(
            f.db.github_restore_delivery_context(&reader, 1, f.binding, &manifest, id, 700)
                .await,
            Err(Error::HostEventState)
        ));
        f.db.revoke_github_event_channel(&owner, &cmd("revoke-channel"), channel)
            .await
            .unwrap();
        let ctx: RestoreDeliveryContext = serde_json::from_value(
            f.db.github_restore_delivery_context(&reader, 1, f.binding, &manifest, id, 700)
                .await
                .unwrap(),
        )
        .unwrap();
        let before = retained_rows(&raw).await;
        d.observed_at_unix_ms = now();
        d.attempt.status_code = Some(200); // Provider success still does not prove receipt.
        if inbox == "payload_conflict" {
            d.payload_sha256 = "de".repeat(32);
        }
        let s = RestoreDeliveryEvidence {
            version: 1,
            binding: f.binding,
            manifest_sha256: manifest,
            context_sha256: ctx.context_sha256,
            recovery: id,
            provider_id: 700,
            report: RestoreDeliveryReport {
                version: 1,
                app_id: 7,
                installation_id: 9,
                repository_id: 11,
                callback_url: ctx.callback_url,
                started_at_unix_ms: d.observed_at_unix_ms,
                finished_at_unix_ms: d.observed_at_unix_ms,
                before: d.clone(),
                after: d,
            },
        };
        assert!(matches!(
            f.db.record_github_restore_delivery(&owner, &cmd("denied"), &s)
                .await,
            Err(Error::Forbidden)
        ));
        let receipt =
            f.db.record_github_restore_delivery(&reader, &cmd("captured"), &s)
                .await
                .unwrap();
        assert_eq!(
            receipt["inbox"]["state"],
            if inbox == "no_details" {
                "missing"
            } else {
                inbox
            }
        );
        assert_eq!(
            receipt["classification"],
            match inbox {
                "no_details" => "no_retained_payload",
                "payload_conflict" => "differs_from_retained_payload",
                _ => "matches_retained_payload",
            }
        );
        for field in [
            "prior_dispatch_settled",
            "event_processing_verified",
            "reconciliation_complete",
            "publication_authorized",
        ] {
            assert_eq!(receipt[field], false);
        }
        assert_eq!(before, retained_rows(&raw).await);
        for case in [
            "app", "scope", "callback", "id", "guid", "old", "future", "order", "digest",
        ] {
            let mut bad = s.clone();
            match case {
                "app" => bad.report.app_id += 1,
                "scope" => bad.report.repository_id += 1,
                "callback" => {
                    bad.report.callback_url = "https://other.example/github/events".into()
                }
                "id" => bad.report.after.attempt.id += 1,
                "guid" => bad.report.after.attempt.guid = Uuid::new_v4(),
                "old" | "future" => {
                    let delta = if case == "old" { -300000 } else { 60000 };
                    bad.report.started_at_unix_ms += delta;
                    bad.report.finished_at_unix_ms += delta;
                    bad.report.before.observed_at_unix_ms += delta;
                    bad.report.after.observed_at_unix_ms += delta;
                }
                "order" => bad.report.after.observed_at_unix_ms -= 1,
                _ => bad.context_sha256 = "ba".repeat(32),
            }
            assert!(
                f.db.record_github_restore_delivery(&reader, &cmd(case), &bad)
                    .await
                    .is_err(),
                "{case}"
            );
        }
        let mut changed = s.clone();
        changed.report.after.attempt.status_code = Some(500);
        let r =
            f.db.record_github_restore_delivery(&reader, &cmd("changed"), &changed)
                .await
                .unwrap();
        assert_eq!(r["classification"], "changed_during_capture");
        assert_eq!(r["inbox"]["state"], "capture_inconsistent");
        assert!(matches!(
            f.db.record_github_restore_delivery(&reader, &cmd("captured"), &changed)
                .await,
            Err(Error::IdempotencyConflict)
        ));
        assert_eq!(before, retained_rows(&raw).await);
        f.db.cancel_github_recovery(&owner, &cmd("cancel"), id)
            .await
            .unwrap();
        assert!(matches!(
            f.db.record_github_restore_delivery(&reader, &cmd("stale"), &s)
                .await,
            Err(Error::RestoreContextChanged)
        ));
        f.db = Metadata::new(connection(&f.schema).await).await.unwrap();
        assert_eq!(
            f.db.record_github_restore_delivery(&reader, &cmd("captured"), &s)
                .await
                .unwrap(),
            receipt
        );
        let historian = f.reader().await;
        assert_eq!(
            f.db.github_restore_delivery(&historian, "captured")
                .await
                .unwrap(),
            receipt
        );
        f.db.set_repository_access(&f.scope, "bridge", &[P::Read], "operator")
            .await
            .unwrap();
        assert!(
            f.db.record_github_restore_delivery(&reader, &cmd("captured"), &s)
                .await
                .is_err()
        );
        cleanup(&f.schema).await;
    }
}
