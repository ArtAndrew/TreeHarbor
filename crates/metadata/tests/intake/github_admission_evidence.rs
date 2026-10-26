//! Real PostgreSQL aggregation boundaries. Source, host and runtime facts below
//! are trusted fixtures, not actual provider calls or sandbox execution.
use super::*;
#[path = "github_current_host_policy.rs"]
mod current_host_policy;
#[path = "github_check_dispatch_policy.rs"]
mod dispatch_policy;
use treeharbor_github_contract::{group_checks as gc, publication as published};
use treeharbor_metadata::github::{InspectionSpec, ManagedPullCheckSpec};

struct Candidate {
    f: QueueFixture,
    owner: Authorization,
    validator: Authorization,
    reader: Authorization,
    publisher: Authorization,
    bridge: Authorization,
    policy: AdmissionPolicy,
    import: Uuid,
    attempt: Uuid,
    membership: QueueObservation,
    programs: Vec<CheckProgram>,
}
struct Released {
    check: Uuid,
    publication: Uuid,
    target: cp::Target,
    host: cp::PublishedCheck,
}
impl Candidate {
    async fn new(count: usize) -> Self {
        Self::new_with_configuration(count, false).await
    }
    async fn new_with_configuration(count: usize, full: bool) -> Self {
        let mut f = QueueFixture::with_merge_queue(true).await;
        let (owner, validator, import, program) =
            setup_source(&mut f.p, &format!("git:sha1:{}", "aa".repeat(20))).await;
        let programs: Vec<_> = (0..count)
            .map(|n| {
                let mut p = program.clone();
                p.argv.push(format!("program-{n}"));
                p
            })
            .collect();
        let mut policy = AdmissionPolicy {
            version: 1,
            host_configuration: None,
            rules: rules(),
            managed: programs
                .iter()
                .map(|program| ManagedRequirement {
                    program: program.clone(),
                    validators: ["ci".into()].into_iter().collect(),
                })
                .collect(),
            workflows: vec![],
        };
        if full {
            policy = current_host_policy::policy(&f, &policy);
        }
        let adopted = set(&mut f.p, &owner, 0, Some(&policy)).await;
        let required: Vec<_> = policy
            .required(7, 1, f.p.binding, "main", 1)
            .unwrap()
            .into_iter()
            .collect();
        let raw = connection(&f.p.f.schema).await;
        let pull: Uuid = raw
            .query_one(
                "SELECT pull FROM th_github_inspections WHERE inspection=$1",
                &[&f.inspection],
            )
            .await
            .unwrap()
            .get(0);
        let inspection = Uuid::new_v4();
        let v =
            f.p.f
                .db
                .request_github_inspection(
                    &f.p.owner,
                    &cmd("aggregate-inspection"),
                    inspection,
                    &InspectionSpec {
                        pull: pull.to_string(),
                        required: required.clone(),
                        ttl_seconds: 900,
                    },
                )
                .await
                .unwrap();
        let mut observation = inspections::report(&f.p, &v).await;
        let template = observation.checks[0].clone();
        observation.checks = required
            .iter()
            .enumerate()
            .map(|(n, required)| {
                let mut check = template.clone();
                check.required = required.clone();
                check.runs[0].id += n as u64;
                check.runs[0].name = required.name.clone();
                check.runs[0].app_id = required.app_id;
                check
            })
            .collect();
        observation.rules_before = policy.rules.clone();
        observation.rules_after = policy.rules.clone();
        f.p.f
            .db
            .report_github_inspection(
                &f.p.reader,
                &cmd("aggregate-inspection-observed"),
                inspection,
                &observation,
            )
            .await
            .unwrap();
        f.inspection = inspection;
        f.spec.inspection = inspection.to_string();
        f.spec.admission = Some(pin(&adopted));
        f.target = QueueTarget {
            pull: serde_json::from_value(v["target"].clone()).unwrap(),
            expected_base_revision: observation.before.base_revision,
            expected_rules: observation.rules_before,
        };
        let reader = read(&mut f.p).await;
        let (publisher, bridge) = actors(&mut f.p).await;
        f.request().await;
        let attempt = f.begin().await;
        let membership = groups::group_report(&f).await;
        f.p.f
            .db
            .report_github_queue(
                &f.bridge,
                &cmd("aggregate-membership"),
                f.id,
                attempt,
                &membership,
            )
            .await
            .unwrap();
        Self {
            f,
            owner,
            validator,
            reader,
            publisher,
            bridge,
            policy,
            import,
            attempt,
            membership,
            programs,
        }
    }
    async fn execute(&mut self, id: Uuid, label: &str, exit: i64) {
        self.f
            .p
            .f
            .db
            .prepare_github_managed_check(&self.validator, &cmd(&format!("{label}-prepare")), id)
            .await
            .unwrap();
        let name = format!("{label}-node");
        let node = actor(&mut self.f.p.f, &name, K::Node, &[P::RuntimeControl]).await;
        let storage = Uuid::new_v4();
        let slot = StorageSlot {
            storage,
            slot: Uuid::new_v4(),
        };
        self.f
            .p
            .f
            .db
            .register_node_storage(self.f.p.f.scope.tenant(), &name, storage, "operator")
            .await
            .unwrap();
        self.f
            .p
            .f
            .db
            .register_runtime_volume(
                self.f.p.f.scope.tenant(),
                &name,
                &RuntimeVolume {
                    slot,
                    digest: [71; 32],
                    limit_bytes: 134217728,
                },
                "operator",
            )
            .await
            .unwrap();
        let job = self
            .f
            .p
            .f
            .db
            .claim_preparation(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
        let prep = self
            .f
            .p
            .f
            .db
            .reserve_preparation(&node, &job, slot)
            .await
            .unwrap();
        self.f
            .p
            .f
            .db
            .complete_preparation(
                &node,
                &prep,
                &PreparedFiles {
                    snapshot: prep.snapshot(),
                    profile: SourceProfile::full().id(),
                    manifest_digest: [91; 32],
                },
            )
            .await
            .unwrap();
        self.f
            .p
            .f
            .db
            .start_github_managed_check(&self.validator, &cmd(&format!("{label}-start")), id)
            .await
            .unwrap();
        let plan = self
            .f
            .p
            .f
            .db
            .claim_run(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
        self.f.p.f.db.admit_run_start(&node, &plan).await.unwrap();
        // Trusted terminal-state fixture only; no real process was executed.
        connection(&self.f.p.f.schema).await.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=$3,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1",&[&plan.run(),&"a".repeat(64),&exit]).await.unwrap();
        self.f.p.f.db.finish_run(&node, &plan).await.unwrap();
        assert_eq!(
            self.f
                .p
                .f
                .db
                .collect_github_managed_check(
                    &self.validator,
                    &cmd(&format!("{label}-collect")),
                    id
                )
                .await
                .unwrap()["outcome"],
            if exit == 0 { "passed" } else { "failed" }
        );
    }
    async fn checked(&mut self, n: usize, exit: i64) -> Uuid {
        let id = Uuid::new_v4();
        self.f
            .p
            .f
            .db
            .approve_github_managed_check(
                &self.owner,
                &cmd(&format!("aggregate-approve-{n}")),
                id,
                &ManagedCheckSpec {
                    queue: self.f.id,
                    membership_sequence: 1,
                    import: self.import,
                    validator: "ci".into(),
                    program: self.programs[n].clone(),
                },
            )
            .await
            .unwrap();
        self.execute(id, &format!("aggregate-{n}"), exit).await;
        id
    }
    async fn release(
        &mut self,
        n: usize,
        exit: i64,
        receipt_version: u8,
        outcome: cp::Outcome,
    ) -> Released {
        let id = self.checked(n, exit).await;
        self.publish(id, &format!("aggregate-{n}"), receipt_version, outcome)
            .await
    }
    async fn publish(
        &mut self,
        id: Uuid,
        label: &str,
        version: u8,
        outcome: cp::Outcome,
    ) -> Released {
        let publication = Uuid::new_v4();
        let v = self
            .f
            .p
            .f
            .db
            .request_github_check_publication(
                &self.publisher,
                &cmd(&format!("{label}-request")),
                publication,
                &CheckPublicationSpec {
                    check_run: id,
                    ttl_seconds: 900,
                },
            )
            .await
            .unwrap();
        let target: cp::Target = serde_json::from_value(v["target"].clone()).unwrap();
        let mut before = preflight(&self.f.p, &target).await;
        let required = self
            .policy
            .required(7, 1, self.f.p.binding, "main", 1)
            .unwrap()
            .into_iter()
            .collect::<Vec<_>>();
        before.policy_before.protection.app_bound_checks = required.clone();
        before.policy_after.protection.app_bound_checks = required;
        let attempt = Uuid::new_v4();
        self.f
            .p
            .f
            .db
            .begin_github_check_publication(
                &self.bridge,
                &cmd(&format!("{label}-begin")),
                publication,
                attempt,
            )
            .await
            .unwrap();
        if self.policy.host_configuration.is_some() && target.conclusion == cp::Conclusion::Success
        {
            let capture = self.host_report().await;
            let raw = capture
                .host_policy
                .as_ref()
                .unwrap()
                .before
                .protection
                .value
                .as_ref()
                .unwrap();
            use sha2::{Digest, Sha256};
            let hash = treeharbor_core::ObjectId::from_bytes(
                Sha256::digest(serde_json::to_vec(raw).unwrap()).into(),
            )
            .to_string();
            before.policy_before.protection.sha256 = hash.clone();
            before.policy_after.protection.sha256 = hash;
            self.save_host(&format!("{label}-dispatch-host"), &capture)
                .await;
        }
        self.f
            .p
            .f
            .db
            .dispatch_github_check_publication(
                &self.bridge,
                &cmd(&format!("{label}-dispatch")),
                publication,
                attempt,
                &before,
            )
            .await
            .unwrap();
        let mut report_value = json!(report(&self.f.p, &target, Some(before), outcome).await);
        report_value["version"] = json!(version);
        let mut host_value = json!({"id":9007199254741000u64+id.as_u128() as u64%1000,"app_id":target.app_id,"name":target.name().unwrap(),"head_revision":target.source.head().unwrap(),"external_id":target.external_id().unwrap(),"status":"completed","conclusion":target.conclusion});
        if version == 2 {
            host_value["node_id"] = json!(format!("CR_opaque_{publication}"));
        }
        let host: cp::PublishedCheck = serde_json::from_value(host_value.clone()).unwrap();
        if outcome == cp::Outcome::Observed {
            report_value["check"] = host_value;
        }
        let observation: cp::Report = serde_json::from_value(report_value).unwrap();
        self.f
            .p
            .f
            .db
            .report_github_check_publication(
                &self.bridge,
                &cmd(&format!("{label}-receipt")),
                publication,
                attempt,
                &observation,
            )
            .await
            .unwrap();
        Released {
            check: id,
            publication,
            target,
            host,
        }
    }
    fn snapshot(&self, released: &[Released]) -> gc::GroupChecks {
        let subject = self
            .membership
            .group_capture
            .as_ref()
            .unwrap()
            .after
            .members[0]
            .candidate
            .as_ref()
            .unwrap()
            .commit
            .clone();
        let suites = released
            .iter()
            .enumerate()
            .map(|(n, r)| {
                let value = json!(r.host);
                gc::Suite {
                    id: format!("CS_aggregate_{n}"),
                    repository_id: self.f.target.pull.repository_id,
                    head_revision: subject.revision.clone(),
                    app_id: Some(7),
                    workflow: None,
                    runs: vec![gc::Run {
                        id: value["node_id"]
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| format!("CR_unlinked_{n}")),
                        name: r.target.name().unwrap(),
                        status: "COMPLETED".into(),
                        conclusion: Some(
                            if r.target.conclusion == cp::Conclusion::Success {
                                "SUCCESS"
                            } else {
                                "FAILURE"
                            }
                            .into(),
                        ),
                        started_at_unix_ms: None,
                        completed_at_unix_ms: None,
                    }],
                }
            })
            .collect();
        let snapshot = gc::Snapshot { suites };
        gc::GroupChecks {
            subject,
            before: snapshot.clone(),
            after: snapshot,
        }
    }
    async fn capture(&mut self, label: &str, checks: gc::GroupChecks) -> i64 {
        let mut observed = self.f.report().await;
        observed.version = 5;
        if self.policy.version == 2 {
            observed.token_revoked = true;
        }
        observed.group_checks = Some(gc::GroupCheckReport {
            target: gc::GroupCheckTarget {
                membership_sequence: 1,
                side: gc::Side::After,
                subject: checks.subject.clone(),
            },
            checks,
        });
        self.f
            .p
            .f
            .db
            .report_github_queue(
                &self.f.bridge,
                &cmd(label),
                self.f.id,
                self.attempt,
                &observed,
            )
            .await
            .unwrap()["report_sequence"]
            .as_i64()
            .unwrap()
    }
    async fn evaluate(&mut self, sequence: i64, publication: Option<i64>) -> Value {
        self.f
            .p
            .f
            .db
            .github_queue_admission_evidence(&self.reader, 1, self.f.id, sequence, publication)
            .await
            .unwrap()
    }
    async fn final_publication(&mut self, label: &str, membership_sequence: i64) -> i64 {
        let selected = self
            .f
            .p
            .f
            .db
            .github_queue_publication_context(&self.f.bridge, 1, self.f.id, membership_sequence)
            .await
            .unwrap();
        let target: published::Target =
            serde_json::from_value(selected["publication_target"].clone()).unwrap();
        let candidate = target.candidate().unwrap().clone();
        let mut pull = self.membership.snapshot.as_ref().unwrap().pull.clone();
        pull.merged = true;
        pull.state = "closed".into();
        let snapshot = published::Snapshot {
            pull,
            main: candidate.commit.clone(),
            final_commit: Some(candidate.clone()),
        };
        let mut observed = self.f.report().await;
        observed.version = 8;
        observed.publication = Some(published::Report {
            target,
            before: snapshot.clone(),
            after: snapshot,
            comparison: published::Comparison {
                base: candidate.commit.clone(),
                head_revision: candidate.commit.revision.clone(),
                merge_base: candidate.commit,
                status: "identical".into(),
                ahead_by: 0,
                behind_by: 0,
                total_commits: 0,
            },
        });
        self.f
            .p
            .f
            .db
            .report_github_queue(
                &self.f.bridge,
                &cmd(label),
                self.f.id,
                self.attempt,
                &observed,
            )
            .await
            .unwrap()["report_sequence"]
            .as_i64()
            .unwrap()
    }
}
fn incomplete(value: &Value) {
    assert_eq!(value["evidence"]["complete"], false);
    assert_eq!(value["checks_verified"], false);
    for field in [
        "admission_verified",
        "merge_verified",
        "host_policy_verified",
    ] {
        assert_eq!(value[field], false);
    }
}

#[test]
fn queue_admission_evidence_wire_is_read_only_and_rejects_asserted_verification() {
    let request = json!({"action":"queue_admission_evidence","epoch":1,"queue":Uuid::new_v4(),"check_sequence":2,"publication_sequence":null});
    assert_eq!(
        serde_json::from_value::<WireCommand>(request.clone())
            .unwrap()
            .permission(),
        P::Read
    );
    for field in [
        "key",
        "checks_verified",
        "complete",
        "policy",
        "candidate",
        "requirements",
        "admission_verified",
    ] {
        let mut bad = request.clone();
        bad[field] = json!(true);
        assert!(
            serde_json::from_value::<WireCommand>(bad).is_err(),
            "{field}"
        );
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; complete exact managed set, read purity, restart, immutable digest and final candidate linkage"]
async fn admission_evidence_requires_every_program_and_is_pure_and_replayable() {
    for second in [None, Some(1), Some(0)] {
        let mut c = Candidate::new(2).await;
        let mut released = vec![c.release(0, 0, 2, cp::Outcome::Observed).await];
        if let Some(exit) = second {
            released.push(c.release(1, exit, 2, cp::Outcome::Observed).await);
        }
        let sequence = c
            .capture("complete-set-capture", c.snapshot(&released))
            .await;
        let publication = c.final_publication("complete-set-publication", 1).await;
        let raw = connection(&c.f.p.f.schema).await;
        let sql = "SELECT jsonb_build_object('operations',(SELECT count(*) FROM th_operations),'outbox',(SELECT count(*) FROM th_outbox),'queue_reports',(SELECT count(*) FROM th_github_queue_reports),'checks',(SELECT count(*) FROM th_github_managed_checks),'publications',(SELECT count(*) FROM th_github_check_publications),'pins',(SELECT count(*) FROM th_object_pins))";
        let before: Value = raw.query_one(sql, &[]).await.unwrap().get(0);
        let result = c.evaluate(sequence, Some(publication)).await;
        assert_eq!(
            result["evidence"]["requirements"].as_array().unwrap().len(),
            2
        );
        assert_eq!(result["evidence"]["complete"], second == Some(0));
        assert_eq!(result["checks_verified"], second == Some(0));
        assert_eq!(
            result["evidence"]["publication"]["exact_candidate_published"],
            true
        );
        assert_eq!(
            result["evidence"]["publication"]["checked_candidate_published"],
            second == Some(0)
        );
        for released in &released {
            if released.target.conclusion == cp::Conclusion::Success {
                let requirement = result["evidence"]["requirements"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|r| r["required"]["name"] == released.target.name().unwrap())
                    .unwrap();
                assert_eq!(requirement["check_run"], released.check.to_string());
                assert_eq!(requirement["publication"], released.publication.to_string());
                assert_eq!(requirement["host_check"]["id"], released.host.id);
            }
        }
        assert!(
            result["evidence_digest"]
                .as_str()
                .unwrap()
                .parse::<ObjectId>()
                .is_ok()
        );
        for field in [
            "admission_verified",
            "merge_verified",
            "host_policy_verified",
        ] {
            assert_eq!(result[field], false);
        }
        if second != Some(0) {
            incomplete(&result);
        }
        c.f.p.f.db = Metadata::new(connection(&c.f.p.f.schema).await)
            .await
            .unwrap();
        assert_eq!(result, c.evaluate(sequence, Some(publication)).await);
        assert_eq!(
            before,
            raw.query_one(sql, &[]).await.unwrap().get::<_, Value>(0)
        );
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; opaque host node linkage, legacy/unknown receipts, ambiguity and observation drift fail closed"]
async fn admission_evidence_never_borrows_same_app_name_or_incomplete_observations() {
    for case in [
        "wrong-node",
        "legacy-receipt",
        "unknown-receipt",
        "ambiguous",
        "drift",
        "failed-host",
    ] {
        let mut c = Candidate::new(1).await;
        let r = c
            .release(
                0,
                0,
                if case == "legacy-receipt" { 1 } else { 2 },
                if case == "unknown-receipt" {
                    cp::Outcome::Unknown
                } else {
                    cp::Outcome::Observed
                },
            )
            .await;
        let mut checks = c.snapshot(&[r]);
        match case {
            "wrong-node" => {
                for side in [&mut checks.before, &mut checks.after] {
                    side.suites[0].runs[0].id = "CR_other_opaque".into();
                }
            }
            "ambiguous" => {
                for side in [&mut checks.before, &mut checks.after] {
                    let mut other = side.suites[0].runs[0].clone();
                    other.id = "CR_other_opaque".into();
                    side.suites[0].runs.push(other);
                }
            }
            "drift" => checks.after.suites[0].runs[0].conclusion = Some("FAILURE".into()),
            "failed-host" => {
                for side in [&mut checks.before, &mut checks.after] {
                    side.suites[0].runs[0].conclusion = Some("FAILURE".into());
                }
            }
            _ => {}
        }
        let sequence = c.capture(&format!("negative-{case}"), checks).await;
        let value = c.evaluate(sequence, None).await;
        incomplete(&value);
        assert!(
            !value["evidence"]["requirements"][0]["reasons"]
                .as_array()
                .unwrap()
                .is_empty()
                || !value["evidence"]["reasons"].as_array().unwrap().is_empty(),
            "{case}"
        );
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; policy/node/source fences change current eligibility without erasing exact historical managed proof"]
async fn admission_evidence_preserves_history_after_policy_node_or_source_changes() {
    for fence in [
        "policy",
        "policy-aba",
        "node",
        "source",
        "publication-token",
        "publication-grant",
        "bridge-grant",
    ] {
        let mut c = Candidate::new(1).await;
        let r = c.release(0, 0, 2, cp::Outcome::Observed).await;
        let sequence = c.capture("historical-capture", c.snapshot(&[r])).await;
        let before = c.evaluate(sequence, None).await;
        assert_eq!(before["checks_verified"], true);
        match fence {
            "policy" => {
                set(&mut c.f.p, &c.owner, 1, None).await;
            }
            "policy-aba" => {
                set(&mut c.f.p, &c.owner, 1, None).await;
                set(&mut c.f.p, &c.owner, 2, Some(&c.policy)).await;
            }
            "node" => {
                c.f.p
                    .f
                    .db
                    .set_repository_access(&c.f.p.f.scope, "aggregate-0-node", &[], "operator")
                    .await
                    .unwrap()
            }
            "source" => c.f.p.new_version().await,
            "publication-token" => {
                let raw = connection(&c.f.p.f.schema).await;
                let token: Uuid = raw
                    .query_one("SELECT token FROM th_github_check_publications", &[])
                    .await
                    .unwrap()
                    .get(0);
                c.f.p
                    .f
                    .db
                    .revoke_repository_token(&c.f.p.f.scope, token, "operator")
                    .await
                    .unwrap();
            }
            "publication-grant" => {
                c.f.p
                    .f
                    .db
                    .set_repository_access(&c.f.p.f.scope, "release-owner", &[], "operator")
                    .await
                    .unwrap()
            }
            "bridge-grant" => {
                c.f.p
                    .f
                    .db
                    .set_repository_access(&c.f.p.f.scope, "bridge", &[], "operator")
                    .await
                    .unwrap()
            }
            _ => unreachable!(),
        }
        let after = c.evaluate(sequence, None).await;
        assert_eq!(after["evidence"], before["evidence"], "{fence}");
        assert_eq!(
            after["evidence_digest"], before["evidence_digest"],
            "{fence}"
        );
        assert_eq!(after["evidence"]["complete"], true, "{fence}");
        assert_eq!(after["current_locally"], false, "{fence}");
        assert_eq!(after["checks_verified"], false, "{fence}");
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; PR success cannot cover its group's exact combined candidate"]
async fn admission_evidence_rejects_a_pr_receipt_for_a_group_candidate() {
    let mut c = Candidate::new(1).await;
    let raw = connection(&c.f.p.f.schema).await;
    let pull: Uuid = raw
        .query_one(
            "SELECT pull FROM th_github_inspections WHERE inspection=$1",
            &[&c.f.inspection],
        )
        .await
        .unwrap()
        .get(0);
    let import = import_source(&mut c.f.p, &format!("git:sha1:{}", "11".repeat(20))).await;
    let id = Uuid::new_v4();
    c.f.p
        .f
        .db
        .approve_github_managed_pull_check(
            &c.owner,
            &cmd("pr-only-approve"),
            id,
            &ManagedPullCheckSpec {
                pull,
                import,
                validator: "ci".into(),
                program: c.programs[0].clone(),
            },
        )
        .await
        .unwrap();
    c.execute(id, "pr-only", 0).await;
    let r = c.publish(id, "pr-only", 2, cp::Outcome::Observed).await;
    let sequence = c.capture("pr-only-capture", c.snapshot(&[r])).await;
    incomplete(&c.evaluate(sequence, None).await);
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; identical candidate bytes in another membership cannot inherit the selected publication linkage"]
async fn admission_evidence_final_publication_requires_its_exact_membership() {
    let mut c = Candidate::new(1).await;
    let r = c.release(0, 0, 2, cp::Outcome::Observed).await;
    let sequence = c.capture("publication-checks", c.snapshot(&[r])).await;
    let other =
        c.f.p
            .f
            .db
            .report_github_queue(
                &c.f.bridge,
                &cmd("other-membership"),
                c.f.id,
                c.attempt,
                &c.membership,
            )
            .await
            .unwrap()["report_sequence"]
            .as_i64()
            .unwrap();
    let publication = c
        .final_publication("other-membership-publication", other)
        .await;
    let result = c.evaluate(sequence, Some(publication)).await;
    assert_eq!(result["evidence"]["complete"], true);
    assert_eq!(
        result["evidence"]["publication"]["exact_candidate_published"],
        false
    );
    assert_eq!(
        result["evidence"]["publication"]["checked_candidate_published"],
        false
    );
    assert_eq!(result["merge_verified"], false);
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; scope/permission/epoch/source sequence checks use actual scoped credentials"]
async fn admission_evidence_obeys_tenant_authorization_and_sequence_boundaries() {
    let mut c = Candidate::new(1).await;
    let r = c.release(0, 0, 2, cp::Outcome::Observed).await;
    let sequence = c.capture("auth-capture", c.snapshot(&[r])).await;
    assert!(matches!(
        c.f.p
            .f
            .db
            .github_queue_admission_evidence(&c.owner, 1, c.f.id, sequence, None)
            .await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        c.f.p
            .f
            .db
            .github_queue_admission_evidence(&c.reader, 2, c.f.id, sequence, None)
            .await,
        Err(MetaError::Fenced)
    ));
    assert!(
        c.f.p
            .f
            .db
            .github_queue_admission_evidence(&c.reader, 1, Uuid::new_v4(), sequence, None)
            .await
            .is_err()
    );
    for bad in [0, 1, -1, 4097] {
        assert!(
            c.f.p
                .f
                .db
                .github_queue_admission_evidence(&c.reader, 1, c.f.id, bad, None)
                .await
                .is_err(),
            "{bad}"
        );
    }
    assert!(
        c.f.p
            .f
            .db
            .github_queue_admission_evidence(&c.reader, 1, c.f.id, sequence, Some(sequence))
            .await
            .is_err()
    );
    let foreign = Scope::new("aggregate-foreign", c.f.p.f.scope.repository()).unwrap();
    c.f.p
        .f
        .db
        .create_repository(&foreign, Authority::Connected)
        .await
        .unwrap();
    let reader = scoped_actor(&mut c.f.p.f.db, &foreign, "foreign-reader", P::Read).await;
    assert!(matches!(
        c.f.p
            .f
            .db
            .github_queue_admission_evidence(&reader, 1, c.f.id, sequence, None)
            .await,
        Err(MetaError::NotFound)
    ));
    c.f.p
        .f
        .db
        .set_repository_access(&c.f.p.f.scope, "policy-owner", &[], "operator")
        .await
        .unwrap();
    assert!(
        c.f.p
            .f
            .db
            .github_queue_admission_evidence(&c.reader, 1, c.f.id, sequence, None)
            .await
            .is_err()
    );
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; cancellation preserves late exact publication evidence while fencing current use"]
async fn admission_evidence_cancelled_unknown_publication_recovers_history_without_current_approval()
 {
    let mut c = Candidate::new(1).await;
    let released = c.release(0, 0, 2, cp::Outcome::Unknown).await;
    c.f.p
        .f
        .db
        .cancel_github_check_publication(
            &c.publisher,
            &cmd("cancel-unknown-publication"),
            released.publication,
        )
        .await
        .unwrap();
    let raw = connection(&c.f.p.f.schema).await;
    let row = raw
        .query_one(
            "SELECT preflight,attempt FROM th_github_check_publications WHERE publication=$1",
            &[&released.publication],
        )
        .await
        .unwrap();
    let preflight: cp::Preflight = serde_json::from_value(row.get::<_, Value>(0)).unwrap();
    let attempt: Uuid = row.get(1);
    let mut observed = json!(
        report(
            &c.f.p,
            &released.target,
            Some(preflight),
            cp::Outcome::Observed
        )
        .await
    );
    observed["version"] = json!(2);
    observed["check"] = json!(released.host);
    let observed: cp::Report = serde_json::from_value(observed).unwrap();
    c.f.p
        .f
        .db
        .report_github_check_publication(
            &c.bridge,
            &cmd("cancelled-late-observed"),
            released.publication,
            attempt,
            &observed,
        )
        .await
        .unwrap();
    let sequence = c
        .capture("cancelled-exact-capture", c.snapshot(&[released]))
        .await;
    let value = c.evaluate(sequence, None).await;
    assert_eq!(value["evidence"]["complete"], true);
    assert_eq!(value["current_locally"], false);
    assert_eq!(value["checks_verified"], false);
    assert_eq!(value, c.evaluate(sequence, None).await);
    cleanup(&c.f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; a later capture or pending same-context check fences current use but keeps prior exact proof"]
async fn admission_evidence_current_use_requires_latest_capture_and_no_pending_context() {
    for fence in ["later-capture", "pending-context"] {
        let mut c = Candidate::new(1).await;
        let r = c.release(0, 0, 2, cp::Outcome::Observed).await;
        let checks = c.snapshot(&[r]);
        let sequence = c.capture("selected-current-capture", checks.clone()).await;
        let before = c.evaluate(sequence, None).await;
        assert_eq!(before["checks_verified"], true);
        if fence == "later-capture" {
            let mut later = checks;
            for side in [&mut later.before, &mut later.after] {
                side.suites[0].runs[0].conclusion = Some("FAILURE".into());
            }
            let newest = c.capture("later-failed-capture", later).await;
            incomplete(&c.evaluate(newest, None).await);
        } else {
            let id = Uuid::new_v4();
            c.f.p
                .f
                .db
                .approve_github_managed_check(
                    &c.owner,
                    &cmd("pending-approve"),
                    id,
                    &ManagedCheckSpec {
                        queue: c.f.id,
                        membership_sequence: 1,
                        import: c.import,
                        validator: "ci".into(),
                        program: c.programs[0].clone(),
                    },
                )
                .await
                .unwrap();
            c.execute(id, "pending-context", 0).await;
            c.f.p
                .f
                .db
                .request_github_check_publication(
                    &c.publisher,
                    &cmd("pending-context-request"),
                    Uuid::new_v4(),
                    &CheckPublicationSpec {
                        check_run: id,
                        ttl_seconds: 900,
                    },
                )
                .await
                .unwrap();
        }
        let after = c.evaluate(sequence, None).await;
        assert_eq!(after["evidence"], before["evidence"], "{fence}");
        assert_eq!(
            after["evidence_digest"], before["evidence_digest"],
            "{fence}"
        );
        assert_eq!(after["current_locally"], false, "{fence}");
        assert_eq!(after["checks_verified"], false, "{fence}");
        cleanup(&c.f.p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; linked successful workflow and matching root bytes never prove external execution context"]
async fn admission_evidence_workflow_root_and_attempt_cannot_assert_runtime_context() {
    let mut f = QueueFixture::with_merge_queue(true).await;
    let owner = actor(
        &mut f.p.f,
        "policy-owner",
        K::Human,
        &[P::PolicyWrite, P::Read],
    )
    .await;
    let policy = workflow(&f);
    let adopted = set(&mut f.p, &owner, 0, Some(&policy)).await;
    f.spec.admission = Some(pin(&adopted));
    let reader = read(&mut f.p).await;
    f.request().await;
    let attempt = f.begin().await;
    let membership = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(
            &f.bridge,
            &cmd("context-membership"),
            f.id,
            attempt,
            &membership,
        )
        .await
        .unwrap();
    let checks = group_checks::report(&f, &membership).await;
    let sequence =
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("context-checks"), f.id, attempt, &checks)
            .await
            .unwrap()["report_sequence"]
            .as_i64()
            .unwrap();
    let selected_attempt = group_attempts::report(&f, &checks).await;
    f.p.f
        .db
        .report_github_queue(
            &f.bridge,
            &cmd("context-attempt"),
            f.id,
            attempt,
            &selected_attempt,
        )
        .await
        .unwrap();
    let selected =
        f.p.f
            .db
            .github_queue_definition_context(&f.bridge, 1, f.id, 3)
            .await
            .unwrap();
    let mut definition: QueueObservation = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/workflow-definition-v7.json"
    ))
    .unwrap();
    definition.started_at_unix_ms = f.report().await.started_at_unix_ms;
    definition.finished_at_unix_ms = definition.started_at_unix_ms;
    definition.workflow_definition.as_mut().unwrap().target =
        serde_json::from_value(selected["workflow_definition_target"].clone()).unwrap();
    f.p.f
        .db
        .report_github_queue(
            &f.bridge,
            &cmd("context-definition"),
            f.id,
            attempt,
            &definition,
        )
        .await
        .unwrap();
    let file =
        f.p.f
            .db
            .github_queue_definitions(&reader, 1, f.id, 0)
            .await
            .unwrap();
    assert_eq!(
        file["captures"][0]["workflow_file_matches_pinned_policy"],
        true
    );
    for active in [true, false] {
        if !active {
            set(&mut f.p, &owner, 1, None).await;
        }
        let value =
            f.p.f
                .db
                .github_queue_admission_evidence(&reader, 1, f.id, sequence, None)
                .await
                .unwrap();
        incomplete(&value);
        assert_eq!(value["evidence"]["requirements"][0]["kind"], "workflow");
        assert!(
            value["evidence"]["requirements"][0]["reasons"]
                .as_array()
                .unwrap()
                .contains(&json!("workflow_context_unproven"))
        );
        if !active {
            assert_eq!(value["current_locally"], false);
        }
    }
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; legacy group captures remain unconfigured and never inherit a later admission policy"]
async fn admission_evidence_legacy_queue_does_not_inherit_later_adoption() {
    let mut f = QueueFixture::with_merge_queue(true).await;
    let owner = actor(
        &mut f.p.f,
        "policy-owner",
        K::Human,
        &[P::PolicyWrite, P::Read],
    )
    .await;
    let reader = read(&mut f.p).await;
    f.request().await;
    let attempt = f.begin().await;
    let membership = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(
            &f.bridge,
            &cmd("legacy-evidence-membership"),
            f.id,
            attempt,
            &membership,
        )
        .await
        .unwrap();
    let checks = group_checks::report(&f, &membership).await;
    let sequence =
        f.p.f
            .db
            .report_github_queue(
                &f.bridge,
                &cmd("legacy-evidence-checks"),
                f.id,
                attempt,
                &checks,
            )
            .await
            .unwrap()["report_sequence"]
            .as_i64()
            .unwrap();
    let before =
        f.p.f
            .db
            .github_queue_admission_evidence(&reader, 1, f.id, sequence, None)
            .await
            .unwrap();
    incomplete(&before);
    assert!(before["evidence"]["policy"].is_null());
    assert!(
        before["evidence"]["reasons"]
            .as_array()
            .unwrap()
            .contains(&json!("admission_policy_unconfigured"))
    );
    let policy = workflow(&f);
    set(&mut f.p, &owner, 0, Some(&policy)).await;
    let after =
        f.p.f
            .db
            .github_queue_admission_evidence(&reader, 1, f.id, sequence, None)
            .await
            .unwrap();
    assert_eq!(after["evidence"], before["evidence"]);
    assert_eq!(after["evidence_digest"], before["evidence_digest"]);
    assert_eq!(after["current_locally"], false);
    incomplete(&after);
    cleanup(&f.p.f.schema).await;
}
