//! Actual transactions and immutable rows; provider/runtime observations are fixtures.
use super::*;
use treeharbor_github_contract::host_policy as hp;
pub(super) fn policy(f: &QueueFixture, proposed: &AdmissionPolicy) -> AdmissionPolicy {
    let v: Value = serde_json::from_str(include_str!(
        "../fixtures/connected-admission-policy-v2.json"
    ))
    .unwrap();
    let mut p: AdmissionPolicy = serde_json::from_value(v["policy"].clone()).unwrap();
    let required = proposed.required(7, 1, f.p.binding, "main", 1).unwrap();
    p.managed = proposed.managed.clone();
    p.workflows = proposed.workflows.clone();
    let c = p.host_configuration.as_mut().unwrap();
    let contexts: Vec<_> = required.iter().map(|v| v.name.clone()).collect();
    c.profile.protection.value.as_mut().unwrap()["required_status_checks"]["contexts"] =
        json!(contexts);
    c.profile.protection.value.as_mut().unwrap()["required_status_checks"]["checks"] = json!(
        required
            .iter()
            .map(|v| json!({"context":v.name,"app_id":v.app_id}))
            .collect::<Vec<_>>()
    );
    c.visibility.legacy.as_mut().unwrap().required_contexts = Some(contexts);
    p.required(7, 1, f.p.binding, "main", 1).unwrap();
    p
}
impl Candidate {
    pub(super) async fn host_report(&mut self) -> QueueObservation {
        let v = self
            .f
            .p
            .f
            .db
            .github_queue_host_policy_context(&self.f.bridge, 1, self.f.id, 1)
            .await
            .unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../github-contract/tests/fixtures/host-policy-v2.json"
        ))
        .unwrap();
        let mut p: hp::Report = serde_json::from_value(fixture["report"].clone()).unwrap();
        p.target = serde_json::from_value(v["host_policy_target"].clone()).unwrap();
        let c = self.policy.host_configuration.as_ref().unwrap();
        p.repository_name = c.visibility.target.repository_name.clone();
        for snap in [&mut p.before, &mut p.after] {
            snap.group = self
                .membership
                .group_capture
                .as_ref()
                .unwrap()
                .before
                .clone();
            snap.group.target.configuration = c.profile.queue.clone();
            snap.review.pull = snap.group.target.pull.clone();
            snap.protection = c.profile.protection.clone();
            snap.rules = c.profile.rules.clone();
            snap.rulesets = c.profile.rulesets.clone();
            snap.visibility = Some(Box::new(c.visibility.clone()));
        }
        let mut r = self.f.report().await;
        r.version = 10;
        r.token_revoked = true;
        r.host_policy = Some(Box::new(p));
        r.validate(&self.f.target).unwrap();
        r
    }
    pub(super) async fn save_host(&mut self, label: &str, r: &QueueObservation) -> i64 {
        self.f
            .p
            .f
            .db
            .report_github_queue(&self.f.bridge, &cmd(label), self.f.id, self.attempt, r)
            .await
            .unwrap()["report_sequence"]
            .as_i64()
            .unwrap()
    }
    async fn with_host(&mut self, checks: i64, host: i64) -> Value {
        self.f
            .p
            .f
            .db
            .github_queue_admission_with_host_policy(
                &self.reader,
                1,
                self.f.id,
                checks,
                None,
                Some(host),
            )
            .await
            .unwrap()
    }
}
fn reason(v: &Value, r: &str) {
    assert!(
        v["evidence"]["host_policy"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!(r)),
        "{v}"
    );
}
#[tokio::test]
#[ignore = "requires PostgreSQL; actual complete managed evidence + current owner pin, latest capture and reporter revocation; fixture host/runtime"]
async fn current_host_policy_joins_exact_complete_checks_and_never_reuses_drift_or_revoked_reporter()
 {
    let mut c = Candidate::new_with_configuration(1, true).await;
    let released = vec![c.release(0, 0, 2, cp::Outcome::Observed).await];
    let checks = c.capture("current-checks", c.snapshot(&released)).await;
    let r = c.host_report().await;
    let host = c.save_host("current-host", &r).await;
    let v = c.with_host(checks, host).await;
    assert_eq!(v["version"], 2);
    assert_eq!(v["checks_verified"], true);
    assert_eq!(v["host_policy_verified"], true, "{v}");
    assert_eq!(v["admission_verified"], true);
    assert_eq!(v["merge_verified"], false);
    assert_eq!(c.save_host("current-host", &r).await, host);
    assert_eq!(v, c.with_host(checks, host).await);
    let mut drift = c.host_report().await;
    let p = drift.host_policy.as_mut().unwrap();
    for s in [&mut p.before, &mut p.after] {
        s.visibility.as_mut().unwrap().legacy.as_mut().unwrap().id = "BPR_replaced".into();
    }
    let changed = c.save_host("changed-host", &drift).await;
    let rejected = c.with_host(checks, changed).await;
    assert_eq!(rejected["checks_verified"], true);
    assert_eq!(rejected["admission_verified"], false);
    reason(&rejected, "owner_configuration_mismatch");
    reason(
        &c.with_host(checks, host).await,
        "newer_policy_capture_exists",
    );
    let r = c.host_report().await;
    let fresh = c.save_host("fresh-host", &r).await;
    assert_eq!(c.with_host(checks, fresh).await["admission_verified"], true);
    let raw = connection(&c.f.p.f.schema).await;
    let token: Uuid = raw
        .query_one(
            "SELECT token FROM th_github_queue_reports WHERE sequence=$1",
            &[&fresh],
        )
        .await
        .unwrap()
        .get(0);
    c.f.p
        .f
        .db
        .revoke_repository_token(&c.f.p.f.scope, token, "operator")
        .await
        .unwrap();
    let denied = c.with_host(checks, fresh).await;
    reason(&denied, "capture_submitter_not_trusted");
    assert_eq!(denied["host_policy_verified"], false);
    assert_eq!(denied["admission_verified"], false);
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; incomplete checks, stale pin, old capture, unknown token revocation and cancellation cannot admit"]
async fn current_host_policy_never_upgrades_incomplete_or_outdated_admission() {
    for case in [
        "checks",
        "policy",
        "legacy",
        "revocation",
        "cancel",
        "before_checks",
        "check_source",
        "membership_source",
        "membership_configuration",
    ] {
        let mut c =
            Candidate::new_with_configuration(if case == "checks" { 2 } else { 1 }, true).await;
        let released = vec![c.release(0, 0, 2, cp::Outcome::Observed).await];
        let earlier = if case == "before_checks" {
            Some(c.host_report().await)
        } else {
            None
        };
        let mut checks = c.capture("negative-checks", c.snapshot(&released)).await;
        if case == "check_source" {
            let raw = connection(&c.f.p.f.schema).await;
            let value: Value = raw
                .query_one(
                    "SELECT report FROM th_github_queue_reports WHERE sequence=$1",
                    &[&checks],
                )
                .await
                .unwrap()
                .get(0);
            let mut source: QueueObservation = serde_json::from_value(value).unwrap();
            source.token_revoked = false;
            checks = c.save_host("unrevoked-check-source", &source).await;
        }
        if case == "membership_source" {
            let mut source = c.membership.clone();
            source.token_revoked = false;
            c.save_host("unrevoked-membership-source", &source).await;
        }
        if case == "membership_configuration" {
            let mut source = c.membership.clone();
            let mut config = c
                .policy
                .host_configuration
                .as_ref()
                .unwrap()
                .profile
                .queue
                .clone()
                .unwrap();
            config.check_timeout_minutes += 1;
            let group = source.group_capture.as_mut().unwrap();
            group.before.target.configuration = Some(config.clone());
            group.after.target.configuration = Some(config);
            source.snapshot = Some(group.after.target.clone());
            c.save_host("changed-membership-settings", &source).await;
        }
        let mut r = if let Some(r) = earlier {
            r
        } else {
            c.host_report().await
        };
        if case == "legacy" {
            r.version = 9;
            let p = r.host_policy.as_mut().unwrap();
            p.version = 1;
            p.before.visibility = None;
            p.after.visibility = None;
        }
        if case == "revocation" {
            r.token_revoked = false;
        }
        let host = c.save_host("negative-host", &r).await;
        match case {
            "policy" => {
                set(&mut c.f.p, &c.owner, 1, None).await;
            }
            "cancel" => {
                c.f.p
                    .f
                    .db
                    .cancel_github_queue(&c.f.owner, &cmd("cancel-current"), c.f.id)
                    .await
                    .unwrap();
            }
            _ => (),
        }
        let v = c.with_host(checks, host).await;
        assert_eq!(v["admission_verified"], false, "{case}: {v}");
        assert_eq!(v["merge_verified"], false);
        match case {
            "checks" => assert_eq!(v["checks_verified"], false),
            "policy" => reason(&v, "owner_policy_not_current"),
            "legacy" => reason(&v, "owner_configuration_mismatch"),
            "revocation" => reason(&v, "provider_token_revocation_unproven"),
            "cancel" => reason(&v, "queue_not_current"),
            "check_source" => assert_eq!(v["evidence"]["capture_sources_trusted"], false),
            "membership_source" => reason(&v, "latest_membership_not_trusted"),
            "membership_configuration" => reason(&v, "latest_membership_changed"),
            _ => reason(&v, "policy_capture_not_fresh_after_checks"),
        }
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated v29 migration preserves receipts/pins without inventing submitting credentials"]
async fn current_host_policy_migration_keeps_history_but_does_not_infer_report_trust() {
    use sha2::{Digest, Sha256};
    let mut c = Candidate::new_with_configuration(1, true).await;
    let released = vec![c.release(0, 0, 2, cp::Outcome::Observed).await];
    let checks = c.capture("upgrade-checks", c.snapshot(&released)).await;
    let r = c.host_report().await;
    let host = c.save_host("upgrade-host", &r).await;
    assert_eq!(c.with_host(checks, host).await["admission_verified"], true);
    let raw = connection(&c.f.p.f.schema).await;
    crate::common::drop_unpopulated_v32(&raw).await;
    raw.batch_execute("DROP TABLE th_github_check_dispatch_policy; DROP INDEX th_github_managed_check_collections")
        .await
        .unwrap();
    raw.batch_execute(
        "ALTER TABLE th_github_queue_reports DROP COLUMN reporter, DROP COLUMN token",
    )
    .await
    .unwrap();
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for p in paths.iter().take(29) {
        digest = Sha256::digest([digest, std::fs::read(p).unwrap()].concat()).to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=29,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    let query = "SELECT jsonb_build_object('reports',(SELECT jsonb_agg(report ORDER BY sequence) FROM th_github_queue_reports),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o),'outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM th_outbox o),'pins',(SELECT jsonb_agg(to_jsonb(p)) FROM th_github_queue_admission p))";
    let before: Value = raw.query_one(query, &[]).await.unwrap().get(0);
    c.f.p.f.db = Metadata::new(connection(&c.f.p.f.schema).await)
        .await
        .unwrap();
    c.f.p.f.db.migrate().await.unwrap();
    c.f.p.f.db.migrate().await.unwrap();
    assert_eq!(
        before,
        raw.query_one(query, &[]).await.unwrap().get::<_, Value>(0)
    );
    let v = c.with_host(checks, host).await;
    reason(&v, "capture_submitter_not_trusted");
    assert_eq!(v["admission_verified"], false);
    assert_eq!(c.save_host("upgrade-host", &r).await, host);
    let fresh = c.host_report().await;
    let seq = c.save_host("upgrade-fresh-host", &fresh).await;
    let still_old = c.with_host(checks, seq).await;
    assert_eq!(still_old["evidence"]["capture_sources_trusted"], false);
    assert_eq!(still_old["admission_verified"], false);
    cleanup(&c.f.p.f.schema).await;
}
