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

fn archived_repository() -> Value {
    let mut repo = repository();
    repo["archived"] = json!(true);
    repo
}

fn archive_access(repo: Value) -> Vec<Vec<u8>> {
    vec![
        response(200, installation()),
        response(201, token()),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repo),
    ]
}

fn archived_page(nodes: Value, total: usize, cursor: Option<&str>, more: bool) -> Value {
    let mut value = page(nodes, total, cursor, more);
    value["data"]["repository"]["isArchived"] = json!(true);
    value
}

#[tokio::test]
async fn archived_branch_observation_reads_every_page_twice_and_revokes_the_read_token() {
    let mut main = head(1);
    main["name"] = json!("main");
    let topic = head(2);
    let first = archived_page(json!([main]), 2, Some("next"), true);
    let second = archived_page(json!([topic]), 2, None, false);
    let mut responses = archive_access(archived_repository());
    responses.extend([
        response(200, first.clone()),
        response(200, second.clone()),
        response(200, first),
        response(200, second),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, archived_repository()),
        revoked(),
    ]);
    let (c, host) = host(responses).await;
    let inventory = c.observe_archived_branches(&binding()).await.unwrap();
    let mut actual = serde_json::to_value(&inventory).unwrap();
    assert!(inventory.finished_at_unix >= inventory.started_at_unix);
    actual["started_at_unix"] = json!(10);
    actual["finished_at_unix"] = json!(11);
    assert_eq!(
        actual,
        json!({
            "version":1,"binding":binding(),"app_id":7,"repository_node_id":"R_test",
            "default_branch":"main","started_at_unix":10,"finished_at_unix":11,
            "heads":[main,topic],"token_revoked":true
        })
    );
    let requests = host.await.unwrap();
    assert_eq!(
        body(&requests[1]),
        json!({"repository_ids":[11],"permissions":{"contents":"read","metadata":"read"}})
    );
    for (request, cursor) in
        requests[4..8]
            .iter()
            .zip([Value::Null, json!("next"), Value::Null, json!("next")])
    {
        assert!(request_text(request).starts_with("POST /graphql "));
        let query = body(request);
        assert!(query["query"].as_str().unwrap().starts_with("query "));
        assert!(query["query"].as_str().unwrap().contains("isArchived"));
        assert_eq!(query["variables"]["after"], cursor);
    }
    assert!(request_text(&requests[9]).starts_with("GET /repos/example-org/demo "));
    assert!(request_text(&requests[10]).starts_with("DELETE /installation/token "));
}

#[tokio::test]
async fn archived_branch_observation_refuses_an_inconsistent_archive_or_inventory() {
    for failure in [
        "active",
        "reopened page",
        "partial",
        "limit",
        "no default",
        "moved",
        "reopened final",
    ] {
        let mut main = head(1);
        main["name"] = json!("main");
        let stable = archived_page(json!([main]), 1, None, false);
        let mut responses = archive_access(if failure == "active" {
            repository()
        } else {
            archived_repository()
        });
        match failure {
            "active" => {}
            "reopened page" => responses.push(response(200, page(json!([main]), 1, None, false))),
            "partial" => {
                responses.push(response(200, archived_page(json!([main]), 2, None, false)))
            }
            "limit" => responses.push(response(
                200,
                archived_page(json!([main]), 129, Some("next"), true),
            )),
            "no default" => responses.push(response(
                200,
                archived_page(json!([head(2)]), 1, None, false),
            )),
            "moved" | "reopened final" => {
                let mut after = stable.clone();
                if failure == "moved" {
                    after["data"]["repository"]["inventory"]["nodes"][0]["target"]["oid"] =
                        json!("22".repeat(20));
                }
                responses.extend([
                    response(200, stable),
                    response(200, after),
                    response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
                    response(
                        200,
                        if failure == "reopened final" {
                            repository()
                        } else {
                            archived_repository()
                        },
                    ),
                ]);
            }
            _ => unreachable!(),
        }
        responses.push(revoked());
        let (c, host) = host(responses).await;
        assert_eq!(
            c.observe_archived_branches(&binding()).await.unwrap_err(),
            Error::ArchivedBranches,
            "{failure}"
        );
        assert!(
            request_text(host.await.unwrap().last().unwrap())
                .starts_with("DELETE /installation/token ")
        );
    }
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
