use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use treeharbor_github_contract::check_publication::*;

pub(super) fn fixture() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/check-publication-v1.json"
    )))
    .unwrap()
}
pub(super) fn target() -> Target {
    serde_json::from_value(fixture()["target"].clone()).unwrap()
}
fn pull() -> Value {
    let t = target();
    let p = t.source.pull();
    json!({"number":p.pull_number,"node_id":p.pull_node,"head":{"ref":p.head_ref,"sha":"11".repeat(20),"repo":{"id":11}},"base":{"ref":p.base_ref,"sha":"22".repeat(20),"repo":{"id":11}},"state":"open","draft":false,"merged":false})
}
pub(super) fn access(write: bool) -> Vec<Vec<u8>> {
    let mut token = token();
    token["permissions"] =
        json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read"});
    if write {
        token["permissions"]["checks"] = json!("write");
        token["permissions"]["administration"] = json!("read");
        token["permissions"]["merge_queues"] = json!("read");
    }
    vec![
        response(200, installation()),
        response(201, token),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn before() -> Vec<Vec<u8>> {
    let f = fixture();
    let mut r = access(true);
    for _ in 0..2 {
        r.extend([
            response(200, pull()),
            response(200, f["raw_protection"].clone()),
            response(200, f["raw_rules"].clone()),
        ]);
    }
    r
}
pub(super) fn run(t: &Target) -> Value {
    json!({"id":73,"app":{"id":t.app_id},"name":t.name().unwrap(),"head_sha":t.source.head().unwrap().strip_prefix("git:sha1:").unwrap(),"external_id":t.external_id().unwrap(),"status":"completed","conclusion":t.conclusion})
}
fn body(request: &[u8]) -> Value {
    serde_json::from_str(request_text(request).split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn recovery_reads_all_pages_twice_without_hiding_competing_runs() {
    let t = target();
    let mut other = Vec::new();
    for id in 100..120 {
        let mut r = run(&t);
        r["id"] = json!(id);
        r["external_id"] = json!(format!("other-{id}"));
        other.push(r);
    }
    let mut responses = access(false);
    for _ in 0..2 {
        responses.extend([
            response(200, json!({"total_count":21,"check_runs":other})),
            response(200, json!({"total_count":21,"check_runs":[run(&t)]})),
        ]);
    }
    responses.push(revoked());
    let (c, h) = host(responses).await;
    let r = c
        .observe_published_check(&binding(), &t, None)
        .await
        .unwrap();
    assert_eq!(r.report.outcome, Outcome::Observed);
    let req = h.await.unwrap();
    assert_eq!(req.len(), 9);
    for (index, page) in [(4, 1), (5, 2), (6, 1), (7, 2)] {
        assert!(request_text(&req[index]).contains(&format!("per_page=20&page={page} ")));
    }
}
fn listing(runs: Vec<Value>) -> Value {
    json!({"total_count":runs.len(),"check_runs":runs})
}

#[tokio::test]
async fn exact_check_uses_only_scoped_write_and_immutable_program_evidence() {
    let t = target();
    let mut responses = before();
    responses.extend([response(201, run(&t)), response(200, run(&t)), revoked()]);
    let (c, h) = host(responses).await;
    let calls = AtomicUsize::new(0);
    let result = c
        .publish_check(&binding(), &t, |p| {
            assert_eq!(json!(p), fixture()["report"]["preflight"]);
            calls.fetch_add(1, Ordering::SeqCst);
            async { Ok(()) }
        })
        .await
        .unwrap();
    result.report.validate(&t).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(result.report.outcome, Outcome::Observed);
    assert!(result.report.token_revoked);
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 13);
    assert_eq!(
        body(&requests[1]),
        json!({"repository_ids":[11],"permissions":{"contents":"read","metadata":"read","pull_requests":"read","checks":"write","administration":"read","merge_queues":"read"}})
    );
    assert!(
        requests[2..10]
            .iter()
            .all(|r| request_text(r).starts_with("GET "))
    );
    assert!(
        request_text(&requests[5])
            .starts_with("GET /repos/example-org/demo/branches/main/protection ")
    );
    assert!(request_text(&requests[10]).starts_with("POST /repos/example-org/demo/check-runs "));
    assert!(request_text(&requests[11]).starts_with("GET /repos/example-org/demo/check-runs/73 "));
    let sent = body(&requests[10]);
    assert_eq!(sent["name"], fixture()["name"]);
    assert_eq!(sent["external_id"], fixture()["external_id"]);
    assert_eq!(sent["head_sha"], "11".repeat(20));
    assert_eq!(sent["conclusion"], "success");
    assert_eq!(sent["status"], "completed");
    assert!(!json!(result.report).to_string().contains(TEST_TOKEN));
}

#[tokio::test]
async fn version_two_publishes_the_independent_admission_context_and_exact_identity() {
    let f: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/check-publication-v2.json"
    )))
    .unwrap();
    let t: Target = serde_json::from_value(f["target"].clone()).unwrap();
    let mut p = pull();
    p["base"]["ref"] = json!(t.source.pull().base_ref);
    let exact_run = json!({
        "id":73,"app":{"id":7},"name":f["name"],
        "head_sha":"11".repeat(20),"external_id":f["external_id"],
        "status":"completed","conclusion":"success"
    });
    let mut responses = access(true);
    for _ in 0..2 {
        responses.extend([
            response(200, p.clone()),
            response(200, f["raw_protection"].clone()),
            response(200, f["raw_rules"].clone()),
        ]);
    }
    responses.extend([
        response(201, exact_run.clone()),
        response(200, exact_run),
        revoked(),
    ]);
    let (c, h) = host(responses).await;
    let result = c
        .publish_check(&binding(), &t, |preflight| {
            assert_eq!(json!(preflight), f["report"]["preflight"]);
            async { Ok(()) }
        })
        .await
        .unwrap();
    result.report.validate(&t).unwrap();
    assert_eq!(json!(result.report.check), f["report"]["check"]);
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 13);
    assert!(
        request_text(&requests[5])
            .starts_with("GET /repos/example-org/demo/branches/release%2F2026.10/protection ")
    );
    let sent = body(&requests[10]);
    assert_eq!(sent["name"], f["name"]);
    assert_eq!(sent["external_id"], f["external_id"]);
    assert_eq!(sent["head_sha"], "11".repeat(20));
    assert_eq!(sent["status"], "completed");
    assert_eq!(sent["conclusion"], "success");
}

#[tokio::test]
async fn version_two_recovery_filters_by_current_context_and_rejects_older_names() {
    let f: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/check-publication-v2.json"
    )))
    .unwrap();
    let t: Target = serde_json::from_value(f["target"].clone()).unwrap();
    for legacy_name in [false, true] {
        let exact_run = json!({
            "id":73,"app":{"id":7},
            "name":if legacy_name {fixture()["name"].clone()} else {f["name"].clone()},
            "head_sha":"11".repeat(20),"external_id":f["external_id"],
            "status":"completed","conclusion":"success"
        });
        let mut responses = access(false);
        responses.extend([
            response(200, listing(vec![exact_run.clone()])),
            response(200, listing(vec![exact_run])),
            revoked(),
        ]);
        let (c, h) = host(responses).await;
        let result = c.observe_published_check(&binding(), &t, None).await;
        if legacy_name {
            assert!(result.is_err());
        } else {
            let report = result.unwrap().report;
            assert_eq!(report.outcome, Outcome::Observed);
            assert_eq!(json!(report.check), f["report"]["check"]);
        }
        let requests = h.await.unwrap();
        let query_name = f["name"].as_str().unwrap().replace('/', "%2F");
        for request in &requests[4..6] {
            assert!(request_text(request).contains(&format!("check_name={query_name}&")));
        }
        assert!(
            requests
                .iter()
                .all(|r| !request_text(r).starts_with("POST /repos/"))
        );
    }
}

#[tokio::test]
async fn policy_head_and_callback_failures_never_send_a_check() {
    for case in [
        "head",
        "base",
        "issuer",
        "any-app",
        "name",
        "admins",
        "force",
        "delete",
        "bypass",
        "rules",
        "policy-drift",
        "callback",
    ] {
        let t = target();
        let f = fixture();
        let mut p = pull();
        let mut protection = f["raw_protection"].clone();
        let mut rules = f["raw_rules"].clone();
        match case {
            "head" => p["head"]["sha"] = json!("44".repeat(20)),
            "base" => p["base"]["sha"] = json!("44".repeat(20)),
            "issuer" => protection["required_status_checks"]["checks"][0]["app_id"] = json!(8),
            "any-app" => protection["required_status_checks"]["checks"][0]["app_id"] = Value::Null,
            "name" => {
                protection["required_status_checks"]["checks"][0]["context"] = json!("TreeHarbor")
            }
            "admins" => protection["enforce_admins"]["enabled"] = json!(false),
            "force" => protection["allow_force_pushes"]["enabled"] = json!(true),
            "delete" => protection["allow_deletions"]["enabled"] = json!(true),
            "bypass" => {
                protection["required_pull_request_reviews"] = json!({"bypass_pull_request_allowances":{"users":[{"id":1}],"teams":[],"apps":[]}})
            }
            "rules" => rules = json!([]),
            "policy-drift" => protection["extra"] = json!("changed"),
            "callback" => {}
            _ => unreachable!(),
        }
        let mut responses = before();
        responses[7] = response(200, p);
        responses[8] = response(200, protection);
        responses[9] = response(200, rules);
        responses.push(revoked());
        let (c, h) = host(responses).await;
        let called = AtomicUsize::new(0);
        let failure = c
            .publish_check(&binding(), &t, |_| async {
                called.fetch_add(1, Ordering::SeqCst);
                Err(Error::PublicationStopped)
            })
            .await
            .err()
            .unwrap();
        assert!(failure.mutation_not_sent, "{case}");
        assert_eq!(
            called.load(Ordering::SeqCst),
            usize::from(case == "callback"),
            "{case}"
        );
        let req = h.await.unwrap();
        assert_eq!(req.len(), 11);
        assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn unavailable_or_malformed_protection_blocks_before_dispatch() {
    for malformed in [
        None,
        Some(json!({})),
        Some(
            json!({"required_status_checks":{"checks":[]},"enforce_admins":{"enabled":true},"allow_force_pushes":{"enabled":false}}),
        ),
    ] {
        let mut responses = access(true);
        responses.push(response(200, pull()));
        responses.push(match malformed {
            Some(v) => response(200, v),
            None => response(404, json!({"message":"not protected"})),
        });
        responses.push(revoked());
        let (c, h) = host(responses).await;
        let failure = c
            .publish_check(&binding(), &target(), |_| async {
                panic!("must not dispatch")
            })
            .await
            .err()
            .unwrap();
        assert!(failure.mutation_not_sent);
        assert_eq!(h.await.unwrap().len(), 7);
    }
}

#[tokio::test]
async fn uncertain_post_and_changed_receipt_are_not_replayed() {
    for case in [
        "lost",
        "redirect",
        "issuer",
        "head",
        "external",
        "conclusion",
        "reread",
    ] {
        let t = target();
        let mut returned = run(&t);
        let mut responses = before();
        match case {
            "issuer" => returned["app"]["id"] = json!(8),
            "head" => returned["head_sha"] = json!("44".repeat(20)),
            "external" => returned["external_id"] = json!("other"),
            "conclusion" => returned["conclusion"] = json!("neutral"),
            _ => {}
        }
        responses.push(match case {
            "lost" => vec![],
            "redirect" => raw(307, "Location: http://127.0.0.1:9/forbidden\r\n", b""),
            _ => response(201, returned),
        });
        if case == "reread" {
            let mut r = run(&t);
            r["id"] = json!(74);
            responses.push(response(200, r));
        }
        responses.push(revoked());
        let (c, h) = host(responses).await;
        let failure = c
            .publish_check(&binding(), &t, |_| async { Ok(()) })
            .await
            .err()
            .unwrap();
        assert!(!failure.mutation_not_sent, "{case}");
        assert!(!failure.error.to_string().contains(TEST_TOKEN));
        let req = h.await.unwrap();
        assert_eq!(
            req.iter()
                .filter(|r| request_text(r).starts_with("POST /repos/"))
                .count(),
            1
        );
        assert_eq!(c.slots.available_permits(), 4);
    }
}

#[tokio::test]
async fn recovery_is_read_only_and_stable_absence_remains_unknown() {
    for found in [true, false] {
        let t = target();
        let mut unrelated = run(&t);
        unrelated["id"] = json!(74);
        unrelated["external_id"] = json!("other-attempt");
        let runs = if found {
            vec![unrelated, run(&t)]
        } else {
            vec![unrelated]
        };
        let mut responses = access(false);
        responses.extend([
            response(200, listing(runs.clone())),
            response(200, listing(runs)),
            revoked(),
        ]);
        let (c, h) = host(responses).await;
        let result = c
            .observe_published_check(&binding(), &t, None)
            .await
            .unwrap();
        assert_eq!(
            result.report.outcome,
            if found {
                Outcome::Observed
            } else {
                Outcome::Unknown
            }
        );
        assert_eq!(result.report.check.is_some(), found);
        assert!(result.report.preflight.is_none());
        let req = h.await.unwrap();
        assert_eq!(
            body(&req[1]),
            json!({"repository_ids":[11],"permissions":{"contents":"read","metadata":"read","pull_requests":"read","checks":"read"}})
        );
        assert!(
            req[2..6]
                .iter()
                .all(|r| request_text(r).starts_with("GET "))
        );
        assert!(request_text(&req[4]).contains("filter=all&per_page=20&page=1 "));
        assert!(request_text(&req[4]).contains("TreeHarbor%2Fe1%2F"));
    }
}

#[tokio::test]
async fn recovery_rejects_drift_duplicate_and_incomplete_results() {
    for case in [
        "drift",
        "duplicate-external",
        "duplicate-id",
        "incomplete",
        "over-limit",
        "head",
        "status",
        "negative",
        "external-app",
    ] {
        let t = target();
        let mut r = run(&t);
        let mut first = listing(vec![r.clone()]);
        match case {
            "duplicate-external" => {
                r["id"] = json!(74);
                first = listing(vec![run(&t), r]);
            }
            "duplicate-id" => first = listing(vec![r.clone(), r]),
            "incomplete" => first["total_count"] = json!(2),
            "over-limit" => first["total_count"] = json!(1001),
            "head" => first["check_runs"][0]["head_sha"] = json!("44".repeat(20)),
            "status" => first["check_runs"][0]["status"] = json!("in_progress"),
            "negative" => first["check_runs"][0]["conclusion"] = json!("failure"),
            "external-app" => first["check_runs"][0]["app"]["id"] = json!(8),
            _ => {}
        }
        let mut responses = access(false);
        responses.push(response(200, first.clone()));
        if !matches!(case, "duplicate-id" | "incomplete" | "over-limit") {
            responses.push(response(
                200,
                if case == "drift" {
                    listing(vec![])
                } else {
                    first
                },
            ));
        }
        responses.push(revoked());
        let (c, h) = host(responses).await;
        let result = c.observe_published_check(&binding(), &t, None).await;
        if case == "external-app" {
            assert_eq!(result.unwrap().report.outcome, Outcome::Unknown);
        } else {
            assert!(result.is_err(), "{case}");
        }
        h.await.unwrap();
    }
}

#[tokio::test]
async fn negative_outcomes_and_failed_revocation_are_preserved() {
    for conclusion in [
        Conclusion::Failure,
        Conclusion::Cancelled,
        Conclusion::ActionRequired,
    ] {
        let mut t = target();
        t.conclusion = conclusion;
        let mut responses = before();
        responses.extend([
            response(201, run(&t)),
            response(200, run(&t)),
            response(500, json!({"error":"retry later"})),
        ]);
        let (c, h) = host(responses).await;
        let result = c
            .publish_check(&binding(), &t, |_| async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(result.report.check.unwrap().conclusion, conclusion);
        assert!(!result.report.token_revoked);
        assert_eq!(body(&h.await.unwrap()[10])["conclusion"], json!(conclusion));
    }
}
