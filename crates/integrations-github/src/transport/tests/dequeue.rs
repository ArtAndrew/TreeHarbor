use super::*;
use uuid::Uuid;
fn vectors() -> Value {
    serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/dequeue-v1.json"
    )))
    .unwrap()
}
fn id() -> Uuid {
    vectors()["request_id"].as_str().unwrap().parse().unwrap()
}
fn remove_access() -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["contents"] = json!("write");
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["merge_queues"] = json!("write");
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn responses(body: Vec<u8>) -> Vec<Vec<u8>> {
    let mut v = remove_access();
    v.extend([response(200, queued()), body, revoked()]);
    v
}
#[tokio::test]
async fn dequeue_targets_exact_pr_once_and_returns_correlated_receipt_before_further_reads() {
    let (c, h) = host(responses(response(
        200,
        vectors()["provider_response"].clone(),
    )))
    .await;
    let r = c
        .dequeue_pull(&binding(), &target(), id(), || async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(json!(r.observation.receipt), vectors()["receipt"]);
    assert!(r.observation.after.is_none());
    assert!(r.observation.token_revoked);
    assert_eq!(
        r.observation.state(&target(), id(), true),
        "dequeue_acknowledged"
    );
    let req = h.await.unwrap();
    assert_eq!(req.len(), 7);
    let body: Value =
        serde_json::from_str(request_text(&req[5]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(
        body["query"]
            .as_str()
            .unwrap()
            .contains("dequeuePullRequest")
    );
    assert_eq!(
        body["variables"],
        json!({"id":target().pull.pull_node,"request":id().to_string()})
    );
    let token: Value =
        serde_json::from_str(request_text(&req[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        token["permissions"],
        json!({"contents":"write","metadata":"read","pull_requests":"read","merge_queues":"write"})
    );
    assert!(request_text(&req[6]).starts_with("DELETE /installation/token "));
}
#[tokio::test]
async fn dequeue_lost_or_foreign_receipt_is_uncertain_and_never_retried() {
    for field in [
        "network",
        "graphql",
        "request",
        "repository",
        "head-repository",
        "pull",
        "number",
    ] {
        let mut g = vectors()["provider_response"].clone();
        let p = &mut g["data"]["dequeuePullRequest"];
        match field {
            "request" => p["clientMutationId"] = json!(Uuid::new_v4().to_string()),
            "repository" => {
                p["mergeQueueEntry"]["mergeQueue"]["repository"]["databaseId"] = json!(12)
            }
            "head-repository" => {
                p["mergeQueueEntry"]["pullRequest"]["headRepository"]["databaseId"] = json!(12)
            }
            "pull" => p["mergeQueueEntry"]["pullRequest"]["id"] = json!("PR_foreign"),
            "number" => p["mergeQueueEntry"]["pullRequest"]["number"] = json!(18),
            "graphql" => g["errors"] = json!([{"message":"untrusted diagnostic"}]),
            _ => (),
        }
        let (c, h) = host(responses(if field == "network" {
            Vec::new()
        } else {
            response(200, g)
        }))
        .await;
        let r = c
            .dequeue_pull(&binding(), &target(), id(), || async { Ok(()) })
            .await
            .unwrap();
        assert!(r.observation.receipt.is_none(), "{field}");
        assert!(!r.observation.mutation_not_sent, "{field}");
        assert_eq!(r.observation.state(&target(), id(), true), "uncertain");
        assert_eq!(h.await.unwrap().len(), 7);
    }
}
#[tokio::test]
async fn dequeue_records_entry_races_without_claiming_atomic_head_comparison() {
    let mut g = vectors()["provider_response"].clone();
    let e = &mut g["data"]["dequeuePullRequest"]["mergeQueueEntry"];
    e["id"] = json!("MQE_replaced");
    e["pullRequest"]["headRefOid"] = json!("88".repeat(20));
    let (c, h) = host(responses(response(200, g))).await;
    let r = c
        .dequeue_pull(&binding(), &target(), id(), || async { Ok(()) })
        .await
        .unwrap();
    assert!(r.observation.changed_during_dequeue());
    assert_eq!(
        r.observation.state(&target(), id(), true),
        "dequeue_acknowledged"
    );
    h.await.unwrap();
    let mut responses = access(false);
    responses.extend([response(200, graph()), revoked()]);
    let (c, h) = host(responses).await;
    let r = c
        .observe_dequeue(&binding(), &target(), id())
        .await
        .unwrap();
    assert_eq!(
        r.observation.state(&target(), id(), false),
        "enqueue_unresolved"
    );
    assert_eq!(
        r.observation.state(&target(), id(), true),
        "removal_observed"
    );
    let req = h.await.unwrap();
    assert!(
        !req.iter()
            .any(|r| request_text(r).contains("dequeuePullRequest"))
    );
    let token: Value =
        serde_json::from_str(request_text(&req[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(token["permissions"]["contents"], "read");
}
#[tokio::test]
async fn dequeue_fences_before_mutation_and_does_not_mutate_an_absent_or_merged_entry() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let mut v = remove_access();
    v.extend([response(200, queued()), revoked()]);
    let (c, h) = host(v).await;
    let calls = AtomicUsize::new(0);
    let failure = c
        .dequeue_pull_attempt(&binding(), &target(), id(), || async {
            if calls.fetch_add(1, Ordering::SeqCst) == 0 {
                Ok(())
            } else {
                Err(Error::PublicationStopped)
            }
        })
        .await
        .err()
        .unwrap();
    assert!(failure.mutation_not_sent);
    assert_eq!(h.await.unwrap().len(), 6);
    for merged in [false, true] {
        let mut g = graph();
        if merged {
            let p = &mut g["data"]["repository"]["pullRequest"];
            p["merged"] = json!(true);
            p["state"] = json!("MERGED");
            p["mergeCommit"] = json!({"oid":"33".repeat(20),"tree":{"oid":"44".repeat(20)}});
        }
        let mut v = remove_access();
        v.extend([response(200, g), revoked()]);
        let (c, h) = host(v).await;
        let r = c
            .dequeue_pull(&binding(), &target(), id(), || async { Ok(()) })
            .await
            .unwrap();
        assert_eq!(
            r.observation.state(&target(), id(), true),
            if merged {
                "host_merged_unverified"
            } else {
                "removal_observed"
            }
        );
        assert!(r.observation.receipt.is_none());
        assert!(r.observation.mutation_not_sent);
        assert_eq!(h.await.unwrap().len(), 6);
    }
}
