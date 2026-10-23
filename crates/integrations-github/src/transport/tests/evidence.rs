use super::*;
use treeharbor_github_contract::{InspectionReport, InspectionTarget};
fn fixture() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/inspection-v1.json"
    )))
    .unwrap()
}
fn target() -> InspectionTarget {
    serde_json::from_value(fixture()["target"].clone()).unwrap()
}
fn pull() -> Value {
    let t = target();
    json!({"number":t.pull_number,"node_id":t.pull_node,"head":{"ref":t.head_ref,"sha":"11".repeat(20),"repo":{"id":11}},"base":{"ref":t.base_ref,"sha":"22".repeat(20),"repo":{"id":11}},"state":"open","draft":false,"merged":false})
}
fn runs() -> Value {
    json!({"total_count":1,"check_runs":[{"id":19,"name":"CI / build","app":{"id":7},"head_sha":"11".repeat(20),"status":"completed","conclusion":"success"}]})
}
fn access() -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["checks"] = json!("read");
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn capture(before: Value, check: Value, rules: Value, after: Value) -> Vec<Vec<u8>> {
    let mut responses = access();
    responses.extend([
        response(200, before),
        response(200, fixture()["raw_rules"].clone()),
        response(200, check),
        response(200, rules),
        response(200, after),
        revoked(),
    ]);
    responses
}

#[tokio::test]
async fn inspection_pins_read_permissions_issuer_sha_and_independent_policy_digest() {
    let t = target();
    let (c, host) = host(capture(
        pull(),
        runs(),
        fixture()["raw_rules"].clone(),
        pull(),
    ))
    .await;
    let observed = c
        .inspect_pull(&binding(), &t, || async { Ok(()) })
        .await
        .unwrap();
    let expected: InspectionReport = serde_json::from_value(fixture()["report"].clone()).unwrap();
    assert!(observed.report.selected_checks_passed(&t));
    assert_eq!(observed.report.rules_before, expected.rules_before);
    assert!(observed.report.token_revoked);
    let requests = host.await.unwrap();
    assert_eq!(requests.len(), 10);
    assert!(request_text(&requests[6]).starts_with(&format!("GET /repos/example-org/demo/commits/{}/check-runs?check_name=CI+%2F+build&app_id=7&filter=latest&per_page=2 ","11".repeat(20))));
    let body: Value =
        serde_json::from_str(request_text(&requests[1]).split("\r\n\r\n").nth(1).unwrap()).unwrap();
    assert_eq!(
        body,
        json!({"repository_ids":[11],"permissions":{"checks":"read","contents":"read","metadata":"read","pull_requests":"read"}})
    );
    assert!(
        requests[2..9]
            .iter()
            .all(|r| request_text(r).starts_with("GET "))
    );
    assert!(
        !serde_json::to_string(&observed.report)
            .unwrap()
            .contains(TEST_TOKEN)
    );
}

#[tokio::test]
async fn inspection_detects_head_base_and_rules_drift_without_carrying_success() {
    for field in ["head", "base", "rules", "closed", "already_moved"] {
        let mut before = pull();
        let mut after = pull();
        let mut rules = fixture()["raw_rules"].clone();
        match field {
            "head" => after["head"]["sha"] = json!("33".repeat(20)),
            "base" => after["base"]["sha"] = json!("33".repeat(20)),
            "rules" => {
                rules[0]["parameters"]["strict_required_status_checks_policy"] = json!(false)
            }
            "closed" => after["state"] = json!("closed"),
            _ => {
                before["head"]["sha"] = json!("33".repeat(20));
                after = before.clone();
            }
        }
        let (c, host) = host(capture(before, runs(), rules, after)).await;
        let result = c
            .inspect_pull(&binding(), &target(), || async { Ok(()) })
            .await
            .unwrap();
        assert!(!result.report.consistent(&target()), "{field}");
        assert!(!result.report.selected_checks_passed(&target()));
        assert_eq!(host.await.unwrap().len(), 10);
    }
}

#[tokio::test]
async fn inspection_missing_ambiguous_failed_or_rerunning_results_are_not_success() {
    for kind in ["missing", "ambiguous", "failed", "rerunning", "skipped"] {
        let mut check = runs();
        match kind {
            "missing" => check = json!({"total_count":0,"check_runs":[]}),
            "ambiguous" => {
                let mut second = check["check_runs"][0].clone();
                second["id"] = json!(20);
                check["check_runs"].as_array_mut().unwrap().push(second);
                check["total_count"] = json!(2);
            }
            "failed" => check["check_runs"][0]["conclusion"] = json!("failure"),
            "rerunning" => {
                check["check_runs"][0]["status"] = json!("queued");
                check["check_runs"][0]["conclusion"] = Value::Null;
            }
            _ => check["check_runs"][0]["conclusion"] = json!("skipped"),
        }
        let (c, host) = host(capture(
            pull(),
            check,
            fixture()["raw_rules"].clone(),
            pull(),
        ))
        .await;
        let result = c
            .inspect_pull(&binding(), &target(), || async { Ok(()) })
            .await
            .unwrap();
        assert!(result.report.consistent(&target()));
        assert!(!result.report.selected_checks_passed(&target()), "{kind}");
        host.await.unwrap();
    }
}

#[tokio::test]
async fn inspection_rejects_foreign_source_pr_and_substituted_check_identity() {
    for field in ["repository", "number", "node", "ref"] {
        let mut p = pull();
        match field {
            "repository" => p["base"]["repo"]["id"] = json!(12),
            "number" => p["number"] = json!(18),
            "node" => p["node_id"] = json!("PR_other"),
            _ => p["head"]["ref"] = json!("other"),
        }
        let mut responses = access();
        responses.extend([response(200, p), revoked()]);
        let (c, host) = host(responses).await;
        assert!(matches!(
            c.inspect_pull(&binding(), &target(), || async { Ok(()) })
                .await,
            Err(Error::Evidence)
        ));
        assert_eq!(host.await.unwrap().len(), 6);
    }
    for field in ["sha", "app", "name", "count"] {
        let mut check = runs();
        match field {
            "sha" => check["check_runs"][0]["head_sha"] = json!("33".repeat(20)),
            "app" => check["check_runs"][0]["app"]["id"] = json!(8),
            "name" => check["check_runs"][0]["name"] = json!("other"),
            _ => check["total_count"] = json!(2),
        }
        let (c, host) = host(capture(
            pull(),
            check,
            fixture()["raw_rules"].clone(),
            pull(),
        ))
        .await;
        assert!(matches!(
            c.inspect_pull(&binding(), &target(), || async { Ok(()) })
                .await,
            Err(Error::Evidence)
        ));
        host.await.unwrap();
    }
}

#[tokio::test]
async fn inspection_rules_pagination_detects_duplicates_and_encodes_base_as_one_segment() {
    let mut rules = vec![];
    for id in 1..=10 {
        rules.push(json!({"ruleset_id":id,"type":"merge_queue"}));
    }
    let mut p = pull();
    p["base"]["ref"] = json!("release/v1");
    let mut t = target();
    t.base_ref = "release/v1".into();
    let mut responses = access();
    responses.extend([
        response(200, p),
        response(200, json!(rules)),
        response(200, json!([rules[0]])),
        revoked(),
    ]);
    let (c, host) = host(responses).await;
    assert!(matches!(
        c.inspect_pull(&binding(), &t, || async { Ok(()) }).await,
        Err(Error::Evidence)
    ));
    let requests = host.await.unwrap();
    assert!(request_text(&requests[5]).contains("rules/branches/release%2Fv1?per_page=10&page=1"));
    assert!(request_text(&requests[6]).contains("page=2"));
}

#[tokio::test]
async fn inspection_guards_and_revocation_failure_preserve_the_read_only_boundary() {
    for stop in [1, 2] {
        let mut responses = if stop == 1 {
            vec![]
        } else {
            capture(pull(), runs(), fixture()["raw_rules"].clone(), pull())
        };
        if stop == 2 {
            responses.pop();
            responses.push(revoked());
        }
        let (c, host) = host(responses).await;
        let mut calls = 0;
        let result = c
            .inspect_pull(&binding(), &target(), || {
                calls += 1;
                std::future::ready(if calls == stop {
                    Err(Error::PublicationStopped)
                } else {
                    Ok(())
                })
            })
            .await;
        assert!(matches!(result, Err(Error::PublicationStopped)));
        assert_eq!(host.await.unwrap().len(), if stop == 1 { 0 } else { 10 });
    }
    let mut responses = capture(pull(), runs(), fixture()["raw_rules"].clone(), pull());
    responses.pop();
    responses.push(response(500, json!({"message":"revoke failed"})));
    let (c, host) = host(responses).await;
    let observed = c
        .inspect_pull(&binding(), &target(), || async { Ok(()) })
        .await
        .unwrap();
    assert!(!observed.report.token_revoked);
    assert!(observed.report.selected_checks_passed(&target()));
    host.await.unwrap();
}
