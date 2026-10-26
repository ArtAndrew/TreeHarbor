//! Real PostgreSQL boundaries; host and terminal-runtime observations are fixtures.
#[path = "github_admission_evidence.rs"]
mod admission_evidence;
use super::*;
use treeharbor_metadata::github::{AdmissionPolicy, ManagedRequirement, PolicyPin};

fn rules() -> treeharbor_github_contract::RuleObservation {
    let v: Value = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/check-publication-v1.json"
    ))
    .unwrap();
    serde_json::from_value(v["report"]["preflight"]["policy_before"]["rules"].clone()).unwrap()
}
fn managed(program: CheckProgram) -> AdmissionPolicy {
    AdmissionPolicy {
        version: 1,
        rules: rules(),
        managed: vec![ManagedRequirement {
            program,
            validators: ["ci".into()].into_iter().collect(),
        }],
        workflows: vec![],
    }
}
fn workflow(f: &QueueFixture) -> AdmissionPolicy {
    let p: treeharbor_github_contract::workflow_definitions::Policy = serde_json::from_str(
        include_str!("../../../github-contract/tests/fixtures/workflow-policy-v1.json"),
    )
    .unwrap();
    AdmissionPolicy {
        version: 1,
        rules: f.target.expected_rules.clone(),
        managed: vec![],
        workflows: p.checks,
    }
}
async fn set(
    p: &mut Projection,
    owner: &Authorization,
    generation: i64,
    value: Option<&AdmissionPolicy>,
) -> Value {
    p.f.db
        .set_github_admission_policy(
            owner,
            &cmd(&format!("admission-policy-{generation}")),
            p.binding,
            "main",
            generation,
            value,
        )
        .await
        .unwrap()
}
fn pin(v: &Value) -> PolicyPin {
    PolicyPin {
        digest: v["policy"].as_str().unwrap().into(),
        generation: v["generation"].as_i64().unwrap(),
    }
}

#[test]
fn admission_policy_wire_is_owner_only_and_rejects_asserted_verification() {
    let v: Value = serde_json::from_str(include_str!(
        "../fixtures/connected-admission-policy-v1.json"
    ))
    .unwrap();
    let binding = Uuid::new_v4();
    let mut request = json!({"action":"admission_policy_set","key":"approval","epoch":1,"binding":binding,"base_ref":"main","expected_generation":0,"definition":v["policy"]});
    assert_eq!(
        serde_json::from_value::<WireCommand>(request.clone())
            .unwrap()
            .permission(),
        P::PolicyWrite
    );
    request["merge_verified"] = json!(true);
    assert!(serde_json::from_value::<WireCommand>(request).is_err());
    let preview = json!({"action":"admission_policy_plan","epoch":1,"binding":binding,"base_ref":"main","expected_generation":0,"definition":v["policy"]});
    assert_eq!(
        serde_json::from_value::<WireCommand>(preview.clone())
            .unwrap()
            .permission(),
        P::PolicyWrite
    );
    for field in ["key", "approved", "admission_verified", "required"] {
        let mut invalid = preview.clone();
        invalid[field] = json!(true);
        assert!(
            serde_json::from_value::<WireCommand>(invalid).is_err(),
            "{field}"
        );
    }
    let read = json!({"action":"admission_policy","epoch":1,"binding":binding,"base_ref":"main"});
    assert_eq!(
        serde_json::from_value::<WireCommand>(read)
            .unwrap()
            .permission(),
        P::Read
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL; branch adoption, exact required set, CAS/ABA/revocation, immutable scope and retained queue receipts"]
async fn admission_policy_requires_current_pin_and_never_reverts_to_legacy_after_revocation() {
    let mut f = QueueFixture::with_merge_queue(true).await;
    let p = workflow(&f);
    let policy_owner = actor(
        &mut f.p.f,
        "admission-owner",
        K::Human,
        &[P::PolicyWrite, P::Read],
    )
    .await;
    assert!(
        f.p.f
            .db
            .set_github_admission_policy(
                &f.bridge,
                &cmd("bridge-policy"),
                f.p.binding,
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .set_github_admission_policy(
                &f.owner,
                &cmd("unprivileged-policy"),
                f.p.binding,
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    let legacy = f.request().await;
    let adopted = set(&mut f.p, &policy_owner, 0, Some(&p)).await;
    assert!(
        f.p.f
            .db
            .begin_github_queue(&f.bridge, &cmd("legacy-fenced"), f.id, Uuid::new_v4())
            .await
            .is_err()
    );
    f.p.f
        .db
        .cancel_github_queue(&f.owner, &cmd("cancel-legacy"), f.id)
        .await
        .unwrap();
    let id = Uuid::new_v4();
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("omit-pin"), id, &f.spec)
            .await
            .is_err()
    );
    let mut wrong = f.spec.clone();
    wrong.admission = Some(PolicyPin {
        digest: ObjectId::from_bytes([7; 32]).to_string(),
        generation: 1,
    });
    assert!(
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("wrong-pin"), id, &wrong)
            .await
            .is_err()
    );
    f.spec.admission = Some(pin(&adopted));
    let queued =
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("pinned-queue"), id, &f.spec)
            .await
            .unwrap();
    let attempt = Uuid::new_v4();
    f.p.f
        .db
        .begin_github_queue(&f.bridge, &cmd("pinned-begin"), id, attempt)
        .await
        .unwrap();
    // A settled upstream enqueue receipt is required before a removal may be
    // dispatched; policy revocation must not bypass that independent fence.
    let mut enqueued = f.report().await;
    enqueued.version = 3;
    enqueued.enqueue_receipt = Some(EnqueueReceipt {
        request_id: attempt.to_string(),
        repository_id: f.target.pull.repository_id,
        pull_node: f.target.pull.pull_node.clone(),
        pull_number: f.target.pull.pull_number,
        queue_id: "MQ_admission_fixture".into(),
        entry_id: "MQE_admission_fixture".into(),
        head_revision: f.target.pull.head_revision.clone(),
    });
    f.p.f
        .db
        .report_github_queue(&f.bridge, &cmd("pinned-enqueued"), id, attempt, &enqueued)
        .await
        .unwrap();
    assert!(
        f.p.f
            .db
            .set_github_admission_policy(
                &policy_owner,
                &cmd("stale-cas"),
                f.p.binding,
                "main",
                0,
                Some(&p)
            )
            .await
            .is_err()
    );
    set(&mut f.p, &policy_owner, 1, None).await;
    assert!(
        f.p.f
            .db
            .github_queue_context(&f.bridge, 1, id, true, Some(attempt))
            .await
            .is_err()
    );
    set(&mut f.p, &policy_owner, 2, Some(&p)).await;
    assert!(
        f.p.f
            .db
            .github_queue_context(&f.bridge, 1, id, true, Some(attempt))
            .await
            .is_err()
    );
    assert_eq!(
        queued,
        f.p.f
            .db
            .request_github_queue(&f.owner, &cmd("pinned-queue"), id, &f.spec)
            .await
            .unwrap()
    );
    assert_ne!(legacy["queue"], queued["queue"]);
    let raw = connection(&f.p.f.schema).await;
    for sql in [
        "DELETE FROM th_github_admission_refs",
        "UPDATE th_github_admission_refs SET generation=generation",
        "UPDATE th_github_queue_admission SET generation=2",
        "DELETE FROM th_github_admission_policies",
    ] {
        assert!(raw.batch_execute(sql).await.is_err(), "{sql}");
    }
    set(&mut f.p, &policy_owner, 3, None).await;
    let removal = Uuid::new_v4();
    f.p.f
        .db
        .request_github_dequeue(
            &f.owner,
            &cmd("revoked-policy-remove"),
            removal,
            &treeharbor_metadata::github::DequeueSpec {
                queue: id.to_string(),
                ttl_seconds: 60,
                scope: "pull_request".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.p.f
            .db
            .begin_github_dequeue(
                &f.bridge,
                &cmd("revoked-policy-remove-begin"),
                removal,
                Uuid::new_v4()
            )
            .await
            .unwrap()["dispatch_granted"],
        true
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; policy adoption fences old PR approvals, missing validator/program and ABA starts"]
async fn admission_policy_pins_each_managed_pr_approval_before_execution() {
    let mut p = Projection::new().await;
    let (owner, validator, legacy, spec) = pull_checks::setup(&mut p).await;
    p.f.db
        .approve_github_managed_pull_check(&owner, &cmd("legacy"), legacy, &spec)
        .await
        .unwrap();
    let policy = managed(spec.program.clone());
    set(&mut p, &owner, 0, Some(&policy)).await;
    let reader = read(&mut p).await;
    assert_eq!(
        p.f.db
            .github_managed_check_status(&reader, 1, legacy)
            .await
            .unwrap()["current_locally"],
        false
    );
    assert!(
        p.f.db
            .prepare_github_managed_check(&validator, &cmd("old-prepare"), legacy)
            .await
            .is_err()
    );
    let mut bad = spec.clone();
    bad.program.argv.push("unapproved".into());
    assert!(
        p.f.db
            .approve_github_managed_pull_check(&owner, &cmd("bad-program"), Uuid::new_v4(), &bad)
            .await
            .is_err()
    );
    bad = spec.clone();
    bad.validator = "other-ci".into();
    assert!(
        p.f.db
            .approve_github_managed_pull_check(&owner, &cmd("bad-validator"), Uuid::new_v4(), &bad)
            .await
            .is_err()
    );
    let id = Uuid::new_v4();
    p.f.db
        .approve_github_managed_pull_check(&owner, &cmd("approved"), id, &spec)
        .await
        .unwrap();
    p.f.db
        .prepare_github_managed_check(&validator, &cmd("new-prepare"), id)
        .await
        .unwrap();
    set(&mut p, &owner, 1, None).await;
    set(&mut p, &owner, 2, Some(&policy)).await;
    assert!(
        p.f.db
            .start_github_managed_check(&validator, &cmd("old-start"), id)
            .await
            .is_err()
    );
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_admission", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert!(
        raw.batch_execute("DELETE FROM th_github_check_admission")
            .await
            .is_err()
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact policy rules/complete required set enforced at final dispatch and historical recovery survives revocation"]
async fn admission_policy_gates_irreversible_check_dispatch_and_preserves_late_receipts() {
    let mut p = Projection::new().await;
    let (policy_owner, validator, id, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program.clone());
    set(&mut p, &policy_owner, 0, Some(&policy)).await;
    p.f.db
        .approve_github_managed_pull_check(&policy_owner, &cmd("approve-policy-check"), id, &spec)
        .await
        .unwrap();
    complete(&mut p, &validator, id, 0).await;
    let collected =
        p.f.db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap();
    let (owner, bridge) = actors(&mut p).await;
    let publication = Uuid::new_v4();
    let target = request(&mut p, &owner, publication, id).await;
    let pref = preflight(&p, &target).await;
    let attempt = Uuid::new_v4();
    p.f.db
        .begin_github_check_publication(&bridge, &cmd("reserve"), publication, attempt)
        .await
        .unwrap();
    for drift in ["rules", "extra-required", "wrong-app"] {
        let mut changed = pref.clone();
        for v in [&mut changed.policy_before, &mut changed.policy_after] {
            match drift {
                "rules" => v.rules.sha256 = "cc".repeat(32),
                "extra-required" => v.protection.app_bound_checks.push(RequiredCheck {
                    app_id: 15368,
                    name: "unapproved".into(),
                }),
                _ => v.protection.app_bound_checks.push(RequiredCheck {
                    app_id: 999,
                    name: "other-app".into(),
                }),
            }
        }
        assert!(changed.eligible(&target));
        assert!(
            p.f.db
                .dispatch_github_check_publication(
                    &bridge,
                    &cmd(drift),
                    publication,
                    attempt,
                    &changed
                )
                .await
                .is_err()
        );
    }
    let valid =
        p.f.db
            .dispatch_github_check_publication(
                &bridge,
                &cmd("valid-dispatch"),
                publication,
                attempt,
                &pref,
            )
            .await
            .unwrap();
    assert_eq!(valid["dispatch_granted"], true);
    assert_eq!(
        p.f.db
            .dispatch_github_check_publication(
                &bridge,
                &cmd("valid-dispatch"),
                publication,
                attempt,
                &pref
            )
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    set(&mut p, &policy_owner, 1, None).await;
    assert_eq!(
        collected,
        p.f.db
            .collect_github_managed_check(&validator, &cmd("collect"), id)
            .await
            .unwrap()
    );
    let historical =
        p.f.db
            .collect_github_managed_check(&validator, &cmd("historical-collected"), id)
            .await
            .unwrap();
    assert_eq!(historical["current_locally"], false);
    assert_eq!(historical["evidence"], collected["evidence"]);
    let r = report(&p, &target, Some(pref), cp::Outcome::Observed).await;
    let receipt = p
        .f
        .db
        .report_github_check_publication(&bridge, &cmd("late-receipt"), publication, attempt, &r)
        .await
        .unwrap();
    assert_eq!(
        receipt,
        p.f.db
            .report_github_check_publication(
                &bridge,
                &cmd("late-receipt"),
                publication,
                attempt,
                &r
            )
            .await
            .unwrap()
    );
    let observed = status(&mut p, "release-owner", publication).await;
    assert_eq!(observed["state"], "observed");
    assert_eq!(observed["current_locally"], false);
    assert_eq!(observed["merge_verified"], false);
    cleanup(&p.f.schema).await;
}

async fn scoped_actor(
    db: &mut Metadata,
    scope: &Scope,
    name: &str,
    permission: P,
) -> Authorization {
    db.provision_principal(scope.tenant(), name, K::Human, "operator")
        .await
        .unwrap();
    db.set_repository_access(scope, name, &[permission], "operator")
        .await
        .unwrap();
    let token = db
        .issue_repository_token(scope, name, &[permission], 3600, "operator")
        .await
        .unwrap();
    db.authorize(&token.secret, scope, permission)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires PostgreSQL; actual tenant/repository/binding scope and authority epoch denial"]
async fn admission_policy_does_not_cross_scope_binding_or_epoch() {
    let mut p = Projection::new().await;
    let (owner, _, _, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program);
    let adopted = set(&mut p, &owner, 0, Some(&policy)).await;
    let reader = read(&mut p).await;
    assert_eq!(
        p.f.db
            .github_admission_policy(&reader, 1, p.binding, "main")
            .await
            .unwrap()["policy"],
        adopted["policy"]
    );
    for scope in [
        Scope::new("other-admission-tenant", p.f.scope.repository()).unwrap(),
        Scope::new(p.f.scope.tenant(), Uuid::new_v4()).unwrap(),
    ] {
        p.f.db
            .create_repository(&scope, Authority::Connected)
            .await
            .unwrap();
        let writer = scoped_actor(&mut p.f.db, &scope, "foreign-writer", P::PolicyWrite).await;
        let foreign_reader = scoped_actor(&mut p.f.db, &scope, "foreign-reader", P::Read).await;
        assert!(matches!(
            p.f.db
                .set_github_admission_policy(
                    &writer,
                    &cmd("foreign-policy"),
                    p.binding,
                    "main",
                    0,
                    Some(&policy)
                )
                .await,
            Err(MetaError::NotFound)
        ));
        assert!(matches!(
            p.f.db
                .github_admission_policy(&foreign_reader, 1, p.binding, "main")
                .await,
            Err(MetaError::NotFound)
        ));
    }
    assert!(
        p.f.db
            .github_admission_policy(&reader, 1, Uuid::new_v4(), "main")
            .await
            .is_err()
    );
    let wrong_epoch = Command {
        key: "wrong-policy-epoch".into(),
        epoch: 2,
    };
    assert!(matches!(
        p.f.db
            .set_github_admission_policy(&owner, &wrong_epoch, p.binding, "main", 1, Some(&policy))
            .await,
        Err(MetaError::Fenced)
    ));
    assert!(matches!(
        p.f.db
            .github_admission_policy(&reader, 2, p.binding, "main")
            .await,
        Err(MetaError::Fenced)
    ));
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_admission_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    p.f.db
        .revoke_github(&p.owner, &cmd("revoke-policy-binding"), p.binding)
        .await
        .unwrap();
    assert!(
        p.f.db
            .set_github_admission_policy(
                &owner,
                &cmd("revoked-policy-binding"),
                p.binding,
                "main",
                1,
                None
            )
            .await
            .is_err()
    );
    assert_eq!(
        raw.query_one("SELECT generation FROM th_github_admission_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; competing policy CAS transactions have exactly one winner"]
async fn admission_policy_concurrent_cas_cannot_overwrite_a_winner() {
    let mut p = Projection::new().await;
    let (owner, _, _, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program);
    set(&mut p, &owner, 0, Some(&policy)).await;
    let mut left = policy.clone();
    left.managed[0].program.argv.push("left".into());
    let mut right = policy.clone();
    right.managed[0].program.argv.push("right".into());
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let left_command = cmd("cas-left");
    let right_command = cmd("cas-right");
    let (a, b) = tokio::join!(
        p.f.db.set_github_admission_policy(
            &owner,
            &left_command,
            p.binding,
            "main",
            1,
            Some(&left)
        ),
        other.set_github_admission_policy(
            &owner,
            &right_command,
            p.binding,
            "main",
            1,
            Some(&right)
        )
    );
    assert_ne!(a.is_ok(), b.is_ok());
    let (winner, loser) = if a.is_ok() { (a, b) } else { (b, a) };
    let winner = winner.unwrap();
    assert!(matches!(loser, Err(MetaError::HostQueueState)));
    let reader = read(&mut p).await;
    let current =
        p.f.db
            .github_admission_policy(&reader, 1, p.binding, "main")
            .await
            .unwrap();
    assert_eq!(current["generation"], 2);
    assert_eq!(current["policy"], winner["policy"]);
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_admission_policies", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_operations WHERE kind='github.admission_policy.set.v1'",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        2
    );
    cleanup(&p.f.schema).await;
}

// Re-observe the same PR with the exact contexts reviewed for this policy.
// Both the host observation and earlier source conversion remain trusted fixtures.
async fn inspect_for_policy(f: &mut QueueFixture, policy: &AdmissionPolicy, generation: i64) {
    let binding: BindingSpec =
        serde_json::from_value(
            serde_json::from_str::<Value>(include_str!("../fixtures/github-bind-v1.json")).unwrap()
                ["specification"]
                .clone(),
        )
        .unwrap();
    let required: Vec<_> = policy
        .required(binding.app_id, 1, f.p.binding, "main", generation)
        .unwrap()
        .into_iter()
        .collect();
    assert_eq!(required.len(), 1);
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
                &cmd(&format!("policy-inspect-{generation}")),
                inspection,
                &treeharbor_metadata::github::InspectionSpec {
                    pull: pull.to_string(),
                    required: required.clone(),
                    ttl_seconds: 900,
                },
            )
            .await
            .unwrap();
    let mut observed = inspections::report(&f.p, &v).await;
    observed.rules_before = policy.rules.clone();
    observed.rules_after = policy.rules.clone();
    observed.checks[0].required = required[0].clone();
    observed.checks[0].runs[0].name = required[0].name.clone();
    observed.checks[0].runs[0].app_id = required[0].app_id;
    f.p.f
        .db
        .report_github_inspection(
            &f.p.reader,
            &cmd(&format!("policy-inspection-report-{generation}")),
            inspection,
            &observed,
        )
        .await
        .unwrap();
    f.inspection = inspection;
    f.spec.inspection = inspection.to_string();
    f.target = QueueTarget {
        pull: serde_json::from_value(v["target"].clone()).unwrap(),
        expected_base_revision: observed.before.base_revision,
        expected_rules: observed.rules_before,
    };
}

#[tokio::test]
#[ignore = "requires PostgreSQL; group approval inherits its immutable queue pin and legacy membership never adopts later policy"]
async fn admission_policy_group_checks_use_the_queue_generation() {
    let mut legacy = QueueFixture::with_merge_queue(true).await;
    let (owner, validator, id, spec) = setup_check(&mut legacy).await;
    legacy
        .p
        .f
        .db
        .approve_github_managed_check(&owner, &cmd("legacy-group-approve"), id, &spec)
        .await
        .unwrap();
    let policy = managed(spec.program.clone());
    set(&mut legacy.p, &owner, 0, Some(&policy)).await;
    assert!(
        legacy
            .p
            .f
            .db
            .prepare_github_managed_check(&validator, &cmd("legacy-group-prepare"), id)
            .await
            .is_err()
    );
    assert!(
        legacy
            .p
            .f
            .db
            .approve_github_managed_check(
                &owner,
                &cmd("legacy-group-new-approval"),
                Uuid::new_v4(),
                &spec
            )
            .await
            .is_err()
    );
    let raw = connection(&legacy.p.f.schema).await;
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_queue_admission)+(SELECT count(*) FROM th_github_check_admission)", &[]).await.unwrap().get::<_,i64>(0),0);
    cleanup(&legacy.p.f.schema).await;

    let mut f = QueueFixture::with_merge_queue(true).await;
    let (owner, validator, import, program) =
        setup_source(&mut f.p, &format!("git:sha1:{}", "aa".repeat(20))).await;
    let policy = managed(program.clone());
    let adopted = set(&mut f.p, &owner, 0, Some(&policy)).await;
    inspect_for_policy(&mut f, &policy, 1).await;
    f.spec.admission = Some(pin(&adopted));
    f.request().await;
    let attempt = f.begin().await;
    let observed = groups::group_report(&f).await;
    f.p.f
        .db
        .report_github_queue(
            &f.bridge,
            &cmd("pinned-group-report"),
            f.id,
            attempt,
            &observed,
        )
        .await
        .unwrap();
    let id = Uuid::new_v4();
    let spec = ManagedCheckSpec {
        queue: f.id,
        membership_sequence: 1,
        import,
        validator: "ci".into(),
        program,
    };
    f.p.f
        .db
        .approve_github_managed_check(&owner, &cmd("pinned-group-approve"), id, &spec)
        .await
        .unwrap();
    let raw = connection(&f.p.f.schema).await;
    let pins = raw.query_one("SELECT q.generation=q2.generation AND q.policy=q2.policy AND q.binding=q2.binding AND q.epoch=q2.epoch AND q.base_ref=q2.base_ref FROM th_github_queue_admission q JOIN th_github_check_admission q2 ON q.queue=$1 AND q2.check_run=$2", &[&f.id,&id]).await.unwrap();
    assert!(pins.get::<_, bool>(0));
    f.p.f
        .db
        .prepare_github_managed_check(&validator, &cmd("pinned-group-prepare"), id)
        .await
        .unwrap();
    // The same definition in a later generation is a new approval context.
    set(&mut f.p, &owner, 1, Some(&policy)).await;
    assert!(
        f.p.f
            .db
            .start_github_managed_check(&validator, &cmd("pinned-group-stale-start"), id)
            .await
            .is_err()
    );
    assert!(
        f.p.f
            .db
            .approve_github_managed_check(
                &owner,
                &cmd("pinned-group-stale-approve"),
                Uuid::new_v4(),
                &spec
            )
            .await
            .is_err()
    );
    assert_eq!(
        raw.query_one("SELECT generation FROM th_github_check_admission", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; a finished trusted-runtime fixture cannot first collect under a revoked or rotated admission policy"]
async fn admission_policy_rotation_fences_first_collection_but_allows_release() {
    for restore in [false, true] {
        let mut p = Projection::new().await;
        let (owner, validator, id, spec) = pull_checks::setup(&mut p).await;
        let policy = managed(spec.program.clone());
        set(&mut p, &owner, 0, Some(&policy)).await;
        p.f.db
            .approve_github_managed_pull_check(&owner, &cmd("collect-approve"), id, &spec)
            .await
            .unwrap();
        p.f.db
            .prepare_github_managed_check(&validator, &cmd("collect-prepare"), id)
            .await
            .unwrap();
        let (node, plan) = execution_plan(&mut p, &validator, id).await;
        p.f.db.admit_run_start(&node, &plan).await.unwrap();
        set(&mut p, &owner, 1, None).await;
        if restore {
            set(&mut p, &owner, 2, Some(&policy)).await;
        }
        // Trusted terminal-state fixture only; this does not execute a sandbox.
        let raw = connection(&p.f.schema).await;
        raw.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=0,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64)]).await.unwrap();
        p.f.db.finish_run(&node, &plan).await.unwrap();
        assert!(
            p.f.db
                .collect_github_managed_check(&validator, &cmd("first-stale-collect"), id)
                .await
                .is_err()
        );
        let row = raw.query_one("SELECT definition IS NULL,evidence IS NULL,outcome IS NULL FROM th_github_managed_executions WHERE check_run=$1", &[&id]).await.unwrap();
        assert!(row.get::<_, bool>(0));
        assert!(row.get::<_, bool>(1));
        assert!(row.get::<_, bool>(2));
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_operations WHERE kind='github.managed_check.collect.v1'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        assert_eq!(
            p.f.db
                .release_github_managed_check(&validator, &cmd("stale-release"), id)
                .await
                .unwrap()["state"],
            "draining"
        );
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; planning computes next-generation host contexts without granting policy authority or writing receipts"]
async fn admission_policy_plan_is_pure_owner_only_and_generation_checked() {
    let mut p = Projection::new().await;
    let (owner, _, _, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program);
    let reader = read(&mut p).await;
    let raw = connection(&p.f.schema).await;
    let snapshot = "SELECT jsonb_build_object('policies',(SELECT jsonb_agg(to_jsonb(p)) FROM th_github_admission_policies p),'refs',(SELECT jsonb_agg(to_jsonb(r)) FROM th_github_admission_refs r),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o),'outbox',(SELECT jsonb_agg(to_jsonb(e) ORDER BY id) FROM th_outbox e))";
    let before: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    assert_eq!(
        p.f.db
            .github_admission_policy(&reader, 1, p.binding, "main")
            .await
            .unwrap()["state"],
        "unconfigured"
    );
    let plan =
        p.f.db
            .plan_github_admission_policy(&owner, 1, p.binding, "main", 0, &policy)
            .await
            .unwrap();
    assert_eq!(plan["approved"], false);
    assert_eq!(plan["expected_generation"], 0);
    assert_eq!(plan["generation"], 1);
    assert_eq!(plan["policy"], policy.digest().unwrap().to_string());
    assert_eq!(plan["required"].as_array().unwrap().len(), 1);
    assert!(
        plan["required"][0]["name"]
            .as_str()
            .unwrap()
            .starts_with("TreeHarbor/e1/a")
    );
    assert!(matches!(
        p.f.db
            .plan_github_admission_policy(&reader, 1, p.binding, "main", 0, &policy)
            .await,
        Err(MetaError::Forbidden)
    ));
    assert!(matches!(
        p.f.db
            .plan_github_admission_policy(&owner, 1, p.binding, "main", 1, &policy)
            .await,
        Err(MetaError::HostQueueState)
    ));
    let after: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    assert_eq!(before, after);
    let adopted = set(&mut p, &owner, 0, Some(&policy)).await;
    assert_eq!(adopted["required"], plan["required"]);
    assert_eq!(adopted["policy"], plan["policy"]);
    assert!(
        p.f.db
            .plan_github_admission_policy(&owner, 1, p.binding, "main", 0, &policy)
            .await
            .is_err()
    );
    let next =
        p.f.db
            .plan_github_admission_policy(&owner, 1, p.binding, "main", 1, &policy)
            .await
            .unwrap();
    assert_ne!(plan["required"], next["required"]);
    set(&mut p, &owner, 1, None).await;
    assert_eq!(
        p.f.db
            .github_admission_policy(&reader, 1, p.binding, "main")
            .await
            .unwrap()["state"],
        "revoked"
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated v28 queue/check/publication/evidence/receipts and grants survive schema29 without implicit adoption"]
async fn admission_policy_upgrade_preserves_populated_v28_without_implicit_pins() {
    let mut f = QueueFixture::with_merge_queue(true).await;
    let (policy_owner, validator, id, spec) = setup_check(&mut f).await;
    f.p.f
        .db
        .approve_github_managed_check(&policy_owner, &cmd("upgrade-group-approve"), id, &spec)
        .await
        .unwrap();
    complete(&mut f.p, &validator, id, 0).await;
    let (owner, bridge) = actors(&mut f.p).await;
    let publication = Uuid::new_v4();
    let target = request(&mut f.p, &owner, publication, id).await;
    assert_eq!(target.version, 1);
    assert!(target.admission.is_none());
    let pre = preflight(&f.p, &target).await;
    let attempt = Uuid::new_v4();
    f.p.f
        .db
        .begin_github_check_publication(&bridge, &cmd("upgrade-begin"), publication, attempt)
        .await
        .unwrap();
    f.p.f
        .db
        .dispatch_github_check_publication(
            &bridge,
            &cmd("upgrade-dispatch"),
            publication,
            attempt,
            &pre,
        )
        .await
        .unwrap();
    let observed = report(&f.p, &target, Some(pre), cp::Outcome::Observed).await;
    let receipt =
        f.p.f
            .db
            .report_github_check_publication(
                &bridge,
                &cmd("upgrade-observed"),
                publication,
                attempt,
                &observed,
            )
            .await
            .unwrap();
    let raw = connection(&f.p.f.schema).await;
    // Reconstruct v28 from populated, compatible legacy rows. This tests the
    // actual additive migration; it does not claim execution by the old binary.
    crate::common::drop_unpopulated_v29(&raw).await;
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(28) {
        digest = Sha256::digest([digest, std::fs::read(path).unwrap()].concat()).to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=28,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    let snapshot = "SELECT jsonb_build_object('queues',(SELECT jsonb_agg(to_jsonb(q) ORDER BY queue) FROM th_github_queue q),'queue_reports',(SELECT jsonb_agg(to_jsonb(q) ORDER BY queue,sequence) FROM th_github_queue_reports q),'checks',(SELECT jsonb_agg(to_jsonb(c) ORDER BY check_run) FROM th_github_managed_checks c),'executions',(SELECT jsonb_agg(to_jsonb(e) ORDER BY check_run) FROM th_github_managed_executions e),'publications',(SELECT jsonb_agg(to_jsonb(p)-'context_name' ORDER BY publication) FROM th_github_check_publications p),'reports',(SELECT jsonb_agg(to_jsonb(p) ORDER BY publication,sequence) FROM th_github_check_publication_reports p),'grants',(SELECT jsonb_agg(to_jsonb(a) ORDER BY principal) FROM th_repository_access a),'tokens',(SELECT jsonb_agg(to_jsonb(t) ORDER BY token) FROM th_access_tokens t),'operations',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o),'outbox',(SELECT jsonb_agg(to_jsonb(o) ORDER BY id) FROM th_outbox o))";
    let before: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    let execution_bytes: Vec<u8> = raw
        .query_one(
            "SELECT definition FROM th_github_managed_executions WHERE check_run=$1",
            &[&id],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        serde_json::from_slice::<Value>(&execution_bytes).unwrap()["version"],
        1
    );
    f.p.f.db = Metadata::new(connection(&f.p.f.schema).await)
        .await
        .unwrap();
    f.p.f.db.migrate().await.unwrap();
    f.p.f.db.migrate().await.unwrap();
    let after: Value = raw.query_one(snapshot, &[]).await.unwrap().get(0);
    for key in before.as_object().unwrap().keys() {
        assert!(
            before[key] == after[key],
            "schema28 migration changed {key}"
        );
    }
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_github_check_publications WHERE context_name<>''",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        29
    );
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_admission_policies)+(SELECT count(*) FROM th_github_admission_refs)+(SELECT count(*) FROM th_github_queue_admission)+(SELECT count(*) FROM th_github_check_admission)", &[]).await.unwrap().get::<_, i64>(0), 0);
    assert_eq!(
        execution_bytes,
        raw.query_one(
            "SELECT definition FROM th_github_managed_executions WHERE check_run=$1",
            &[&id]
        )
        .await
        .unwrap()
        .get::<_, Vec<u8>>(0)
    );
    assert_eq!(
        receipt,
        f.p.f
            .db
            .report_github_check_publication(
                &bridge,
                &cmd("upgrade-observed"),
                publication,
                attempt,
                &observed,
            )
            .await
            .unwrap()
    );
    let policy = managed(spec.program);
    set(&mut f.p, &policy_owner, 0, Some(&policy)).await;
    let reader = read(&mut f.p).await;
    let old_check =
        f.p.f
            .db
            .github_managed_check_status(&reader, 1, id)
            .await
            .unwrap();
    assert_eq!(old_check["current_locally"], false);
    assert_eq!(old_check["outcome"], "passed");
    assert_eq!(old_check["admission_verified"], false);
    assert_eq!(raw.query_one("SELECT (SELECT count(*) FROM th_github_queue_admission)+(SELECT count(*) FROM th_github_check_admission)", &[]).await.unwrap().get::<_, i64>(0), 0);
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; revoked running check drains while cleanup remains fenced until trusted removal"]
async fn admission_policy_revoked_running_check_drains_before_cleanup() {
    let mut p = Projection::new().await;
    let (owner, validator, id, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program.clone());
    set(&mut p, &owner, 0, Some(&policy)).await;
    p.f.db
        .approve_github_managed_pull_check(&owner, &cmd("drain-approve"), id, &spec)
        .await
        .unwrap();
    p.f.db
        .prepare_github_managed_check(&validator, &cmd("drain-prepare"), id)
        .await
        .unwrap();
    let (node, plan) = execution_plan(&mut p, &validator, id).await;
    p.f.db.admit_run_start(&node, &plan).await.unwrap();
    set(&mut p, &owner, 1, None).await;
    let drained =
        p.f.db
            .release_github_managed_check(&validator, &cmd("drain-release"), id)
            .await
            .unwrap();
    assert_eq!(drained["state"], "draining");
    let workspace = plan.prepared().workspace;
    let storage = plan.prepared().slot.storage;
    assert!(matches!(
        p.f.db
            .claim_workspace_cleanup(&node, workspace, storage, Uuid::new_v4(), 60)
            .await,
        Err(MetaError::ExecutionState)
    ));
    // Trusted runtime removal fixture: no real runtime or physical deletion is
    // performed here. The persisted stop/removal boundary remains mandatory.
    assert_eq!(
        p.f.db.finish_run(&node, &plan).await.unwrap()["state"],
        "cancelled"
    );
    let cleanup_plan =
        p.f.db
            .claim_workspace_cleanup(&node, workspace, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let deleted = treeharbor_metadata::cleanup::DeletedFiles {
        intent_digest: [41; 32],
        receipt_digest: [42; 32],
    };
    assert!(
        p.f.db
            .complete_workspace_cleanup(&node, &cleanup_plan, &deleted)
            .await
            .is_err()
    );
    p.f.db
        .admit_workspace_cleanup(&node, &cleanup_plan, deleted.intent_digest)
        .await
        .unwrap();
    assert_eq!(
        p.f.db
            .complete_workspace_cleanup(&node, &cleanup_plan, &deleted)
            .await
            .unwrap()["state"],
        "deleted"
    );
    cleanup(&p.f.schema).await;
}

async fn complete_named_fixture(
    p: &mut Projection,
    validator: &Authorization,
    id: Uuid,
    label: &str,
) {
    p.f.db
        .prepare_github_managed_check(validator, &cmd(&format!("{label}-prepare")), id)
        .await
        .unwrap();
    let name = format!("{label}-node");
    let node = actor(&mut p.f, &name, K::Node, &[P::RuntimeControl]).await;
    let storage = Uuid::new_v4();
    let slot = StorageSlot {
        storage,
        slot: Uuid::new_v4(),
    };
    p.f.db
        .register_node_storage(p.f.scope.tenant(), &name, storage, "operator")
        .await
        .unwrap();
    p.f.db
        .register_runtime_volume(
            p.f.scope.tenant(),
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
    let job =
        p.f.db
            .claim_preparation(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    let prep = p.f.db.reserve_preparation(&node, &job, slot).await.unwrap();
    // Trusted filesystem observation, not actual workspace preparation.
    p.f.db
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
    p.f.db
        .start_github_managed_check(validator, &cmd(&format!("{label}-start")), id)
        .await
        .unwrap();
    let plan =
        p.f.db
            .claim_run(&node, storage, Uuid::new_v4(), 60)
            .await
            .unwrap()
            .unwrap();
    p.f.db.admit_run_start(&node, &plan).await.unwrap();
    // Trusted terminal runtime fixture, not execution in a real sandbox.
    connection(&p.f.schema).await.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=0,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64)]).await.unwrap();
    p.f.db.finish_run(&node, &plan).await.unwrap();
    assert_eq!(
        p.f.db
            .collect_github_managed_check(validator, &cmd(&format!("{label}-collect")), id)
            .await
            .unwrap()["outcome"],
        "passed"
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL; rotated host context can publish despite an unresolved old generation and preserves its late receipt"]
async fn admission_policy_rotation_does_not_let_unknown_old_publication_block_new_context() {
    let mut p = Projection::new().await;
    let (policy_owner, validator, old_check, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program.clone());
    set(&mut p, &policy_owner, 0, Some(&policy)).await;
    p.f.db
        .approve_github_managed_pull_check(
            &policy_owner,
            &cmd("rotate-old-approve"),
            old_check,
            &spec,
        )
        .await
        .unwrap();
    complete(&mut p, &validator, old_check, 0).await;
    let (owner, bridge) = actors(&mut p).await;
    let old_publication = Uuid::new_v4();
    let old_target = request(&mut p, &owner, old_publication, old_check).await;
    let old_preflight = preflight(&p, &old_target).await;
    let old_attempt = Uuid::new_v4();
    p.f.db
        .begin_github_check_publication(
            &bridge,
            &cmd("rotate-old-begin"),
            old_publication,
            old_attempt,
        )
        .await
        .unwrap();
    p.f.db
        .dispatch_github_check_publication(
            &bridge,
            &cmd("rotate-old-dispatch"),
            old_publication,
            old_attempt,
            &old_preflight,
        )
        .await
        .unwrap();
    set(&mut p, &policy_owner, 1, Some(&policy)).await;
    let new_check = Uuid::new_v4();
    p.f.db
        .approve_github_managed_pull_check(
            &policy_owner,
            &cmd("rotate-new-approve"),
            new_check,
            &spec,
        )
        .await
        .unwrap();
    complete_named_fixture(&mut p, &validator, new_check, "rotated").await;
    let publication = Uuid::new_v4();
    let new_spec = CheckPublicationSpec {
        check_run: new_check,
        ttl_seconds: 900,
    };
    let requested =
        p.f.db
            .request_github_check_publication(
                &owner,
                &cmd("rotate-new-request"),
                publication,
                &new_spec,
            )
            .await
            .unwrap();
    let new_target: cp::Target = serde_json::from_value(requested["target"].clone()).unwrap();
    assert_ne!(old_target.name().unwrap(), new_target.name().unwrap());
    assert_eq!(
        old_target.source.head().unwrap(),
        new_target.source.head().unwrap()
    );
    assert_eq!(old_target.program, new_target.program);
    assert!(
        p.f.db
            .request_github_check_publication(
                &owner,
                &cmd("rotate-new-duplicate"),
                Uuid::new_v4(),
                &new_spec
            )
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .dispatch_github_check_publication(
                &bridge,
                &cmd("rotate-old-retry"),
                old_publication,
                old_attempt,
                &old_preflight
            )
            .await
            .is_err()
    );
    let observed = report(&p, &old_target, Some(old_preflight), cp::Outcome::Observed).await;
    p.f.db
        .report_github_check_publication(
            &bridge,
            &cmd("rotate-old-late-observed"),
            old_publication,
            old_attempt,
            &observed,
        )
        .await
        .unwrap();
    let retained = status(&mut p, owner.principal(), old_publication).await;
    assert_eq!(retained["state"], "observed");
    assert_eq!(retained["current_locally"], false);
    assert_eq!(retained["merge_verified"], false);
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; delayed policy outbox write serializes replacement ahead of concurrent old-program approval"]
async fn admission_policy_replacement_serializes_with_check_approval_after_outbox_write() {
    let mut p = Projection::new().await;
    let (owner, _, id, spec) = pull_checks::setup(&mut p).await;
    let policy = managed(spec.program.clone());
    set(&mut p, &owner, 0, Some(&policy)).await;
    let mut replacement = policy.clone();
    replacement.managed[0]
        .program
        .argv
        .push("replacement".into());
    let raw = connection(&p.f.schema).await;
    raw.batch_execute("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_policy() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.admission_policy.changed' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(0.5); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_policy AFTER INSERT ON th_outbox FOR EACH ROW EXECUTE FUNCTION delay_policy();").await.unwrap();
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let rotate = cmd("delayed-policy-replacement");
    let approve = cmd("racing-old-program-approval");
    let (replaced, approved) = tokio::join!(
        p.f.db.set_github_admission_policy(
            &owner,
            &rotate,
            p.binding,
            "main",
            1,
            Some(&replacement)
        ),
        async {
            let mut observed = false;
            for _ in 0..200 {
                if raw
                    .query_one("SELECT is_called FROM delay_seen", &[])
                    .await
                    .unwrap()
                    .get::<_, bool>(0)
                {
                    observed = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            assert!(observed, "replacement did not reach its outbox write");
            other
                .approve_github_managed_pull_check(&owner, &approve, id, &spec)
                .await
        }
    );
    assert_eq!(replaced.unwrap()["generation"], 2);
    assert!(matches!(approved, Err(MetaError::ExecutionState)));
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_github_managed_checks WHERE check_run=$1",
            &[&id]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        0
    );
    assert_eq!(
        raw.query_one("SELECT count(*) FROM th_github_check_admission", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; validator command-token expiry during receipt/outbox writes rolls back preparation or first collection"]
async fn admission_policy_rechecks_validator_after_execution_receipt_and_outbox_writes() {
    // Approval lifetime is controlled by policy/grant/profile/explicit token
    // revocation. Approval-token expiry limits future submissions. Here the
    // validator's current command token must remain live through commit.
    for (phase, table, kind) in [
        (
            "prepare",
            "th_operations",
            "github.managed_check.prepare.v1",
        ),
        ("prepare", "th_outbox", "workspace.prepare"),
        (
            "collect",
            "th_operations",
            "github.managed_check.collect.v1",
        ),
    ] {
        let mut p = Projection::new().await;
        let (owner, validator, id, spec) = pull_checks::setup(&mut p).await;
        let policy = managed(spec.program.clone());
        set(&mut p, &owner, 0, Some(&policy)).await;
        p.f.db
            .approve_github_managed_pull_check(&owner, &cmd("expiry-approve"), id, &spec)
            .await
            .unwrap();
        if phase == "collect" {
            p.f.db
                .prepare_github_managed_check(&validator, &cmd("expiry-prepare"), id)
                .await
                .unwrap();
            let (node, plan) = execution_plan(&mut p, &validator, id).await;
            p.f.db.admit_run_start(&node, &plan).await.unwrap();
            // Trusted terminal runtime fixture, not a real sandbox execution.
            connection(&p.f.schema).await.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=0,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64)]).await.unwrap();
            p.f.db.finish_run(&node, &plan).await.unwrap();
        }
        let raw = connection(&p.f.schema).await;
        raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_execution() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='{kind}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(0.8); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_execution AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_execution();")).await.unwrap();
        raw.execute("UPDATE th_access_tokens SET expires_at=clock_timestamp()+interval '400 milliseconds' WHERE tenant=$1 AND repository=$2 AND principal='ci'",&[&p.f.scope.tenant(),&p.f.scope.repository()]).await.unwrap();
        let result = if phase == "prepare" {
            p.f.db
                .prepare_github_managed_check(&validator, &cmd("expiry-final-prepare"), id)
                .await
        } else {
            p.f.db
                .collect_github_managed_check(&validator, &cmd("expiry-final-collect"), id)
                .await
        };
        assert!(
            matches!(result, Err(MetaError::Unauthenticated)),
            "{phase}/{table}: {result:?}"
        );
        assert!(
            raw.query_one("SELECT is_called FROM delay_seen", &[])
                .await
                .unwrap()
                .get::<_, bool>(0)
        );
        assert_eq!(
            raw.query_one(
                "SELECT count(*) FROM th_operations WHERE kind=$1",
                &[&format!("github.managed_check.{phase}.v1")]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
            0
        );
        if phase == "prepare" {
            assert_eq!(
                raw.query_one("SELECT count(*) FROM th_github_managed_executions", &[])
                    .await
                    .unwrap()
                    .get::<_, i64>(0),
                0
            );
        } else {
            let row=raw.query_one("SELECT state,definition IS NULL,evidence IS NULL,outcome IS NULL FROM th_github_managed_executions WHERE check_run=$1",&[&id]).await.unwrap();
            assert_eq!(row.get::<_, String>(0), "requested");
            assert!(row.get::<_, bool>(1) && row.get::<_, bool>(2) && row.get::<_, bool>(3));
        }
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; admission workflow-file comparison preserves historical matches and never borrows legacy approval"]
async fn admission_policy_workflow_file_comparison_uses_only_its_pinned_requirements() {
    for matches_admission in [true, false] {
        let mut f = QueueFixture::with_merge_queue(true).await;
        let owner = actor(
            &mut f.p.f,
            "policy-owner",
            K::Human,
            &[P::PolicyWrite, P::Read],
        )
        .await;
        let original = workflow(&f);
        let legacy = treeharbor_github_contract::workflow_definitions::Policy {
            version: 1,
            checks: original.workflows.clone(),
        };
        let legacy_pin =
            f.p.f
                .db
                .set_github_check_policy(
                    &owner,
                    &cmd("workflow-legacy-policy"),
                    f.p.binding,
                    "main",
                    0,
                    Some(&legacy),
                )
                .await
                .unwrap();
        f.spec.policy = Some(pin(&legacy_pin));
        let mut admitted = original;
        if !matches_admission {
            // The legacy policy approves this file, but the matching workflow
            // clause is absent from the independently reviewed admission policy.
            admitted.workflows[0].content_sha256 = format!("sha256:{}", "ab".repeat(32));
        }
        let adopted = set(&mut f.p, &owner, 0, Some(&admitted)).await;
        f.spec.admission = Some(pin(&adopted));
        let reader = read(&mut f.p).await;
        f.request().await;
        let attempt = f.begin().await;
        // Trusted host captures only: these fixtures exercise durable evidence
        // linkage and policy lookup, not a real provider workflow execution.
        let membership = groups::group_report(&f).await;
        f.p.f
            .db
            .report_github_queue(
                &f.bridge,
                &cmd("workflow-membership"),
                f.id,
                attempt,
                &membership,
            )
            .await
            .unwrap();
        let checks = group_checks::report(&f, &membership).await;
        f.p.f
            .db
            .report_github_queue(&f.bridge, &cmd("workflow-checks"), f.id, attempt, &checks)
            .await
            .unwrap();
        let selected_attempt = group_attempts::report(&f, &checks).await;
        f.p.f
            .db
            .report_github_queue(
                &f.bridge,
                &cmd("workflow-attempt"),
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
                &cmd("workflow-definition"),
                f.id,
                attempt,
                &definition,
            )
            .await
            .unwrap();
        for current in [true, false] {
            if !current {
                set(&mut f.p, &owner, 1, None).await;
            }
            let page =
                f.p.f
                    .db
                    .github_queue_definitions(&reader, 1, f.id, 0)
                    .await
                    .unwrap();
            assert_eq!(
                page["captures"][0]["workflow_file_matches_pinned_policy"],
                matches_admission
            );
            assert_eq!(page["admission_policy"]["current"], current);
            assert_eq!(page["admission_policy"]["pin"]["generation"], 1);
            assert_eq!(page["workflow_policy"]["current"], true);
            for field in ["definition_verified", "execution_context_verified"] {
                assert_eq!(page["captures"][0][field], false);
            }
            assert_eq!(page["merge_verified"], false);
            assert_eq!(page["host_policy_verified"], false);
        }
        cleanup(&f.p.f.schema).await;
    }
}
