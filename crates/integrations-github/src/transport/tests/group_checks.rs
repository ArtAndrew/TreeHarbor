use super::*;
use treeharbor_github_contract::group_checks::GroupCheckTarget;
fn selected() -> GroupCheckTarget {
    let v: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/group-checks-v5.json"
    ))
    .unwrap();
    serde_json::from_value(v["group_checks"]["target"].clone()).unwrap()
}
fn checks() -> Value {
    let mut nodes = Vec::new();
    for event in ["merge_group", "push"] {
        nodes.push(json!({"id":format!("CS_{event}"),"app":{"databaseId":7},"repository":{"databaseId":11},"commit":{"oid":"33".repeat(20)},
            "workflowRun":{"id":format!("WFR_{event}"),"workflow":{"id":"WF_checks"},"event":event,"runAttempt":1,
            "file":{"path":".github/workflows/check.yml","repositoryName":"example-org/demo","repositoryFileUrl":format!("https://github.com/example-org/demo/blob/{}/.github/workflows/check.yml","33".repeat(20))}},
            "checkRuns":{"totalCount":1,"pageInfo":{"hasNextPage":false,"hasPreviousPage":false},"nodes":[{"id":format!("CR_{event}"),"name":"CI / build","status":"COMPLETED","conclusion":"SUCCESS","startedAt":"2026-01-01T00:00:00Z","completedAt":"2026-01-01T00:00:01Z"}]}}));
    }
    json!({"data":{"repository":{"databaseId":11,"object":{"oid":"33".repeat(20),"tree":{"oid":"66".repeat(20)},"checkSuites":{"totalCount":2,"pageInfo":{"hasNextPage":false,"hasPreviousPage":false},"nodes":nodes}}}}})
}
fn access() -> Vec<Vec<u8>> {
    let mut r = good_responses();
    r.pop();
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["checks"] = json!("read");
    r[1] = response(201, t);
    r
}
#[tokio::test]
async fn group_checks_pin_commit_and_preserve_push_duplicates_without_new_permissions() {
    let mut replies = access();
    replies.extend([response(200, checks()), response(200, checks()), revoked()]);
    let (c, h) = host(replies).await;
    let v = c
        .observe_group_checks(&binding(), &target(), &selected())
        .await
        .unwrap();
    let checks = v.observation.group_checks.as_ref().unwrap();
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/group-checks-v5.json"
    ))
    .unwrap();
    assert_eq!(json!(checks), golden["group_checks"]);
    assert!(v.observation.token_revoked);
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 7);
    for i in [4, 5] {
        let v: Value =
            serde_json::from_str(request_text(&requests[i]).split_once("\r\n\r\n").unwrap().1)
                .unwrap();
        assert_eq!(v["variables"]["oid"], "33".repeat(20));
        assert!(v["query"].as_str().unwrap().contains("checkType:ALL"));
        assert!(!v["query"].as_str().unwrap().contains("mutation"));
    }
    let mint: Value =
        serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        mint["permissions"],
        json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read"})
    );
}
#[tokio::test]
async fn group_checks_reject_incomplete_pages_foreign_subjects_and_invalid_provenance() {
    for case in [
        "page",
        "previous",
        "count",
        "null",
        "subject",
        "tree",
        "repo",
        "suite-repo",
        "suite-sha",
        "runs-page",
        "runs-count",
        "duplicate",
        "timestamp",
        "url",
        "errors",
    ] {
        let mut g = checks();
        let r = &mut g["data"]["repository"];
        match case {
            "subject" => r["object"]["oid"] = json!("ab".repeat(20)),
            "tree" => r["object"]["tree"]["oid"] = json!("ab".repeat(20)),
            "repo" => r["databaseId"] = json!(12),
            _ => {
                let s = &mut r["object"]["checkSuites"];
                match case {
                    "page" => s["pageInfo"]["hasNextPage"] = json!(true),
                    "previous" => s["pageInfo"]["hasPreviousPage"] = json!(true),
                    "count" => s["totalCount"] = json!(3),
                    "null" => s["nodes"][0] = Value::Null,
                    "suite-repo" => s["nodes"][0]["repository"]["databaseId"] = json!(12),
                    "suite-sha" => s["nodes"][0]["commit"]["oid"] = json!("ab".repeat(20)),
                    "runs-page" => {
                        s["nodes"][0]["checkRuns"]["pageInfo"]["hasNextPage"] = json!(true)
                    }
                    "runs-count" => s["nodes"][0]["checkRuns"]["totalCount"] = json!(2),
                    "duplicate" => {
                        s["nodes"][1]["checkRuns"]["nodes"][0]["id"] =
                            s["nodes"][0]["checkRuns"]["nodes"][0]["id"].clone()
                    }
                    "timestamp" => {
                        s["nodes"][0]["checkRuns"]["nodes"][0]["completedAt"] = json!("not-a-time")
                    }
                    "url" => {
                        s["nodes"][0]["workflowRun"]["file"]["repositoryFileUrl"] =
                            json!("https://example.com/a")
                    }
                    "errors" => g["errors"] = json!([{"message":"incomplete"}]),
                    _ => unreachable!(),
                }
            }
        }
        let mut replies = access();
        replies.extend([response(200, g), revoked()]);
        let (c, h) = host(replies).await;
        assert!(
            c.observe_group_checks(&binding(), &target(), &selected())
                .await
                .is_err(),
            "{case}"
        );
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 6);
        assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn group_checks_retain_empty_results_and_observed_rerun_without_old_green_assurance() {
    let mut changed = checks();
    changed["data"]["repository"]["object"]["checkSuites"]["nodes"][0]["workflowRun"]["runAttempt"] =
        json!(2);
    let mut replies = access();
    replies.extend([response(200, checks()), response(200, changed), revoked()]);
    let (c, h) = host(replies).await;
    let v = c
        .observe_group_checks(&binding(), &target(), &selected())
        .await
        .unwrap();
    assert!(!v.observation.group_checks.unwrap().checks.consistent(11));
    h.await.unwrap();
    let mut empty = checks();
    let s = &mut empty["data"]["repository"]["object"]["checkSuites"];
    s["totalCount"] = json!(0);
    s["nodes"] = json!([]);
    let mut replies = access();
    replies.extend([
        response(200, empty.clone()),
        response(200, empty),
        revoked(),
    ]);
    let (c, h) = host(replies).await;
    let v = c
        .observe_group_checks(&binding(), &target(), &selected())
        .await
        .unwrap();
    assert!(
        v.observation
            .group_checks
            .as_ref()
            .unwrap()
            .checks
            .before
            .suites
            .is_empty()
    );
    assert_eq!(v.observation.state(&target()), "uncertain");
    h.await.unwrap();
}
