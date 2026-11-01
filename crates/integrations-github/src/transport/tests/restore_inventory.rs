use super::*;

fn access() -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn page(nodes: Value, total: usize, cursor: Option<&str>, more: bool) -> Value {
    json!({"data":{"repository":{"id":"R_test","databaseId":11,"nameWithOwner":"example-org/demo",
        "inventory":{"nodes":nodes,"totalCount":total,"pageInfo":{"endCursor":cursor,"hasNextPage":more}}}}})
}
fn head(id: u64) -> Value {
    json!({"id":format!("REF_{id}"),"name":format!("treeharbor/{id:032x}"),"prefix":"refs/heads/",
        "target":{"__typename":"Commit","oid":"11".repeat(20)}})
}
fn pull(number: u64, state: &str) -> Value {
    json!({"id":format!("PR_{number}"),"number":number,"state":state,"isDraft":false,"merged":state=="MERGED",
        "headRefName":format!("treeharbor/{number:032x}"),"headRefOid":"22".repeat(20),
        "baseRefName":"main","baseRefOid":"33".repeat(20),"headRepository":{"databaseId":11},
        "baseRepository":{"databaseId":11},"mergeCommit":if state=="MERGED" { json!({"oid":"44".repeat(20)}) } else { Value::Null }})
}
fn body(request: &[u8]) -> Value {
    serde_json::from_str(request_text(request).split("\r\n\r\n").nth(1).unwrap()).unwrap()
}
#[tokio::test]
async fn restore_inventory_discovers_closed_missing_branch_and_unknown_operations_read_only() {
    // No restored operation IDs are supplied. PR 99 has no surviving branch;
    // 100 comes from a deleted fork and 101 from a different repository.
    let mut deleted = pull(100, "CLOSED");
    deleted["headRepository"] = Value::Null;
    let mut foreign = pull(101, "OPEN");
    foreign["headRepository"]["databaseId"] = json!(77);
    let mut responses = access();
    responses.extend([
        response(200, page(json!([head(1)]), 2, Some("heads-next"), true)),
        response(200, page(json!([head(2)]), 2, None, false)),
        response(
            200,
            page(json!([pull(99, "MERGED")]), 3, Some("pulls-next"), true),
        ),
        response(200, page(json!([deleted, foreign]), 3, None, false)),
        revoked(),
    ]);
    let (c, host) = host(responses).await;
    let inventory = c.observe_restore_inventory(&binding()).await.unwrap();
    assert_eq!(inventory.heads.len(), 2);
    assert_eq!(inventory.pulls.len(), 3);
    assert!(inventory.pulls[0].merged);
    assert_eq!(inventory.pulls[0].number, 99);
    assert!(inventory.pulls[1].head_repository.is_none());
    assert_eq!(
        inventory.pulls[2]
            .head_repository
            .as_ref()
            .unwrap()
            .database_id,
        77
    );
    assert!(inventory.token_revoked);
    assert!(!inventory.reconciliation_complete);
    assert!(inventory.finished_at_unix >= inventory.started_at_unix);
    assert!(
        !serde_json::to_string(&inventory)
            .unwrap()
            .contains(TEST_TOKEN)
    );
    let requests = host.await.unwrap();
    assert_eq!(requests.len(), 9);
    assert_eq!(
        body(&requests[1]),
        json!({"repository_ids":[11],"permissions":{
        "contents":"read","metadata":"read","pull_requests":"read"}})
    );
    assert_eq!(body(&requests[5])["variables"]["after"], "heads-next");
    assert_eq!(body(&requests[7])["variables"]["after"], "pulls-next");
    for request in &requests[4..8] {
        assert!(request_text(request).starts_with("POST /graphql "));
        assert!(
            body(request)["query"]
                .as_str()
                .unwrap()
                .starts_with("query ")
        );
        assert!(
            !body(request)["query"]
                .as_str()
                .unwrap()
                .contains("mutation")
        );
    }
    assert!(
        body(&requests[6])["query"]
            .as_str()
            .unwrap()
            .contains("states:[OPEN,CLOSED,MERGED]")
    );
    assert!(request_text(&requests[8]).starts_with("DELETE /installation/token "));
}

#[tokio::test]
async fn restore_inventory_rejects_partial_graphql_data_scope_and_invalid_heads() {
    for kind in [
        "errors",
        "null",
        "repo",
        "node",
        "rename",
        "count",
        "huge",
        "empty-progress",
        "null-cursor",
        "wrong-kind",
        "bad-oid",
        "prefix",
        "missing-field",
    ] {
        let mut p = page(json!([head(1)]), 1, None, false);
        match kind {
            "errors" => p["errors"] = json!([{"message":"private upstream error"}]),
            "null" => p["data"] = Value::Null,
            "repo" => p["data"]["repository"]["databaseId"] = json!(77),
            "node" => p["data"]["repository"]["id"] = json!("R_other"),
            "rename" => p["data"]["repository"]["nameWithOwner"] = json!("other/demo"),
            "count" => p["data"]["repository"]["inventory"]["totalCount"] = json!(2),
            "huge" => p["data"]["repository"]["inventory"]["totalCount"] = json!(5001),
            "empty-progress" => p = page(json!([]), 1, Some("next"), true),
            "null-cursor" => p = page(json!([head(1)]), 2, None, true),
            "wrong-kind" => {
                p["data"]["repository"]["inventory"]["nodes"][0]["target"]["__typename"] =
                    json!("Tag")
            }
            "bad-oid" => {
                p["data"]["repository"]["inventory"]["nodes"][0]["target"]["oid"] =
                    json!("0".repeat(40))
            }
            "prefix" => {
                p["data"]["repository"]["inventory"]["nodes"][0]["prefix"] = json!("refs/tags/")
            }
            "missing-field" => {
                p["data"]["repository"]["inventory"]["nodes"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("target");
            }
            _ => unreachable!(),
        }
        let mut responses = access();
        responses.extend([response(200, p), revoked()]);
        let (c, host) = host(responses).await;
        let error = c.observe_restore_inventory(&binding()).await.unwrap_err();
        assert!(!error.to_string().contains("private upstream error"));
        assert!(!error.to_string().contains(TEST_TOKEN));
        assert_eq!(host.await.unwrap().len(), 6, "{kind}");
    }
}

#[tokio::test]
async fn restore_inventory_refuses_repeated_ids_names_cursors_and_changing_counts() {
    for kind in ["id", "name", "cursor", "count"] {
        let mut second = head(2);
        if kind == "id" {
            second["id"] = head(1)["id"].clone();
        }
        if kind == "name" {
            second["name"] = head(1)["name"].clone();
        }
        let mut responses = access();
        responses.extend([
            response(200, page(json!([head(1)]), 3, Some("one"), true)),
            response(
                200,
                page(
                    json!([second]),
                    if kind == "count" { 4 } else { 3 },
                    Some(if kind == "cursor" { "one" } else { "two" }),
                    true,
                ),
            ),
            revoked(),
        ]);
        let (c, host) = host(responses).await;
        assert_eq!(
            c.observe_restore_inventory(&binding()).await.unwrap_err(),
            Error::RestoreInventory
        );
        assert_eq!(host.await.unwrap().len(), 7);
    }
}

#[tokio::test]
async fn restore_inventory_never_returns_a_truncated_success_at_the_page_budget() {
    let mut responses = access();
    for n in 1..=100 {
        responses.push(response(
            200,
            page(json!([head(n)]), 101, Some(&format!("cursor-{n}")), true),
        ));
    }
    responses.push(revoked());
    let (c, host) = host(responses).await;
    assert_eq!(
        c.observe_restore_inventory(&binding()).await.unwrap_err(),
        Error::RestoreInventory
    );
    assert_eq!(host.await.unwrap().len(), 105);
}

#[tokio::test]
async fn restore_inventory_validates_pulls_even_when_all_heads_have_been_deleted() {
    for kind in [
        "base",
        "state",
        "merge",
        "oid",
        "number",
        "duplicate",
        "null-node",
        "missing-head-repo",
        "missing-merge-commit",
    ] {
        let mut p = pull(1, "OPEN");
        match kind {
            "base" => p["baseRepository"]["databaseId"] = json!(77),
            "state" => p["state"] = json!("UNKNOWN"),
            "merge" => p["merged"] = json!(true),
            "oid" => p["headRefOid"] = json!("invalid"),
            "number" => p["number"] = json!(0),
            "null-node" => p = Value::Null,
            "missing-head-repo" => {
                p.as_object_mut().unwrap().remove("headRepository");
            }
            "missing-merge-commit" => {
                p.as_object_mut().unwrap().remove("mergeCommit");
            }
            _ => {}
        }
        let nodes = if kind == "duplicate" {
            json!([p.clone(), p])
        } else {
            json!([p])
        };
        let mut responses = access();
        responses.extend([
            response(200, page(json!([]), 0, None, false)),
            response(
                200,
                page(nodes, if kind == "duplicate" { 2 } else { 1 }, None, false),
            ),
            revoked(),
        ]);
        let (c, host) = host(responses).await;
        assert!(
            c.observe_restore_inventory(&binding()).await.is_err(),
            "{kind}"
        );
        assert_eq!(host.await.unwrap().len(), 7);
    }
}

#[tokio::test]
async fn restore_inventory_reports_failed_token_revocation_and_never_retries_a_lost_read() {
    for lost_read in [false, true] {
        let mut responses = access();
        if lost_read {
            responses.push(vec![]);
            responses.push(revoked());
        } else {
            responses.extend([
                response(200, page(json!([]), 0, None, false)),
                response(200, page(json!([]), 0, None, false)),
                response(500, json!({"message":"revoke failed"})),
            ]);
        }
        let (c, host) = host(responses).await;
        let result = c.observe_restore_inventory(&binding()).await;
        if lost_read {
            assert!(result.is_err());
        } else {
            assert!(!result.unwrap().token_revoked);
        }
        assert_eq!(host.await.unwrap().len(), if lost_read { 6 } else { 7 });
    }
}
