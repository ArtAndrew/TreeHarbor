use super::*;
use crate::github_projection::restore::{journal, retained};
use treeharbor_github_contract::restore_check::{CheckIdentity, CheckSnapshot, RestoreCheckReport};
use treeharbor_metadata::github::{RestoreCheckContext, RestoreCheckEvidence};

#[tokio::test]
#[ignore = "requires PostgreSQL; exact check recovery never transfers historical success to validation"]
async fn restore_check_evidence_retains_exact_target_without_changing_old_attempts() {
    for observed in [false, true] {
        let mut p = Projection::new().await;
        let (check, _, _) = pull(&mut p, 0).await;
        let (owner, bridge) = actors(&mut p).await;
        let id = Uuid::new_v4();
        let t = request(&mut p, &owner, id, check).await;
        let identity = CheckIdentity {
            id: 73,
            node_id: "check-73".into(),
        };
        if observed {
            let attempt = Uuid::new_v4();
            p.f.db
                .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
                .await
                .unwrap();
            let pref = preflight(&p, &t).await;
            p.f.db
                .dispatch_github_check_publication(&bridge, &cmd("dispatch"), id, attempt, &pref)
                .await
                .unwrap();
            let mut r = report(&p, &t, Some(pref), cp::Outcome::Observed).await;
            r.version = 2;
            r.check.as_mut().unwrap().node_id = Some(identity.node_id.clone());
            p.f.db
                .report_github_check_publication(&bridge, &cmd("published"), id, attempt, &r)
                .await
                .unwrap();
        }
        p.f.db
            .revoke_github(&p.owner, &cmd("revoke"), p.binding)
            .await
            .unwrap();
        let manifest = "b1".repeat(32);
        journal(&p, &manifest, None).await;
        let before = retained(&p).await;
        let ctx: RestoreCheckContext = serde_json::from_value(
            p.f.db
                .github_restore_check_context(&p.reader, 1, p.binding, &manifest, id, &identity)
                .await
                .unwrap(),
        )
        .unwrap();
        assert_eq!(before, retained(&p).await);
        assert_eq!(ctx.target, t);
        if observed {
            for bad in [
                CheckIdentity {
                    id: 74,
                    node_id: identity.node_id.clone(),
                },
                CheckIdentity {
                    id: 73,
                    node_id: "other".into(),
                },
            ] {
                assert!(matches!(
                    p.f.db
                        .github_restore_check_context(&p.reader, 1, p.binding, &manifest, id, &bad)
                        .await,
                    Err(MetaError::HostPublicationState)
                ));
            }
        }
        let now = report(&p, &t, None, cp::Outcome::Unknown)
            .await
            .finished_at_unix_ms;
        let snapshot = CheckSnapshot {
            identity: identity.clone(),
            app_id: t.app_id,
            name: t.name().unwrap(),
            head_revision: t.source.head().unwrap().into(),
            external_id: Some(t.external_id().unwrap()),
            status: "completed".into(),
            conclusion: Some("success".into()),
        };
        let spec = RestoreCheckEvidence {
            version: 1,
            binding: p.binding,
            manifest_sha256: manifest,
            context_sha256: ctx.context_sha256,
            operation: id,
            identity,
            report: RestoreCheckReport {
                version: 1,
                app_id: t.app_id,
                installation_id: ctx.context.specification.upstream.installation_id,
                repository_id: t.source.pull().repository_id,
                started_at_unix_ms: now,
                finished_at_unix_ms: now,
                before: snapshot.clone(),
                after: snapshot,
                token_revoked: true,
            },
        };
        let wrong = actor(&mut p.f, "foreign-reader", K::Bridge, &[P::HostRead]).await;
        for auth in [&p.owner, &wrong] {
            assert!(matches!(
                p.f.db
                    .record_github_restore_check(auth, &cmd("denied"), &spec)
                    .await,
                Err(MetaError::Forbidden)
            ));
        }
        let before = retained(&p).await;
        let receipt =
            p.f.db
                .record_github_restore_check(&p.reader, &cmd("captured"), &spec)
                .await
                .unwrap();
        assert_eq!(receipt["metadata_matches"], true);
        assert_eq!(receipt["validation_authorized"], false);
        assert_eq!(receipt["publication_authorized"], false);
        assert_eq!(before[1..], retained(&p).await[1..]);
        for case in [
            "app",
            "installation",
            "repository",
            "identity",
            "old",
            "future",
            "duration",
            "digest",
        ] {
            let mut bad = spec.clone();
            match case {
                "app" => bad.report.app_id += 1,
                "installation" => bad.report.installation_id += 1,
                "repository" => bad.report.repository_id += 1,
                "identity" => bad.report.after.identity.id += 1,
                "old" => {
                    bad.report.started_at_unix_ms -= 300000;
                    bad.report.finished_at_unix_ms -= 300000;
                }
                "future" => {
                    bad.report.started_at_unix_ms += 60000;
                    bad.report.finished_at_unix_ms += 60000;
                }
                "duration" => bad.report.started_at_unix_ms -= 120001,
                _ => bad.context_sha256 = "b2".repeat(32),
            }
            let before = retained(&p).await;
            assert!(
                p.f.db
                    .record_github_restore_check(&p.reader, &cmd(case), &bad)
                    .await
                    .is_err(),
                "{case}"
            );
            assert_eq!(before, retained(&p).await);
        }
        let mut changed = spec.clone();
        changed.report.after.head_revision = "git:sha1:".to_owned() + &"44".repeat(20);
        assert_eq!(
            p.f.db
                .record_github_restore_check(&p.reader, &cmd("changed"), &changed)
                .await
                .unwrap()["classification"],
            "changed_during_capture"
        );
        changed.report.before = changed.report.after.clone();
        assert_eq!(
            p.f.db
                .record_github_restore_check(&p.reader, &cmd("wrong-code"), &changed)
                .await
                .unwrap()["classification"],
            "differs_from_retained_target"
        );
        assert!(matches!(
            p.f.db
                .record_github_restore_check(&p.reader, &cmd("captured"), &changed)
                .await,
            Err(MetaError::IdempotencyConflict)
        ));
        p.f.db
            .cancel_github_check_publication(&owner, &cmd("cancel"), id)
            .await
            .unwrap();
        let after_cancel =
            p.f.db
                .record_github_restore_check(&p.reader, &cmd("after-cancel"), &spec)
                .await;
        if observed {
            // Normal cancellation cannot rewrite an already observed terminal row.
            assert_eq!(after_cancel.unwrap()["metadata_matches"], true);
        } else {
            assert!(matches!(
                after_cancel,
                Err(MetaError::RestoreContextChanged)
            ));
        }
        let mut restarted = Metadata::new(connection(&p.f.schema).await).await.unwrap();
        assert_eq!(
            restarted
                .record_github_restore_check(&p.reader, &cmd("captured"), &spec)
                .await
                .unwrap(),
            receipt
        );
        restarted
            .set_repository_access(&p.f.scope, "bridge", &[], "operator")
            .await
            .unwrap();
        assert!(matches!(
            restarted
                .record_github_restore_check(&p.reader, &cmd("captured"), &spec)
                .await,
            Err(MetaError::Forbidden)
        ));
        cleanup(&p.f.schema).await;
    }
}
