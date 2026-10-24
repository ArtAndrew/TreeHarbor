use super::*;
use uuid::Uuid;

fn body(request: &[u8]) -> Value {
    serde_json::from_str(request_text(request).split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn queue_read_omits_write_protected_settings_but_checks_queue_identity() {
    for foreign in [false, true] {
        let mut g = queued();
        let q = &mut g["data"]["repository"]["pullRequest"]["mergeQueue"];
        q.as_object_mut().unwrap().remove("configuration");
        if foreign {
            q["id"] = json!("MQ_foreign");
        }
        let mut replies = access(false);
        replies.extend([response(200, g), revoked()]);
        let (c, h) = host(replies).await;
        let result = c.observe_queue(&binding(), &target()).await;
        if foreign {
            assert!(result.is_err());
        } else {
            let o = result.unwrap().observation;
            assert_eq!(o.version, 3);
            assert!(o.token_revoked);
            assert_eq!(o.state(&target()), "validating");
            let s = o.snapshot.unwrap();
            assert!(s.configuration.is_none() && !s.can_request(&target()));
            assert_eq!(s.queue_id.as_deref(), Some("MQ_one"));
        }
        let requests = h.await.unwrap();
        assert_eq!(body(&requests[4])["variables"]["configuration"], false);
        assert_eq!(body(&requests[1])["permissions"]["contents"], "read");
        assert_eq!(body(&requests[1])["permissions"]["merge_queues"], "read");
        assert!(
            body(&requests[4])["query"]
                .as_str()
                .unwrap()
                .contains("configuration @include(if:$configuration)")
        );
        assert_eq!(requests.len(), 6);
    }
}

fn acknowledgment(id: Uuid) -> Value {
    json!({"data":{"enqueuePullRequest":{"clientMutationId":id.to_string(),"mergeQueueEntry":{
        "id":"MQE_one","mergeQueue":{"id":"MQ_one","repository":{"databaseId":11}},
        "pullRequest":{"id":target().pull.pull_node,"number":17,"headRefOid":"11".repeat(20),
        "baseRepository":{"databaseId":11},"headRepository":{"databaseId":11}}
    }}}})
}
fn graphql_delivery(result: Vec<u8>) -> Vec<Vec<u8>> {
    let mut v = delivery(graph(), graph(), result);
    let mut token = token();
    token["permissions"] = json!({"contents":"write","metadata":"read","pull_requests":"read","checks":"read","merge_queues":"write"});
    v[11] = response(201, token);
    v
}
#[tokio::test]
async fn graphql_enqueue_pins_head_attempt_and_queue_without_direct_merge_or_retry() {
    let id = Uuid::new_v4();
    let (c, h) = host(graphql_delivery(response(200, acknowledgment(id)))).await;
    let result = c
        .enqueue_pull_graphql_attempt(&binding(), &target(), id, || async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(result.observation.version, 3);
    assert_eq!(result.observation.state(&target()), "host_enqueued");
    assert!(result.observation.receipt.is_none() && result.observation.async_result.is_none());
    assert!(result.observation.token_revoked);
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 19);
    assert_eq!(
        body(&requests[17])["variables"],
        json!({"id":target().pull.pull_node,"head":"11".repeat(20),"request":id.to_string()})
    );
    let query = body(&requests[17])["query"].as_str().unwrap().to_owned();
    assert!(
        query.contains("enqueuePullRequest")
            && query.contains("expectedHeadOid:$head")
            && query.contains("jump:false")
    );
    assert!(!query.contains("mergePullRequest") && !query.contains("bypass"));
    assert!(request_text(&requests[17]).starts_with("POST /graphql "));
    assert!(!requests.iter().any(|r| request_text(r).starts_with("PUT ")));
    assert_eq!(body(&requests[11])["permissions"]["merge_queues"], "write");
}
#[tokio::test]
async fn graphql_enqueue_rejects_wrong_identity_partial_errors_and_lost_responses() {
    let id = Uuid::new_v4();
    for fault in [
        "head",
        "base-repo",
        "head-repo",
        "queue-repo",
        "queue",
        "pull",
        "number",
        "request",
        "errors",
        "lost",
    ] {
        let mut v = acknowledgment(id);
        let p = &mut v["data"]["enqueuePullRequest"];
        match fault {
            "head" => p["mergeQueueEntry"]["pullRequest"]["headRefOid"] = json!("22".repeat(20)),
            "base-repo" => {
                p["mergeQueueEntry"]["pullRequest"]["baseRepository"]["databaseId"] = json!(12)
            }
            "head-repo" => {
                p["mergeQueueEntry"]["pullRequest"]["headRepository"]["databaseId"] = json!(12)
            }
            "queue-repo" => {
                p["mergeQueueEntry"]["mergeQueue"]["repository"]["databaseId"] = json!(12)
            }
            "queue" => p["mergeQueueEntry"]["mergeQueue"]["id"] = json!("MQ_other"),
            "pull" => p["mergeQueueEntry"]["pullRequest"]["id"] = json!("PR_other"),
            "number" => p["mergeQueueEntry"]["pullRequest"]["number"] = json!(18),
            "request" => p["clientMutationId"] = json!(Uuid::new_v4().to_string()),
            "errors" => v["errors"] = json!([{"message":"unavailable"}]),
            _ => (),
        }
        let bytes = if fault == "lost" {
            Vec::new()
        } else {
            response(200, v)
        };
        let (c, h) = host(graphql_delivery(bytes)).await;
        let result = c
            .enqueue_pull_graphql_attempt(&binding(), &target(), id, || async { Ok(()) })
            .await
            .unwrap();
        assert!(result.observation.enqueue_receipt.is_none(), "{fault}");
        assert_eq!(result.observation.state(&target()), "uncertain", "{fault}");
        assert!(!result.observation.mutation_not_sent);
        assert_eq!(h.await.unwrap().len(), 19, "{fault}");
    }
}
