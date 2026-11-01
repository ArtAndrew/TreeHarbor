use super::*;

fn readonly(mut g: Value) -> Value {
    if let Some(q) = g["data"]["repository"]["pullRequest"]["mergeQueue"].as_object_mut() {
        q.remove("configuration");
    }
    g
}

#[tokio::test]
async fn restore_queue_retains_two_read_only_snapshots_and_changes_without_settlement() {
    let absent = readonly(graph());
    let queued = readonly(queued());
    let mut merged = absent.clone();
    let p = &mut merged["data"]["repository"]["pullRequest"];
    p["state"] = json!("MERGED");
    p["merged"] = json!(true);
    p["mergeCommit"] = json!({"oid":"33".repeat(20),"tree":{"oid":"66".repeat(20)}});
    let mut closed = absent.clone();
    closed["data"]["repository"]["pullRequest"]["state"] = json!("CLOSED");
    let mut disabled = absent.clone();
    let p = &mut disabled["data"]["repository"]["pullRequest"];
    p["isMergeQueueEnabled"] = json!(false);
    p["mergeQueue"] = Value::Null;
    let mut moved = absent.clone();
    moved["data"]["repository"]["pullRequest"]["headRefOid"] = json!("44".repeat(20));
    for (before, after, expected) in [
        (absent.clone(), absent.clone(), "not_in_queue"),
        (queued.clone(), queued.clone(), "queued"),
        (queued, absent, "changed_during_capture"),
        (merged.clone(), merged, "host_merged_unverified"),
        (closed.clone(), closed, "closed_unmerged"),
        (disabled.clone(), disabled, "queue_disabled"),
        (moved.clone(), moved, "head_differs"),
    ] {
        let mut replies = access(false);
        replies.extend([response(200, before), response(200, after), revoked()]);
        let (c, h) = host(replies).await;
        let observed = c
            .observe_restore_queue(&binding(), &target())
            .await
            .unwrap();
        assert_eq!(observed.report.classification(&target()).unwrap(), expected);
        assert!(observed.report.token_revoked);
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 7);
        let mint: Value =
            serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1)
                .unwrap();
        assert_eq!(
            mint["permissions"],
            json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read","merge_queues":"read"})
        );
        for r in &requests[4..6] {
            let text = request_text(r);
            assert!(text.starts_with("POST /graphql "));
            let body: Value = serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap();
            assert!(body["query"].as_str().unwrap().starts_with("query "));
            assert_eq!(body["variables"]["configuration"], false);
            assert_eq!(body["variables"]["number"], 17);
        }
        assert!(!requests.iter().any(|r| request_text(r).starts_with("PUT ")));
        assert!(request_text(&requests[6]).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn restore_queue_rejects_missing_nullable_fields_wrong_scope_and_partial_capture() {
    for field in [
        "mergeCommit",
        "mergeQueue",
        "mergeQueueEntry",
        "baseCommit",
        "headCommit",
        "scope",
        "entry",
        "configuration",
        "errors",
        "network",
    ] {
        let mut g = readonly(queued());
        let p = &mut g["data"]["repository"]["pullRequest"];
        match field {
            "mergeCommit" | "mergeQueue" | "mergeQueueEntry" => {
                p.as_object_mut().unwrap().remove(field);
            }
            "baseCommit" | "headCommit" => {
                p["mergeQueueEntry"].as_object_mut().unwrap().remove(field);
            }
            "scope" => p["baseRepository"]["databaseId"] = json!(12),
            "entry" => p["mergeQueueEntry"]["pullRequest"]["id"] = json!("PR_other"),
            "configuration" => g = graph(),
            "errors" => g["errors"] = json!([{"message":"untrusted diagnostic"}]),
            _ => (),
        }
        let mut replies = access(false);
        // Reject the second read too; never keep the first as a complete capture.
        replies.push(response(200, readonly(queued())));
        replies.push(if field == "network" {
            Vec::new()
        } else {
            response(200, g)
        });
        replies.push(revoked());
        let (c, h) = host(replies).await;
        assert!(
            c.observe_restore_queue(&binding(), &target())
                .await
                .is_err(),
            "{field}"
        );
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 7);
        assert!(request_text(&requests[6]).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn restore_queue_keeps_failed_revocation_explicit() {
    let mut replies = access(false);
    replies.extend([
        response(200, readonly(graph())),
        response(200, readonly(graph())),
        response(500, json!({})),
    ]);
    let (c, h) = host(replies).await;
    assert!(
        !c.observe_restore_queue(&binding(), &target())
            .await
            .unwrap()
            .report
            .token_revoked
    );
    assert_eq!(h.await.unwrap().len(), 7);
}
