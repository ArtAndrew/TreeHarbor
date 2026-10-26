#[path = "github_admission_policy.rs"]
mod admission_policy;
use super::*;
use sha2::{Digest, Sha256};
use treeharbor_github_contract::{PullState, RequiredCheck, check_publication as cp};
use treeharbor_metadata::github::CheckPublicationSpec;

async fn complete(p: &mut Projection, validator: &Authorization, id: Uuid, exit: i64) {
    p.f.db
        .prepare_github_managed_check(validator, &cmd("prepare"), id)
        .await
        .unwrap();
    let (node, plan) = execution_plan(p, validator, id).await;
    p.f.db.admit_run_start(&node, &plan).await.unwrap();
    // Explicit trusted runtime fixture, not execution in a real sandbox.
    connection(&p.f.schema).await.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=$3,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64),&exit]).await.unwrap();
    p.f.db.finish_run(&node, &plan).await.unwrap();
    let v =
        p.f.db
            .collect_github_managed_check(validator, &cmd("collect"), id)
            .await
            .unwrap();
    assert_eq!(v["outcome"], if exit == 0 { "passed" } else { "failed" });
}
async fn pull(
    p: &mut Projection,
    exit: i64,
) -> (
    Uuid,
    Authorization,
    treeharbor_metadata::github::ManagedPullCheckSpec,
) {
    let (owner, validator, id, spec) = pull_checks::setup(p).await;
    p.f.db
        .approve_github_managed_pull_check(&owner, &cmd("approve"), id, &spec)
        .await
        .unwrap();
    complete(p, &validator, id, exit).await;
    (id, owner, spec)
}
async fn actors(p: &mut Projection) -> (Authorization, Authorization) {
    let owner = actor(
        &mut p.f,
        "release-owner",
        K::Human,
        &[P::HostQueue, P::Read],
    )
    .await;
    let bridge = actor(
        &mut p.f,
        "bridge",
        K::Bridge,
        &[
            P::HostCheck,
            P::Read,
            P::HostPublish,
            P::HostRead,
            P::HostMerge,
        ],
    )
    .await;
    (owner, bridge)
}
async fn status(p: &mut Projection, principal: &str, id: Uuid) -> Value {
    let token =
        p.f.db
            .issue_repository_token(&p.f.scope, principal, &[P::Read], 3600, "operator")
            .await
            .unwrap();
    let reader =
        p.f.db
            .authorize(&token.secret, &p.f.scope, P::Read)
            .await
            .unwrap();
    p.f.db
        .github_check_publication_status(&reader, 1, id, false)
        .await
        .unwrap()
}
async fn request(
    p: &mut Projection,
    owner: &Authorization,
    id: Uuid,
    check_run: Uuid,
) -> cp::Target {
    let v =
        p.f.db
            .request_github_check_publication(
                owner,
                &cmd("release-request"),
                id,
                &CheckPublicationSpec {
                    check_run,
                    ttl_seconds: 900,
                },
            )
            .await
            .unwrap();
    serde_json::from_value(v["target"].clone()).unwrap()
}
async fn preflight(p: &Projection, t: &cp::Target) -> cp::Preflight {
    let golden: Value = serde_json::from_str(include_str!(
        "../../../github-contract/tests/fixtures/check-publication-v1.json"
    ))
    .unwrap();
    let mut policy: cp::Policy =
        serde_json::from_value(golden["report"]["preflight"]["policy_before"].clone()).unwrap();
    policy.protection.app_bound_checks = vec![RequiredCheck {
        name: t.name().unwrap(),
        app_id: t.app_id,
    }];
    let view = match &t.source {
        cp::Source::Pull { target } => cp::SourceView::Pull(PullState {
            repository_id: target.repository_id,
            number: target.pull_number,
            node_id: target.pull_node.clone(),
            head_ref: target.head_ref.clone(),
            base_ref: target.base_ref.clone(),
            head_revision: target.head_revision.clone(),
            base_revision: format!("git:sha1:{}", "22".repeat(20)),
            state: "open".into(),
            draft: false,
            merged: false,
        }),
        cp::Source::Group { queue, .. } => {
            policy.rules = queue.expected_rules.clone();
            let v:Value=connection(&p.f.schema).await.query_one("SELECT report->'group_capture'->'before' FROM th_github_queue_reports WHERE sequence=1",&[]).await.unwrap().get(0);
            cp::SourceView::Group(Box::new(serde_json::from_value(v).unwrap()))
        }
    };
    cp::Preflight {
        before: view.clone(),
        after: view,
        policy_before: policy.clone(),
        policy_after: policy,
    }
}
async fn report(
    p: &Projection,
    t: &cp::Target,
    pref: Option<cp::Preflight>,
    outcome: cp::Outcome,
) -> cp::Report {
    let at: i64 = connection(&p.f.schema)
        .await
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    cp::Report {
        version: 1,
        started_at_unix_ms: at,
        finished_at_unix_ms: at,
        outcome,
        preflight: pref,
        check: if outcome == cp::Outcome::Observed {
            Some(cp::PublishedCheck {
                node_id: None,
                id: 73,
                app_id: t.app_id,
                name: t.name().unwrap(),
                head_revision: t.source.head().unwrap().into(),
                external_id: t.external_id().unwrap(),
                status: "completed".into(),
                conclusion: t.conclusion,
            })
        } else {
            None
        },
        token_revoked: true,
    }
}

#[test]
fn check_publication_wire_derives_outcome_and_separates_owner_from_bridge() {
    let value: Value = serde_json::from_str(include_str!(
        "../fixtures/check-publication-request-v1.json"
    ))
    .unwrap();
    assert_eq!(
        serde_json::from_value::<WireCommand>(value.clone())
            .unwrap()
            .permission(),
        P::HostQueue
    );
    for field in [
        "outcome",
        "head",
        "program",
        "evidence",
        "app_id",
        "source",
        "conclusion",
    ] {
        let mut v = value.clone();
        v["specification"][field] = json!("caller value");
        assert!(serde_json::from_value::<WireCommand>(v).is_err(), "{field}");
    }
    assert_eq!(
        serde_json::from_value::<WireCommand>(
            json!({"action":"check_publication_context","epoch":1,"publication":Uuid::new_v4()})
        )
        .unwrap()
        .permission(),
        P::HostCheck
    );
    assert_eq!(
        serde_json::from_value::<WireCommand>(
            json!({"action":"check_publication_status","epoch":1,"publication":Uuid::new_v4()})
        )
        .unwrap()
        .permission(),
        P::Read
    );
}

#[tokio::test]
#[ignore = "requires PostgreSQL; exact runtime derivation, owner/bridge authority and one-shot reservation/dispatch"]
async fn check_publication_derives_execution_and_never_regrants_a_lost_dispatch() {
    let mut p = Projection::new().await;
    let (check, _, _) = pull(&mut p, 0).await;
    let (owner, bridge) = actors(&mut p).await;
    let id = Uuid::new_v4();
    let spec = CheckPublicationSpec {
        check_run: check,
        ttl_seconds: 900,
    };
    assert!(
        p.f.db
            .request_github_check_publication(&p.bridge, &cmd("forbidden-request"), id, &spec)
            .await
            .is_err()
    );
    let target = request(&mut p, &owner, id, check).await;
    assert_eq!(target.conclusion, cp::Conclusion::Success);
    assert!(matches!(target.source, cp::Source::Pull { .. }));
    let raw = connection(&p.f.schema).await;
    let evidence: Vec<u8> = raw
        .query_one(
            "SELECT evidence FROM th_github_managed_executions WHERE check_run=$1",
            &[&check],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        target.evidence,
        ObjectId::from_bytes(evidence.try_into().unwrap()).to_string()
    );
    let receipt =
        p.f.db
            .request_github_check_publication(&owner, &cmd("release-request"), id, &spec)
            .await
            .unwrap();
    assert_eq!(receipt["target"], json!(target));
    assert!(
        p.f.db
            .request_github_check_publication(&owner, &cmd("duplicate"), Uuid::new_v4(), &spec)
            .await
            .is_err()
    );
    assert!(
        p.f.db
            .begin_github_check_publication(&p.bridge, &cmd("old-token"), id, Uuid::new_v4())
            .await
            .is_err()
    );
    let attempt = Uuid::new_v4();
    let mut second = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let a = cmd("claim-one");
    let b = cmd("claim-two");
    let (x, y) = tokio::join!(
        p.f.db
            .begin_github_check_publication(&bridge, &a, id, attempt),
        second.begin_github_check_publication(&bridge, &b, id, Uuid::new_v4())
    );
    assert_ne!(x.is_ok(), y.is_ok());
    let (claim, winning_key) = if let Ok(v) = x {
        (v, a)
    } else {
        (y.unwrap(), b)
    };
    let actual: Uuid = claim["receipt"]["attempt"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(claim["dispatch_granted"], true);
    p.f.db = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let replay =
        p.f.db
            .begin_github_check_publication(&bridge, &winning_key, id, actual)
            .await
            .unwrap();
    assert_eq!(replay["dispatch_granted"], false);
    assert_eq!(replay["receipt"], claim["receipt"]);
    let pref = preflight(&p, &target).await;
    assert!(pref.eligible(&target));
    let mut bad = pref.clone();
    bad.policy_after.protection.enforce_admins = false;
    assert!(
        p.f.db
            .dispatch_github_check_publication(&bridge, &cmd("weak-policy"), id, actual, &bad)
            .await
            .is_err()
    );
    let sent =
        p.f.db
            .dispatch_github_check_publication(&bridge, &cmd("dispatch"), id, actual, &pref)
            .await
            .unwrap();
    assert_eq!(sent["dispatch_granted"], true);
    assert_eq!(
        p.f.db
            .dispatch_github_check_publication(&bridge, &cmd("dispatch"), id, actual, &pref)
            .await
            .unwrap()["dispatch_granted"],
        false
    );
    assert!(
        p.f.db
            .dispatch_github_check_publication(&bridge, &cmd("new-dispatch-key"), id, actual, &pref)
            .await
            .is_err()
    );
    assert!(
        raw.execute(
            "UPDATE th_github_check_publications SET target='{}' WHERE publication=$1",
            &[&id]
        )
        .await
        .is_err()
    );
    let status = status(&mut p, owner.principal(), id).await;
    assert_eq!(status["preflight"], json!(pref));
    assert_eq!(status["current_locally"], true);
    assert_eq!(status["admission_verified"], false);
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; uncertain effects remain fenced across cancel/expiry/revocation and recover historically"]
async fn check_publication_unknown_effect_cannot_be_cleared_by_cancel_or_old_receipts() {
    let mut p = Projection::new().await;
    let (check, _, _) = pull(&mut p, 0).await;
    let (owner, bridge) = actors(&mut p).await;
    let id = Uuid::new_v4();
    let t = request(&mut p, &owner, id, check).await;
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
    let unknown = report(&p, &t, Some(pref.clone()), cp::Outcome::Unknown).await;
    let ack =
        p.f.db
            .report_github_check_publication(&bridge, &cmd("unknown"), id, attempt, &unknown)
            .await
            .unwrap();
    assert_eq!(ack["sequence"], 1);
    assert_eq!(
        p.f.db
            .report_github_check_publication(&bridge, &cmd("unknown"), id, attempt, &unknown)
            .await
            .unwrap(),
        ack
    );
    p.f.db
        .cancel_github_check_publication(&owner, &cmd("cancel"), id)
        .await
        .unwrap();
    let new_owner = actor(
        &mut p.f,
        "new-release-owner",
        K::Human,
        &[P::HostQueue, P::Read],
    )
    .await;
    p.f.db
        .set_repository_access(&p.f.scope, "release-owner", &[], "operator")
        .await
        .unwrap();
    assert!(
        p.f.db
            .request_github_check_publication(
                &new_owner,
                &cmd("cannot-unlock"),
                Uuid::new_v4(),
                &CheckPublicationSpec {
                    check_run: check,
                    ttl_seconds: 60
                }
            )
            .await
            .is_err()
    );
    let not_sent = report(&p, &t, Some(pref.clone()), cp::Outcome::NotSent).await;
    assert!(
        p.f.db
            .report_github_check_publication(&bridge, &cmd("cannot-erase"), id, attempt, &not_sent)
            .await
            .is_err()
    );
    p.new_version().await;
    let observed = report(&p, &t, Some(pref), cp::Outcome::Observed).await;
    let mut wrong = observed.clone();
    wrong.check.as_mut().unwrap().head_revision = format!("git:sha1:{}", "ff".repeat(20));
    assert!(
        p.f.db
            .report_github_check_publication(&bridge, &cmd("wrong-head"), id, attempt, &wrong)
            .await
            .is_err()
    );
    let ack =
        p.f.db
            .report_github_check_publication(&bridge, &cmd("observed"), id, attempt, &observed)
            .await
            .unwrap();
    assert_eq!(ack["sequence"], 2);
    assert_eq!(
        p.f.db
            .report_github_check_publication(&bridge, &cmd("observed"), id, attempt, &observed)
            .await
            .unwrap(),
        ack
    );
    let status = status(&mut p, new_owner.principal(), id).await;
    assert_eq!(status["state"], "observed");
    assert_eq!(status["current_locally"], false);
    assert_eq!(
        status["observation"]["check"]["head_revision"],
        t.source.head().unwrap()
    );
    let mut contradiction = observed;
    contradiction.check.as_mut().unwrap().id = 74;
    assert!(
        p.f.db
            .report_github_check_publication(
                &bridge,
                &cmd("contradiction"),
                id,
                attempt,
                &contradiction
            )
            .await
            .is_err()
    );
    let raw = connection(&p.f.schema).await;
    assert_eq!(
        raw.query_one(
            "SELECT count(*) FROM th_github_check_publication_reports",
            &[]
        )
        .await
        .unwrap()
        .get::<_, i64>(0),
        2
    );
    assert!(
        raw.execute("DELETE FROM th_github_check_publication_reports", &[])
            .await
            .is_err()
    );
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; source/trust/token fences at final irreversible boundary"]
async fn check_publication_dispatch_rechecks_each_live_dependency() {
    for reason in [
        "source",
        "approval",
        "policy-token",
        "validator-token",
        "release-token",
        "bridge-token",
        "profile",
        "node",
        "binding",
        "epoch",
    ] {
        let mut p = Projection::new().await;
        let (check, policy, spec) = pull(&mut p, 0).await;
        let (owner, bridge) = actors(&mut p).await;
        let id = Uuid::new_v4();
        let t = request(&mut p, &owner, id, check).await;
        let attempt = Uuid::new_v4();
        p.f.db
            .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
            .await
            .unwrap();
        let pref = preflight(&p, &t).await;
        let raw = connection(&p.f.schema).await;
        match reason {
            "source" => p.new_version().await,
            "approval" => {
                p.f.db
                    .revoke_github_managed_check(&policy, &cmd("revoke-approval"), check)
                    .await
                    .unwrap();
            }
            "policy-token" | "validator-token" | "release-token" | "bridge-token" => {
                let sql = match reason {
                    "policy-token" => "SELECT token FROM th_github_managed_checks LIMIT 1",
                    "validator-token" => "SELECT token FROM th_github_managed_executions LIMIT 1",
                    "release-token" => "SELECT token FROM th_github_check_publications LIMIT 1",
                    _ => "SELECT attempt_token FROM th_github_check_publications LIMIT 1",
                };
                let token = raw.query_one(sql, &[]).await.unwrap().get(0);
                p.f.db
                    .revoke_repository_token(&p.f.scope, token, "operator")
                    .await
                    .unwrap();
            }
            "profile" => {
                p.f.db
                    .revoke_runtime_profile(
                        &p.f.scope,
                        *spec
                            .program
                            .runtime_profile
                            .parse::<ObjectId>()
                            .unwrap()
                            .as_bytes(),
                        "operator",
                    )
                    .await
                    .unwrap()
            }
            "node" => {
                p.f.db
                    .set_repository_access(&p.f.scope, "check-node", &[], "operator")
                    .await
                    .unwrap()
            }
            "binding" => {
                p.f.db
                    .revoke_github(&p.owner, &cmd("revoke-binding"), p.binding)
                    .await
                    .unwrap();
            }
            "epoch" => {
                raw.execute("UPDATE th_repositories SET epoch=2", &[])
                    .await
                    .unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            p.f.db
                .dispatch_github_check_publication(&bridge, &cmd("blocked"), id, attempt, &pref)
                .await
                .is_err(),
            "{reason}"
        );
        assert!(
            raw.query_one(
                "SELECT preflight IS NULL FROM th_github_check_publications",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
        );
        cleanup(&p.f.schema).await;
    }
}

#[tokio::test]
#[ignore = "requires PostgreSQL; atomic no-send closes an unadmitted attempt and fences late callback"]
async fn check_publication_proven_no_send_allows_explicit_new_intent_only() {
    let mut p = Projection::new().await;
    let (check, _, _) = pull(&mut p, 0).await;
    let (owner, bridge) = actors(&mut p).await;
    let id = Uuid::new_v4();
    let t = request(&mut p, &owner, id, check).await;
    let attempt = Uuid::new_v4();
    p.f.db
        .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
        .await
        .unwrap();
    let no_send = report(&p, &t, None, cp::Outcome::NotSent).await;
    p.f.db
        .report_github_check_publication(&bridge, &cmd("no-send"), id, attempt, &no_send)
        .await
        .unwrap();
    let pref = preflight(&p, &t).await;
    assert!(
        p.f.db
            .dispatch_github_check_publication(&bridge, &cmd("late-dispatch"), id, attempt, &pref)
            .await
            .is_err()
    );
    let next = Uuid::new_v4();
    p.f.db
        .request_github_check_publication(
            &owner,
            &cmd("explicit-new"),
            next,
            &CheckPublicationSpec {
                check_run: check,
                ttl_seconds: 60,
            },
        )
        .await
        .unwrap();
    assert_eq!(
        status(&mut p, owner.principal(), next).await["state"],
        "requested"
    );
    let old =
        p.f.db
            .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
            .await
            .unwrap();
    assert_eq!(old["dispatch_granted"], false);
    assert_eq!(old["receipt"]["state"], "attempting");
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; PR failure and exact combined-head group results derive from protected execution"]
async fn check_publication_preserves_failure_and_combined_group_identity() {
    let mut p = Projection::new().await;
    let (check, _, _) = pull(&mut p, 7).await;
    let (owner, _) = actors(&mut p).await;
    let t = request(&mut p, &owner, Uuid::new_v4(), check).await;
    assert_eq!(t.conclusion, cp::Conclusion::Failure);
    cleanup(&p.f.schema).await;

    let mut f = QueueFixture::with_merge_queue(true).await;
    let (policy, validator, check, spec) = setup_check(&mut f).await;
    f.p.f
        .db
        .approve_github_managed_check(&policy, &cmd("approve"), check, &spec)
        .await
        .unwrap();
    complete(&mut f.p, &validator, check, 0).await;
    let (owner, bridge) = actors(&mut f.p).await;
    let id = Uuid::new_v4();
    let t = request(&mut f.p, &owner, id, check).await;
    assert!(matches!(t.source, cp::Source::Group { .. }));
    assert_eq!(
        t.source.head().unwrap(),
        format!("git:sha1:{}", "aa".repeat(20))
    );
    assert_ne!(t.source.head().unwrap(), t.source.pull().head_revision);
    let pref = preflight(&f.p, &t).await;
    assert!(pref.eligible(&t));
    let attempt = Uuid::new_v4();
    f.p.f
        .db
        .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
        .await
        .unwrap();
    f.p.f
        .db
        .dispatch_github_check_publication(&bridge, &cmd("dispatch"), id, attempt, &pref)
        .await
        .unwrap();
    let mut r = report(&f.p, &t, Some(pref), cp::Outcome::Observed).await;
    r.check.as_mut().unwrap().head_revision = t.source.pull().head_revision.clone();
    assert!(
        f.p.f
            .db
            .report_github_check_publication(&bridge, &cmd("wrong-pr-head"), id, attempt, &r)
            .await
            .is_err()
    );
    r.check.as_mut().unwrap().head_revision = t.source.head().unwrap().into();
    f.p.f
        .db
        .report_github_check_publication(&bridge, &cmd("group-observed"), id, attempt, &r)
        .await
        .unwrap();
    assert_eq!(
        status(&mut f.p, owner.principal(), id).await["state"],
        "observed"
    );
    cleanup(&f.p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; no-send recovery and final dispatch cannot both commit"]
async fn check_publication_no_send_racing_dispatch_has_one_winner() {
    let mut p = Projection::new().await;
    let (check, _, _) = pull(&mut p, 0).await;
    let (owner, bridge) = actors(&mut p).await;
    let id = Uuid::new_v4();
    let t = request(&mut p, &owner, id, check).await;
    let attempt = Uuid::new_v4();
    p.f.db
        .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
        .await
        .unwrap();
    let pref = preflight(&p, &t).await;
    let no_send = report(&p, &t, None, cp::Outcome::NotSent).await;
    let mut other = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    let dispatch = cmd("race-dispatch");
    let recover = cmd("race-no-send");
    let (sent, closed) = tokio::join!(
        p.f.db
            .dispatch_github_check_publication(&bridge, &dispatch, id, attempt, &pref),
        other.report_github_check_publication(&bridge, &recover, id, attempt, &no_send)
    );
    assert_ne!(sent.is_ok(), closed.is_ok());
    let state = status(&mut p, owner.principal(), id).await;
    if sent.is_ok() {
        assert_eq!(state["state"], "attempting");
        assert_eq!(state["preflight"], json!(pref));
        assert!(
            p.f.db
                .report_github_check_publication(&bridge, &recover, id, attempt, &no_send)
                .await
                .is_err()
        );
    } else {
        assert_eq!(state["state"], "not_sent");
        assert!(state["preflight"].is_null());
        assert!(
            p.f.db
                .dispatch_github_check_publication(&bridge, &dispatch, id, attempt, &pref)
                .await
                .is_err()
        );
    }
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; populated schema27 upgrade preserves evidence, receipts and permission arrays"]
async fn check_publication_upgrade_does_not_expand_existing_credentials() {
    let mut p = Projection::new().await;
    let (check, _, _) = pull(&mut p, 0).await;
    let raw = connection(&p.f.schema).await;
    let before: Value = raw.query_one("SELECT jsonb_build_object('executions',(SELECT jsonb_agg(to_jsonb(e) ORDER BY check_run) FROM th_github_managed_executions e),'grants',(SELECT jsonb_agg(to_jsonb(a) ORDER BY principal) FROM th_repository_access a),'tokens',(SELECT jsonb_agg(to_jsonb(t) ORDER BY token) FROM th_access_tokens t),'receipts',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o))", &[]).await.unwrap().get(0);
    crate::common::drop_unpopulated_v28(&raw).await;
    let mut paths: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/migrations"))
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "sql"))
        .collect();
    paths.sort();
    let mut digest = Vec::new();
    for path in paths.iter().take(27) {
        digest = Sha256::digest([digest, std::fs::read(path).unwrap()].concat()).to_vec();
    }
    raw.execute(
        "UPDATE th_schema_version SET version=27,migration_digest=$1",
        &[&digest],
    )
    .await
    .unwrap();
    p.f.db = Metadata::new(connection(&p.f.schema).await).await.unwrap();
    p.f.db.migrate().await.unwrap();
    p.f.db.migrate().await.unwrap();
    let after: Value = raw.query_one("SELECT jsonb_build_object('executions',(SELECT jsonb_agg(to_jsonb(e) ORDER BY check_run) FROM th_github_managed_executions e),'grants',(SELECT jsonb_agg(to_jsonb(a) ORDER BY principal) FROM th_repository_access a),'tokens',(SELECT jsonb_agg(to_jsonb(t) ORDER BY token) FROM th_access_tokens t),'receipts',(SELECT jsonb_agg(to_jsonb(o) ORDER BY key) FROM th_operations o))", &[]).await.unwrap().get(0);
    assert_eq!(before, after);
    assert_eq!(
        raw.query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        29
    );
    assert!(
        p.f.db
            .begin_github_check_publication(
                &p.bridge,
                &cmd("old-token"),
                Uuid::new_v4(),
                Uuid::new_v4()
            )
            .await
            .is_err()
    );
    for kind in [K::Human, K::Agent, K::Validator, K::Node] {
        let name = format!("not-check-{}", kind.as_str());
        p.f.db
            .provision_principal(p.f.scope.tenant(), &name, kind, "operator")
            .await
            .unwrap();
        assert!(
            p.f.db
                .set_repository_access(&p.f.scope, &name, &[P::HostCheck], "operator")
                .await
                .is_err()
        );
    }
    let (owner, bridge) = actors(&mut p).await;
    let id = Uuid::new_v4();
    request(&mut p, &owner, id, check).await;
    p.f.db
        .begin_github_check_publication(&bridge, &cmd("explicit-grant"), id, Uuid::new_v4())
        .await
        .unwrap();
    cleanup(&p.f.schema).await;
}

#[tokio::test]
#[ignore = "requires PostgreSQL; expiry at receipt/outbox boundaries rolls back reservations and dispatch"]
async fn check_publication_expiry_rolls_back_every_irreversible_grant() {
    for stage in ["request", "begin", "dispatch"] {
        for table in ["th_operations", "th_outbox"] {
            let mut p = Projection::new().await;
            let (check, _, _) = pull(&mut p, 0).await;
            let (owner, bridge) = actors(&mut p).await;
            let id = Uuid::new_v4();
            let attempt = Uuid::new_v4();
            let spec = CheckPublicationSpec {
                check_run: check,
                ttl_seconds: 1,
            };
            let mut pref = None;
            if stage != "request" {
                let v =
                    p.f.db
                        .request_github_check_publication(&owner, &cmd("request"), id, &spec)
                        .await
                        .unwrap();
                let t = serde_json::from_value(v["target"].clone()).unwrap();
                pref = Some(preflight(&p, &t).await);
                if stage == "dispatch" {
                    p.f.db
                        .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
                        .await
                        .unwrap();
                }
            }
            let suffix = match (stage, table) {
                ("request", "th_operations") => "request.v1",
                ("request", _) => "requested",
                ("begin", "th_operations") => "begin.v1",
                ("begin", _) => "attempting",
                (_, "th_operations") => "dispatch.v1",
                _ => "dispatched",
            };
            let raw = connection(&p.f.schema).await;
            raw.batch_execute(&format!("CREATE SEQUENCE delay_seen; CREATE FUNCTION delay_check() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.kind='github.check_publication.{suffix}' THEN PERFORM nextval('delay_seen'); PERFORM pg_sleep(1.2); END IF; RETURN NEW; END $$; CREATE TRIGGER delay_check AFTER INSERT ON {table} FOR EACH ROW EXECUTE FUNCTION delay_check();")).await.unwrap();
            let value = match stage {
                "request" => {
                    p.f.db
                        .request_github_check_publication(&owner, &cmd("request"), id, &spec)
                        .await
                }
                "begin" => {
                    p.f.db
                        .begin_github_check_publication(&bridge, &cmd("begin"), id, attempt)
                        .await
                }
                _ => {
                    p.f.db
                        .dispatch_github_check_publication(
                            &bridge,
                            &cmd("dispatch"),
                            id,
                            attempt,
                            pref.as_ref().unwrap(),
                        )
                        .await
                }
            };
            assert!(value.is_err(), "{stage}/{table}");
            assert!(
                raw.query_one("SELECT is_called FROM delay_seen", &[])
                    .await
                    .unwrap()
                    .get::<_, bool>(0)
            );
            assert_eq!(
                raw.query_one(
                    "SELECT count(*) FROM th_operations WHERE kind=$1",
                    &[&format!("github.check_publication.{stage}.v1")]
                )
                .await
                .unwrap()
                .get::<_, i64>(0),
                0
            );
            if stage == "request" {
                assert_eq!(
                    raw.query_one("SELECT count(*) FROM th_github_check_publications", &[])
                        .await
                        .unwrap()
                        .get::<_, i64>(0),
                    0
                );
            } else {
                let r = raw
                    .query_one(
                        "SELECT state,preflight IS NULL FROM th_github_check_publications",
                        &[],
                    )
                    .await
                    .unwrap();
                assert_eq!(
                    r.get::<_, String>(0),
                    if stage == "begin" {
                        "requested"
                    } else {
                        "attempting"
                    }
                );
                assert!(r.get::<_, bool>(1));
            }
            cleanup(&p.f.schema).await;
        }
    }
}
