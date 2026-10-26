use super::*;
use treeharbor_github_contract::host_configuration::{Configuration, Target};
fn fixture() -> Configuration {
    let v: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/host-configuration-inspection-v1.json"
    )))
    .unwrap();
    let mut c: Configuration = serde_json::from_value(v["configuration"].clone()).unwrap();
    c.profile.repository_name = "example-org/demo".into();
    c.visibility.target.repository_name = c.profile.repository_name.clone();
    c.profile.rules[0]["ruleset_source"] = json!("example-org/demo");
    c.profile.rulesets[0].source.name = "example-org/demo".into();
    c.profile.rulesets[0].document.value.as_mut().unwrap()["source"] = json!("example-org/demo");
    c
}
fn graph(c: &Configuration, settings: bool) -> Value {
    let l = c.visibility.legacy.as_ref().unwrap();
    let mut rule = json!({"id":l.id,"repository":{"databaseId":11},"requiredApprovingReviewCount":l.approving_reviews,
        "requiredStatusCheckContexts":l.required_contexts,"requiredDeploymentEnvironments":l.deployment_environments,
        "bypassPullRequestAllowances":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"hasPreviousPage":false}},
        "bypassForcePushAllowances":{"totalCount":0,"nodes":[],"pageInfo":{"hasNextPage":false,"hasPreviousPage":false}}});
    for (k, v) in &l.flags {
        rule[k] = json!(v);
    }
    let mut q = json!({"id":c.visibility.queue_id,"repository":{"databaseId":11}});
    if settings {
        let v = c.profile.queue.as_ref().unwrap();
        q["configuration"] = json!({"mergeMethod":v.merge_method,"mergingStrategy":v.merging_strategy,
        "maximumEntriesToBuild":v.maximum_entries_to_build,"maximumEntriesToMerge":v.maximum_entries_to_merge,"minimumEntriesToMerge":v.minimum_entries_to_merge,
        "minimumEntriesToMergeWaitTime":v.minimum_entries_wait_minutes,"checkResponseTimeout":v.check_timeout_minutes});
    }
    json!({"data":{"repository":{"databaseId":11,"nameWithOwner":"example-org/demo","owner":{"__typename":"Organization"},
        "ref":{"name":c.profile.base_ref,"prefix":"refs/heads/","repository":{"databaseId":11},"branchProtectionRule":rule},"mergeQueue":q}}})
}
fn access(settings: bool) -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"] = json!({"contents":if settings {"write"}else{"read"},"metadata":"read","administration":"read","merge_queues":"read"});
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn round(c: &Configuration, g: Value) -> Vec<Vec<u8>> {
    vec![
        response(200, g),
        response(200, c.profile.protection.value.clone().unwrap()),
        response(200, json!(c.profile.rules)),
        response(
            c.profile.rulesets[0].document.status,
            c.profile.rulesets[0]
                .document
                .value
                .clone()
                .unwrap_or(json!({"message":"synthetic denied"})),
        ),
    ]
}
#[tokio::test]
async fn configuration_inspection_is_pr_independent_and_never_implicitly_upgrades() {
    for settings in [false, true] {
        let c = fixture();
        let mut replies = access(settings);
        replies.extend(round(&c, graph(&c, settings)));
        replies.extend(round(&c, graph(&c, settings)));
        replies.push(revoked());
        let (connection, h) = host(replies).await;
        let report = connection
            .inspect_host_configuration(&binding(), "main", settings)
            .await
            .unwrap();
        let req = h.await.unwrap();
        assert_eq!(req.len(), 13);
        assert!(report.token_revoked);
        let t = &c.visibility.target;
        assert_eq!(report.before.visibility, c.visibility);
        if settings {
            assert_eq!(*report.before, c);
            assert!(report.reasons(t).unwrap().is_empty());
        } else {
            assert!(report.before.profile.queue.is_none());
            assert!(
                report
                    .reasons(t)
                    .unwrap()
                    .contains("queue_configuration_unproven")
            );
        }
        let mint: Value =
            serde_json::from_str(request_text(&req[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            mint["permissions"],
            json!({"contents":if settings {"write"}else{"read"},"metadata":"read","administration":"read","merge_queues":"read"})
        );
        assert_eq!(mint["repository_ids"], json!([11]));
        for request in &req[4..12] {
            let request = request_text(request);
            assert!(!request.contains("mutation "));
            assert!(!request.contains("pullRequest("));
            assert!(!request.contains("/pulls/"));
            assert!(request.starts_with("GET ") || request.starts_with("POST /graphql "));
        }
        let q: Value =
            serde_json::from_str(request_text(&req[4]).split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(
            q["variables"],
            json!({"owner":"example-org","name":"demo","branch":"main","ref":"refs/heads/main","settings":settings})
        );
        assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn malformed_graph_identity_or_visibility_revokes_before_any_rest_round() {
    for case in [
        "repository",
        "ref-repository",
        "rule-repository",
        "queue-repository",
        "owner",
        "ref",
        "prefix",
        "errors",
        "missing-flag",
        "missing-count",
        "partial",
        "duplicate",
        "null-nodes",
        "unexpected-settings",
    ] {
        let c = fixture();
        let mut g = graph(&c, false);
        let r = &mut g["data"]["repository"];
        match case {
            "repository" => r["databaseId"] = json!(12),
            "ref-repository" => r["ref"]["repository"]["databaseId"] = json!(12),
            "rule-repository" => {
                r["ref"]["branchProtectionRule"]["repository"]["databaseId"] = json!(12)
            }
            "queue-repository" => r["mergeQueue"]["repository"]["databaseId"] = json!(12),
            "owner" => r["owner"]["__typename"] = json!("User"),
            "ref" => r["ref"]["name"] = json!("other"),
            "prefix" => r["ref"]["prefix"] = json!("refs/tags/"),
            "errors" => g["errors"] = json!([{"message":"synthetic partial response"}]),
            "missing-flag" => {
                r["ref"]["branchProtectionRule"]
                    .as_object_mut()
                    .unwrap()
                    .remove("requiresDeployments");
            }
            "missing-count" => {
                r["ref"]["branchProtectionRule"]
                    .as_object_mut()
                    .unwrap()
                    .remove("requiredApprovingReviewCount");
            }
            "partial" => {
                r["ref"]["branchProtectionRule"]["bypassPullRequestAllowances"]["pageInfo"]["hasNextPage"] =
                    json!(true)
            }
            "null-nodes" => {
                r["ref"]["branchProtectionRule"]["bypassPullRequestAllowances"]["nodes"] =
                    Value::Null
            }
            "duplicate" => {
                let a = &mut r["ref"]["branchProtectionRule"]["bypassForcePushAllowances"];
                a["nodes"] = json!([{"id":"A_one"},{"id":"A_one"}]);
                a["totalCount"] = json!(2);
            }
            _ => r["mergeQueue"]["configuration"] = Value::Null,
        }
        let mut replies = access(false);
        replies.push(response(200, g));
        replies.push(revoked());
        let (connection, h) = host(replies).await;
        let result = connection
            .inspect_host_configuration(&binding(), "main", false)
            .await;
        let req = h.await.unwrap();
        assert!(result.is_err(), "{case}");
        assert_eq!(req.len(), 6, "{case}");
        assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn drift_missing_legacy_and_redacted_rules_remain_diagnostics() {
    for case in ["drift", "no-legacy", "redacted", "null-settings"] {
        let c = fixture();
        let mut after = c.clone();
        let mut g = graph(&c, true);
        match case {
            "drift" => {
                g["data"]["repository"]["ref"]["branchProtectionRule"]["id"] = json!("BPR_changed")
            }
            "no-legacy" => g["data"]["repository"]["ref"]["branchProtectionRule"] = Value::Null,
            "redacted" => {
                after.profile.rulesets[0]
                    .document
                    .value
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("bypass_actors");
            }
            _ => g["data"]["repository"]["mergeQueue"]["configuration"] = Value::Null,
        }
        let mut replies = access(true);
        replies.extend(round(&c, graph(&c, true)));
        replies.extend(round(&after, g));
        replies.push(revoked());
        let (connection, h) = host(replies).await;
        let report = connection
            .inspect_host_configuration(&binding(), "main", true)
            .await
            .unwrap();
        h.await.unwrap();
        let reasons = report.reasons(&c.visibility.target).unwrap();
        assert!(reasons.contains("configuration_changed"), "{case}");
        if case == "no-legacy" {
            assert!(reasons.contains("legacy_visibility_unavailable"));
        }
        if case == "null-settings" {
            assert!(reasons.contains("queue_configuration_unproven"));
        }
        if case == "redacted" {
            assert!(
                !report
                    .after
                    .analyze(&c.visibility.target)
                    .unwrap()
                    .configuration_reasons
                    .is_empty()
            );
        }
    }
}
#[tokio::test]
async fn overgranted_token_and_provider_failure_are_revoked_without_fallback() {
    for case in ["overgrant", "provider", "revoke"] {
        let c = fixture();
        let mut replies = access(false);
        match case {
            "overgrant" => {
                let mut t = token();
                t["permissions"] = json!({"contents":"write","metadata":"read","administration":"read","merge_queues":"read"});
                replies.truncate(1);
                replies.push(response(201, t));
            }
            "provider" => replies.push(response(503, json!({"message":"synthetic failure"}))),
            _ => {
                replies.extend(round(&c, graph(&c, false)));
                replies.extend(round(&c, graph(&c, false)));
            }
        }
        replies.push(if case == "revoke" {
            response(503, json!({"message":"synthetic failure"}))
        } else {
            revoked()
        });
        let (connection, h) = host(replies).await;
        let result = connection
            .inspect_host_configuration(&binding(), "main", false)
            .await;
        let req = h.await.unwrap();
        if case == "revoke" {
            let r = result.unwrap();
            assert!(!r.token_revoked);
            assert!(
                r.reasons(&c.visibility.target)
                    .unwrap()
                    .contains("token_revocation_unproven")
            );
        } else {
            assert!(result.is_err(), "{case}");
        }
        assert_eq!(
            req.iter()
                .filter(|r| request_text(r).contains("POST /app/installations/9/access_tokens "))
                .count(),
            1
        );
        assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn branch_path_is_encoded_and_target_validation_precedes_credentials() {
    let mut c = fixture();
    c.profile.base_ref = "release/v1".into();
    c.visibility.target.base_ref = c.profile.base_ref.clone();
    let mut replies = access(false);
    replies.extend(round(&c, graph(&c, false)));
    replies.extend(round(&c, graph(&c, false)));
    replies.push(revoked());
    let (connection, h) = host(replies).await;
    connection
        .inspect_host_configuration(&binding(), "release/v1", false)
        .await
        .unwrap();
    let req = h.await.unwrap();
    assert!(
        request_text(&req[5])
            .starts_with("GET /repos/example-org/demo/branches/release%2Fv1/protection ")
    );
    assert!(request_text(&req[6]).contains("rules/branches/release%2Fv1"));
    let (connection, h) = host(vec![]).await;
    assert!(
        connection
            .inspect_host_configuration(&binding(), "../escape", false)
            .await
            .is_err()
    );
    assert!(h.await.unwrap().is_empty());
    let t = Target {
        repository_id: 11,
        repository_name: "example-org/demo".into(),
        base_ref: "main".into(),
    };
    t.validate().unwrap();
}
