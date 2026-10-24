use super::*;
#[path = "dequeue.rs"]
mod dequeue_tests;
use treeharbor_github_contract::queue::QueueTarget;
fn fixture() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/inspection-v1.json"
    )))
    .unwrap()
}
fn target() -> QueueTarget {
    let f = fixture();
    QueueTarget {
        pull: serde_json::from_value(f["target"].clone()).unwrap(),
        expected_base_revision: "git:sha1:".to_owned() + &"22".repeat(20),
        expected_rules: serde_json::from_value(f["report"]["rules_before"].clone()).unwrap(),
    }
}
fn access(write: bool) -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["checks"] = json!("read");
    t["permissions"]["merge_queues"] = json!("read");
    if write {
        t["permissions"]["contents"] = json!("write");
    }
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn graph() -> Value {
    let t = target();
    json!({"data":{"repository":{"databaseId":11,"pullRequest":{
        "id":t.pull.pull_node,"number":t.pull.pull_number,"headRefName":t.pull.head_ref,"headRefOid":"11".repeat(20),"baseRefName":"main","baseRefOid":"22".repeat(20),"state":"OPEN","isDraft":false,"merged":false,"headRepository":{"databaseId":11},"baseRepository":{"databaseId":11},"isMergeQueueEnabled":true,"isInMergeQueue":false,"mergeCommit":null,
        "mergeQueue":{"id":"MQ_one","repository":{"databaseId":11},"configuration":{"mergeMethod":"SQUASH","mergingStrategy":"ALLGREEN","maximumEntriesToBuild":5,"maximumEntriesToMerge":5,"minimumEntriesToMerge":1,"minimumEntriesToMergeWaitTime":0,"checkResponseTimeout":60}},"mergeQueueEntry":null
    }}}})
}
fn queued() -> Value {
    let mut g = graph();
    let p = &mut g["data"]["repository"]["pullRequest"];
    p["isInMergeQueue"] = json!(true);
    p["mergeQueueEntry"] = json!({"id":"MQE_one","state":"AWAITING_CHECKS","baseCommit":{"oid":"22".repeat(20),"tree":{"oid":"55".repeat(20)}},"headCommit":{"oid":"33".repeat(20),"tree":{"oid":"66".repeat(20)}},"mergeQueue":{"id":"MQ_one"},"pullRequest":{"id":target().pull.pull_node,"number":17,"headRefOid":"11".repeat(20)}});
    g
}
fn inspection() -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["checks"] = json!("read");
    let p = json!({"number":17,"node_id":target().pull.pull_node,"head":{"ref":target().pull.head_ref,"sha":"11".repeat(20),"repo":{"id":11}},"base":{"ref":"main","sha":"22".repeat(20),"repo":{"id":11}},"state":"open","draft":false,"merged":false});
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
        response(200, p.clone()),
        response(200, fixture()["raw_rules"].clone()),
        response(
            200,
            json!({"total_count":1,"check_runs":[{"id":19,"app":{"id":7},"name":"CI / build","head_sha":"11".repeat(20),"status":"completed","conclusion":"success"}]}),
        ),
        response(200, fixture()["raw_rules"].clone()),
        response(200, p),
        revoked(),
    ]
}
fn pending() -> Value {
    json!({"status":"pending","details":{"uuid":"a15d30c6-3af7-4191-9a68-600000000001","merge_action":"merge_queue","expected_head_sha":"11".repeat(20)}})
}
fn delivery(before: Value, after: Value, result: Vec<u8>) -> Vec<Vec<u8>> {
    let mut v = inspection();
    v.extend(access(true));
    v.extend([
        response(200, before),
        response(200, fixture()["raw_rules"].clone()),
        response(200, after),
        result,
        revoked(),
    ]);
    v
}
#[tokio::test]
async fn queue_sends_exact_native_action_once_and_returns_receipt_before_more_reads() {
    let t = target();
    let (c, h) = host(delivery(graph(), graph(), response(202, pending()))).await;
    let v = c
        .enqueue_pull(&binding(), &t, || async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(v.observation.state(&t), "host_pending");
    assert!(v.observation.snapshot.is_none());
    assert!(v.observation.token_revoked);
    let req = h.await.unwrap();
    assert_eq!(req.len(), 19);
    let put: Vec<_> = req
        .iter()
        .filter(|r| request_text(r).starts_with("PUT "))
        .collect();
    assert_eq!(put.len(), 1);
    let (headers, body) = request_text(put[0])
        .split_once("\r\n\r\n")
        .map(|(a, b)| (a.to_owned(), b.to_owned()))
        .unwrap();
    assert!(headers.starts_with("PUT /repos/example-org/demo/pulls/17/merge-async "));
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap(),
        json!({"sha":"11".repeat(20),"merge_action":"merge_queue","bypass_rules":false})
    );
    let mint: Value =
        serde_json::from_str(request_text(&req[11]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        mint["permissions"],
        json!({"contents":"write","metadata":"read","pull_requests":"read","checks":"read","merge_queues":"read"})
    );
    assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
}
#[tokio::test]
async fn queue_lost_or_foreign_response_never_retries_and_observation_is_read_scoped() {
    for result in [
        Vec::new(),
        response(
            409,
            json!({"status":"pending","details":{"uuid":"a15d30c6-3af7-4191-9a68-600000000001","merge_action":"direct_merge","expected_head_sha":"11".repeat(20),"bypass_rules":true}}),
        ),
    ] {
        let (c, h) = host(delivery(graph(), graph(), result)).await;
        let v = c
            .enqueue_pull(&binding(), &target(), || async { Ok(()) })
            .await
            .unwrap();
        assert!(v.observation.receipt.is_none());
        assert_eq!(v.observation.state(&target()), "uncertain");
        assert_eq!(
            h.await
                .unwrap()
                .iter()
                .filter(|r| request_text(r).starts_with("PUT "))
                .count(),
            1
        );
    }
    let mut responses = access(false);
    responses.extend([response(200, queued()), revoked()]);
    let (c, h) = host(responses).await;
    let v = c.observe_queue(&binding(), &target()).await.unwrap();
    assert_eq!(v.observation.state(&target()), "validating");
    assert_eq!(
        v.observation
            .snapshot
            .unwrap()
            .entry
            .unwrap()
            .head
            .unwrap()
            .revision,
        "git:sha1:".to_owned() + &"33".repeat(20)
    );
    let req = h.await.unwrap();
    assert_eq!(req.len(), 6);
    assert!(!req.iter().any(|r| request_text(r).starts_with("PUT ")));
    let mint: Value =
        serde_json::from_str(request_text(&req[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(mint["permissions"]["contents"], "read");
}
#[tokio::test]
async fn queue_rejects_changed_base_disabled_queue_and_graphql_errors_before_put() {
    for field in ["head", "base", "method", "queue", "errors"] {
        let before = graph();
        let mut after = graph();
        let p = &mut after["data"]["repository"]["pullRequest"];
        match field {
            "head" => p["headRefOid"] = json!("33".repeat(20)),
            "base" => p["baseRefOid"] = json!("33".repeat(20)),
            "method" => p["mergeQueue"]["configuration"]["mergeMethod"] = json!("REBASE"),
            "queue" => p["isMergeQueueEnabled"] = json!(false),
            _ => after["errors"] = json!([{"message":"untrusted secret"}]),
        }
        let mut v = inspection();
        v.extend(access(true));
        v.extend([
            response(200, before),
            response(200, fixture()["raw_rules"].clone()),
            response(200, after),
            revoked(),
        ]);
        let (c, h) = host(v).await;
        let failure = c
            .enqueue_pull_attempt(&binding(), &target(), || async { Ok(()) })
            .await
            .err()
            .unwrap();
        assert!(matches!(failure.error, Error::Queue));
        assert!(failure.mutation_not_sent);
        assert!(
            !h.await
                .unwrap()
                .iter()
                .any(|r| request_text(r).starts_with("PUT ")),
            "{field}"
        );
    }
}
#[tokio::test]
async fn queue_rejects_foreign_group_membership_and_never_confirms_final_tree() {
    for field in ["repository", "entry", "head", "queue"] {
        let mut g = queued();
        let p = &mut g["data"]["repository"]["pullRequest"];
        match field {
            "repository" => p["headRepository"]["databaseId"] = json!(12),
            "entry" => p["mergeQueueEntry"]["pullRequest"]["id"] = json!("PR_foreign"),
            "head" => p["mergeQueueEntry"]["pullRequest"]["headRefOid"] = json!("88".repeat(20)),
            _ => p["mergeQueueEntry"]["mergeQueue"]["id"] = json!("MQ_foreign"),
        }
        let mut v = access(false);
        v.extend([response(200, g), revoked()]);
        let (c, h) = host(v).await;
        assert!(matches!(
            c.observe_queue(&binding(), &target()).await,
            Err(Error::Queue)
        ));
        h.await.unwrap();
    }
    let mut g = graph();
    let p = &mut g["data"]["repository"]["pullRequest"];
    p["state"] = json!("MERGED");
    p["merged"] = json!(true);
    p["mergeCommit"] = json!({"oid":"33".repeat(20),"tree":{"oid":"66".repeat(20)}});
    let mut v = access(false);
    v.extend([response(200, g), revoked()]);
    let (c, h) = host(v).await;
    let r = c.observe_queue(&binding(), &target()).await.unwrap();
    assert_eq!(r.observation.state(&target()), "host_merged_unverified");
    h.await.unwrap();
}

fn result_access() -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["contents"] = json!("write");
    let mut r = repository();
    r["archived"] = json!(true); // historical recovery must still be possible
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, r),
    ]
}
#[tokio::test]
async fn queue_result_reads_retained_uuid_without_mutation_and_scopes_required_write_token() {
    let vectors: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/queue-result-v1.json"
    )))
    .unwrap();
    let uuid = vectors["request_uuid"].as_str().unwrap();
    for case in vectors["cases"].as_array().unwrap() {
        let mut responses = result_access();
        responses.extend([
            response(
                case["http_status"].as_u64().unwrap() as u16,
                case["body"].clone(),
            ),
            revoked(),
        ]);
        let (c, h) = host(responses).await;
        let r = c.queue_result(&binding(), &target(), uuid).await.unwrap();
        let v = &r.observation;
        assert_eq!(json!(v.state(&target())), case["state"]);
        assert_eq!(
            json!(v.async_result.as_ref().unwrap().outcome),
            case["outcome"]
        );
        assert_eq!(v.async_result.as_ref().unwrap().request_uuid, uuid);
        assert!(v.token_revoked && v.snapshot.is_none() && v.receipt.is_none());
        assert!(
            !serde_json::to_string(v)
                .unwrap()
                .contains("Provider diagnostic")
        );
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 6);
        assert!(request_text(&requests[4]).starts_with(&format!(
            "GET /repos/example-org/demo/pulls/17/merge-async/{uuid} "
        )));
        let mint: Value =
            serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1)
                .unwrap();
        assert_eq!(
            mint["permissions"],
            json!({"contents":"write","metadata":"read"})
        );
        assert!(!requests.iter().any(|r| request_text(r).starts_with("PUT ")));
        assert!(request_text(&requests[5]).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn queue_result_rejects_foreign_or_incomplete_identity_and_never_guesses_failure() {
    let uuid = "a15d30c6-3af7-4191-9a68-600000000001";
    for field in [
        "uuid",
        "head",
        "action",
        "bypass",
        "absent-bypass",
        "status",
        "sha",
        "forbidden",
        "network",
    ] {
        let mut p = pending();
        p["details"]["bypass_rules"] = json!(false);
        let mut status = 200;
        match field {
            "uuid" => p["details"]["uuid"] = json!("a15d30c6-3af7-4191-9a68-600000000002"),
            "head" => p["details"]["expected_head_sha"] = json!("99".repeat(20)),
            "action" => p["details"]["merge_action"] = json!("direct_merge"),
            "bypass" => p["details"]["bypass_rules"] = json!(true),
            "absent-bypass" => p["details"]["bypass_rules"] = Value::Null,
            "status" => p["status"] = json!("future-status"),
            "sha" => p["details"]["sha"] = json!("33".repeat(20)),
            "forbidden" => status = 403,
            _ => (),
        }
        let mut responses = result_access();
        responses.extend([
            if field == "network" {
                Vec::new()
            } else {
                response(status, p)
            },
            revoked(),
        ]);
        let (c, h) = host(responses).await;
        assert!(
            c.queue_result(&binding(), &target(), uuid).await.is_err(),
            "{field}"
        );
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 6);
        assert!(request_text(&requests[5]).starts_with("DELETE /installation/token "));
    }
}
