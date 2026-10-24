use super::*;
use treeharbor_github_contract::group_attempts::Target;
const RUN: u64 = 40000000001;
const CHECK: u64 = 120000000001;
const SUITE: u64 = 108000000001;
fn selected() -> Target {
    let v: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/group-attempt-v6.json"
    ))
    .unwrap();
    serde_json::from_value(v["group_attempt"]["target"].clone()).unwrap()
}
fn ids() -> Value {
    json!({"data":{"workflow":{"__typename":"WorkflowRun","id":"WFR_merge_group","databaseId":RUN,
        "workflow":{"id":"WF_checks","databaseId":300000001},"checkSuite":{"id":"CS_merge_group","databaseId":SUITE,"repository":{"databaseId":11},"commit":{"oid":"33".repeat(20)}}},
        "check":{"__typename":"CheckRun","id":"CR_merge_group","databaseId":CHECK,"name":"CI / build","repository":{"databaseId":11},"checkSuite":{"id":"CS_merge_group","databaseId":SUITE,"commit":{"oid":"33".repeat(20)}}}}})
}
fn attempt() -> Value {
    json!({"id":RUN,"node_id":"WFR_merge_group","workflow_id":300000001,"check_suite_id":SUITE,"check_suite_node_id":"CS_merge_group","repository":{"id":11},"head_sha":"33".repeat(20),"run_attempt":1,"event":"merge_group","status":"completed","conclusion":"success","path":".github/workflows/check.yml","referenced_workflows":[]})
}
fn jobs() -> Value {
    json!({"total_count":1,"jobs":[{"id":71000000001u64,"node_id":"JOB_selected","run_id":RUN,"run_attempt":1,"head_sha":"33".repeat(20),"check_run_url":format!("https://api.github.com/repos/example-org/demo/check-runs/{CHECK}"),"name":"CI / build","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:00:01Z"}]})
}
fn check() -> Value {
    json!({"id":CHECK,"node_id":"CR_merge_group","app":{"id":7},"check_suite":{"id":SUITE},"head_sha":"33".repeat(20),"name":"CI / build","status":"completed","conclusion":"success","started_at":"2026-01-01T00:00:00Z","completed_at":"2026-01-01T00:00:01Z"})
}
fn access() -> Vec<Vec<u8>> {
    let mut r = good_responses();
    r.pop();
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["checks"] = json!("read");
    t["permissions"]["actions"] = json!("read");
    r[1] = response(201, t);
    r
}
fn replies() -> Vec<Vec<u8>> {
    let mut r = access();
    r.push(response(200, ids()));
    for _ in 0..2 {
        r.extend([
            response(200, attempt()),
            response(200, jobs()),
            response(200, check()),
            response(200, attempt()),
        ]);
    }
    r.push(revoked());
    r
}
#[tokio::test]
async fn exact_attempt_transport_uses_narrow_actions_read_and_independent_job_check_mapping() {
    let (c, h) = host(replies()).await;
    let r = c
        .observe_group_attempt(&binding(), &target(), &selected())
        .await
        .unwrap();
    assert!(r.observation.token_revoked);
    let a = r.observation.group_attempt.as_ref().unwrap();
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/group-attempt-v6.json"
    ))
    .unwrap();
    assert_eq!(json!(a), golden["group_attempt"]);
    assert!(a.attempt_linked(&target(), r.observation.finished_at_unix_ms));
    assert!(a.current_attempt_matches());
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 14);
    let mint: Value =
        serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        mint["permissions"],
        json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read","actions":"read"})
    );
    for i in [5, 9] {
        assert!(request_text(&requests[i]).starts_with(&format!(
            "GET /repos/example-org/demo/actions/runs/{RUN}/attempts/1 "
        )));
    }
    for i in [6, 10] {
        assert!(request_text(&requests[i]).starts_with(&format!(
            "GET /repos/example-org/demo/actions/runs/{RUN}/attempts/1/jobs?per_page=100 "
        )));
    }
    for i in [7, 11] {
        assert!(
            request_text(&requests[i])
                .starts_with(&format!("GET /repos/example-org/demo/check-runs/{CHECK} "))
        );
    }
}
#[tokio::test]
async fn attempt_rejects_foreign_opaque_numeric_repository_and_suite_resolution_before_rest() {
    for case in [
        "type", "node", "workflow", "repo", "suite", "check", "head", "zero", "errors",
    ] {
        let mut g = ids();
        match case {
            "type" => g["data"]["workflow"]["__typename"] = json!("CheckSuite"),
            "node" => g["data"]["workflow"]["id"] = json!("WFR_other"),
            "workflow" => g["data"]["workflow"]["workflow"]["id"] = json!("WF_other"),
            "repo" => g["data"]["check"]["repository"]["databaseId"] = json!(12),
            "suite" => g["data"]["check"]["checkSuite"]["databaseId"] = json!(7),
            "check" => g["data"]["check"]["id"] = json!("CR_other"),
            "head" => g["data"]["workflow"]["checkSuite"]["commit"]["oid"] = json!("ab".repeat(20)),
            "zero" => g["data"]["workflow"]["databaseId"] = json!(0),
            "errors" => g["errors"] = json!([{"message":"incomplete"}]),
            _ => unreachable!(),
        }
        let mut r = access();
        r.extend([response(200, g), revoked()]);
        let (c, h) = host(r).await;
        assert!(
            c.observe_group_attempt(&binding(), &target(), &selected())
                .await
                .is_err(),
            "{case}"
        );
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 6);
        assert!(request_text(&requests[5]).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn attempt_rejects_job_truncation_foreign_urls_and_denied_actions_without_retry() {
    for case in [
        "count",
        "overflow",
        "link",
        "foreign-url",
        "suffix",
        "denied",
    ] {
        let mut r = access();
        r.extend([response(200, ids()), response(200, attempt())]);
        let mut j = jobs();
        let answer = match case {
            "count" => {
                j["total_count"] = json!(2);
                response(200, j)
            }
            "overflow" => {
                j["total_count"] = json!(101);
                j["jobs"] = json!(vec![j["jobs"][0].clone(); 101]);
                response(200, j)
            }
            "link" => raw(
                200,
                "Content-Type: application/json\r\nLink: <https://example.com/private>; rel=\"next\"\r\n",
                &serde_json::to_vec(&j).unwrap(),
            ),
            "foreign-url" => {
                j["jobs"][0]["check_run_url"] =
                    json!("https://api.github.com/repos/foreign/demo/check-runs/1");
                response(200, j)
            }
            "suffix" => {
                j["jobs"][0]["check_run_url"] = json!(format!(
                    "https://api.github.com/repos/example-org/demo/check-runs/{CHECK}?token=bad"
                ));
                response(200, j)
            }
            "denied" => response(403, json!({"message":"not permitted"})),
            _ => unreachable!(),
        };
        r.extend([answer, revoked()]);
        let (c, h) = host(r).await;
        assert!(
            c.observe_group_attempt(&binding(), &target(), &selected())
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(h.await.unwrap().len(), 8);
    }
}
#[tokio::test]
async fn attempt_preserves_later_rerun_and_missing_job_without_promoting_old_success() {
    for missing in [false, true] {
        let mut r = replies();
        if missing {
            for i in [6, 10] {
                r[i] = response(200, json!({"total_count":0,"jobs":[]}));
            }
        } else {
            let mut newer = attempt();
            newer["run_attempt"] = json!(2);
            newer["conclusion"] = json!("failure");
            for i in [8, 12] {
                r[i] = response(200, newer.clone());
            }
        }
        let (c, h) = host(r).await;
        let o = c
            .observe_group_attempt(&binding(), &target(), &selected())
            .await
            .unwrap();
        let a = o.observation.group_attempt.unwrap();
        if missing {
            assert!(!a.attempt_linked(&target(), o.observation.finished_at_unix_ms));
        } else {
            assert!(a.attempt_linked(&target(), o.observation.finished_at_unix_ms));
            assert!(!a.current_attempt_matches());
        }
        h.await.unwrap();
    }
}
