use super::*;
use crate::ObservedQueue;
use treeharbor_github_contract::{host_policy as hp, publication};
fn fixture() -> (QueueTarget, hp::Report) {
    let v: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/host-policy-v1.json"
    )))
    .unwrap();
    let mut t: QueueTarget = serde_json::from_value(v["queue_target"].clone()).unwrap();
    let mut r: hp::Report = serde_json::from_value(v["report"].clone()).unwrap();
    r.repository_name = "example-org/demo".into();
    for s in [&mut r.before, &mut r.after] {
        s.rules[0]["ruleset_source"] = json!("example-org/demo");
        s.rulesets[0].source.name = "example-org/demo".into();
        s.rulesets[0].document.value.as_mut().unwrap()["source"] = json!("example-org/demo");
    }
    t.expected_rules = hp::rules_fingerprint(&r.before.rules).unwrap();
    (t, r)
}
fn access() -> Vec<Vec<u8>> {
    let mut token = token();
    token["permissions"] = json!({"contents":"read","metadata":"read","pull_requests":"read","administration":"read","merge_queues":"read"});
    vec![
        response(200, installation()),
        response(201, token),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn review() -> Value {
    let mut v = queued();
    v["data"]["repository"]["pullRequest"]["reviewDecision"] = Value::Null;
    v["data"]["repository"]["pullRequest"]["reviewThreads"] = json!({"totalCount":1,"pageInfo":{"hasNextPage":false,"hasPreviousPage":false,"endCursor":"end"},"nodes":[{"id":"PRRT_test_1","isResolved":true,"isOutdated":false}]});
    v
}
fn round(r: &hp::Report, review: Value) -> Vec<Vec<u8>> {
    vec![
        response(200, group_tests::groups()),
        response(200, r.before.protection.value.clone().unwrap()),
        response(200, json!(r.before.rules)),
        response(
            r.before.rulesets[0].document.status,
            r.before.rulesets[0]
                .document
                .value
                .clone()
                .unwrap_or(json!({"message":"synthetic access denied"})),
        ),
        response(200, review),
    ]
}
async fn observe(
    responses: Vec<Vec<u8>>,
    t: &QueueTarget,
    selected: &publication::Target,
) -> (Result<ObservedQueue>, Vec<Vec<u8>>) {
    let (c, h) = host(responses).await;
    let r = c.observe_host_policy(&binding(), t, selected).await;
    (r, h.await.unwrap())
}
#[tokio::test]
async fn complete_policy_is_read_twice_with_read_only_tokens_and_no_host_mutation() {
    let (t, r) = fixture();
    let mut replies = access();
    replies.extend(round(&r, review()));
    replies.extend(round(&r, review()));
    replies.push(revoked());
    let (result, requests) = observe(replies, &t, &r.target).await;
    let result = result.unwrap();
    assert_eq!(result.observation.version, 9);
    assert!(result.observation.token_revoked);
    assert_eq!(result.observation.state(&t), "uncertain");
    let captured = result.observation.host_policy.unwrap();
    assert!(captured.capture_reasons(&t).unwrap().is_empty());
    assert_eq!(requests.len(), 15);
    let mint: Value =
        serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(mint["repository_ids"], json!([11]));
    assert!(
        mint["permissions"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == "read")
    );
    assert!(mint["permissions"].get("checks").is_none());
    for req in &requests[4..14] {
        let q = request_text(req);
        assert!(!q.contains("mutation "));
        assert!(q.starts_with("GET ") || q.starts_with("POST /graphql "));
    }
    assert!(
        request_text(&requests[7])
            .starts_with("GET /repos/example-org/demo/rulesets/42?includes_parents=true ")
    );
    assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
}
#[tokio::test]
async fn redacted_and_denied_rulesets_survive_as_unproven_and_drift_is_retained() {
    for case in ["redacted", "forbidden", "bypass", "review"] {
        let (t, mut r) = fixture();
        match case {
            "redacted" => {
                r.before.rulesets[0]
                    .document
                    .value
                    .as_mut()
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove("bypass_actors");
            }
            "forbidden" => {
                r.before.rulesets[0].document.status = 403;
                r.before.rulesets[0].document.value = None;
            }
            "bypass" => {
                r.before.rulesets[0].document.value.as_mut().unwrap()["bypass_actors"] =
                    json!([{"actor_type":"OrganizationAdmin","bypass_mode":"always"}])
            }
            _ => (),
        }
        let mut after = review();
        if case == "review" {
            after["data"]["repository"]["pullRequest"]["reviewDecision"] =
                json!("CHANGES_REQUESTED");
        }
        let mut replies = access();
        replies.extend(round(&r, review()));
        replies.extend(round(&r, after));
        replies.push(revoked());
        let (result, _) = observe(replies, &t, &r.target).await;
        let captured = result.unwrap().observation.host_policy.unwrap();
        let reason = match case {
            "redacted" => "ruleset_bypass_redacted",
            "forbidden" => "ruleset_unavailable",
            "bypass" => "ruleset_bypass_present",
            _ => "capture_changed",
        };
        assert!(
            captured.capture_reasons(&t).unwrap().contains(reason),
            "{case}"
        );
    }
}
#[tokio::test]
async fn malformed_or_incomplete_review_fails_and_revokes_without_retry() {
    for case in [
        "missing-decision",
        "foreign",
        "head-repo",
        "errors",
        "count",
        "null",
        "duplicate",
        "cursor",
    ] {
        let (t, r) = fixture();
        let mut v = review();
        let p = &mut v["data"]["repository"]["pullRequest"];
        match case {
            "missing-decision" => {
                p.as_object_mut().unwrap().remove("reviewDecision");
            }
            "foreign" => p["id"] = json!("PR_other"),
            "head-repo" => p["headRepository"]["databaseId"] = json!(12),
            "count" => p["reviewThreads"]["totalCount"] = json!(1001),
            "null" => p["reviewThreads"]["nodes"] = Value::Null,
            "duplicate" => {
                let n = p["reviewThreads"]["nodes"][0].clone();
                p["reviewThreads"]["nodes"].as_array_mut().unwrap().push(n);
                p["reviewThreads"]["totalCount"] = json!(2);
            }
            "cursor" => {
                p["reviewThreads"]["totalCount"] = json!(2);
                p["reviewThreads"]["pageInfo"]["hasNextPage"] = json!(true);
                p["reviewThreads"]["pageInfo"]["endCursor"] = Value::Null;
            }
            "errors" => v["errors"] = json!([{"message":"synthetic partial response"}]),
            _ => unreachable!(),
        }
        let mut replies = access();
        replies.extend(round(&r, v));
        replies.push(revoked());
        let (result, requests) = observe(replies, &t, &r.target).await;
        assert!(result.is_err(), "{case}");
        assert_eq!(requests.len(), 10, "{case}");
        assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn review_pages_bind_one_head_and_complete_count() {
    for changed in [false, true] {
        let (t, r) = fixture();
        let mut first = review();
        let p = &mut first["data"]["repository"]["pullRequest"]["reviewThreads"];
        p["totalCount"] = json!(101);
        p["pageInfo"]["hasNextPage"] = json!(true);
        p["pageInfo"]["endCursor"] = json!("page-two");
        p["nodes"] = json!(
            (0..100)
                .map(|n| json!({"id":format!("Thread_{n}"),"isResolved":true,"isOutdated":false}))
                .collect::<Vec<_>>()
        );
        let mut last = review();
        let q = &mut last["data"]["repository"]["pullRequest"];
        q["reviewThreads"]["totalCount"] = json!(101);
        q["reviewThreads"]["pageInfo"]["hasPreviousPage"] = json!(true);
        if changed {
            q["headRefOid"] = json!("ab".repeat(20));
        }
        let mut replies = access();
        replies.extend(round(&r, first.clone()));
        replies.push(response(200, last.clone()));
        if !changed {
            replies.extend(round(&r, first));
            replies.push(response(200, last));
        }
        replies.push(revoked());
        let (result, requests) = observe(replies, &t, &r.target).await;
        if changed {
            assert!(result.is_err());
        } else {
            let p = result.unwrap().observation.host_policy.unwrap();
            assert_eq!(p.before.review.threads.len(), 101);
            assert!(p.capture_reasons(&t).unwrap().is_empty());
        }
        assert!(request_text(&requests[9]).contains("page-two"));
        assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn rule_pages_and_inherited_details_are_complete_and_source_qualified() {
    for corrupt in [false, true] {
        let (mut t, mut r) = fixture();
        let s = &mut r.before;
        s.rules[0]["ruleset_source_type"] = json!("Organization");
        s.rules[0]["ruleset_source"] = json!("example-org");
        s.rulesets[0].source.kind = "Organization".into();
        s.rulesets[0].source.name = "example-org".into();
        let detail = s.rulesets[0].document.value.as_mut().unwrap();
        detail["source_type"] = json!("Organization");
        detail["source"] = json!("example-org");
        for ch in b'a'..=b'j' {
            let mut rule = s.rules[0].clone();
            rule["type"] = json!(format!("future_{}", ch as char));
            rule["parameters"] = json!({"unknown":true});
            detail["rules"]
                .as_array_mut()
                .unwrap()
                .push(json!({"type":rule["type"],"parameters":rule["parameters"]}));
            s.rules.push(rule);
        }
        t.expected_rules = hp::rules_fingerprint(&s.rules).unwrap();
        let mut replies = access();
        for _ in 0..if corrupt { 1 } else { 2 } {
            let mut pages = round(&r, review());
            pages[2] = response(200, json!(&r.before.rules[..10]));
            let mut last = r.before.rules[10].clone();
            if corrupt {
                last["ruleset_source"] = json!("other-org");
            }
            pages.insert(3, response(200, json!([last])));
            if corrupt {
                pages.truncate(4);
            }
            replies.extend(pages);
        }
        replies.push(revoked());
        let (outcome, requests) = observe(replies, &t, &r.target).await;
        assert!(request_text(&requests[7]).contains("per_page=10&page=2"));
        if corrupt {
            assert!(outcome.is_err());
        } else {
            let p = outcome.unwrap().observation.host_policy.unwrap();
            assert_eq!(p.before.rules.len(), 11);
            assert_eq!(p.before.rulesets.len(), 1);
            assert_eq!(p.before.rulesets[0].source.kind, "Organization");
            assert!(p.capture_reasons(&t).unwrap().is_empty());
            assert!(request_text(&requests[8]).contains("rulesets/42?includes_parents=true"));
        }
        assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn duplicate_rules_foreign_details_and_upstream_failure_revoke_without_mutation() {
    for case in ["duplicate", "detail", "failure", "overgrant"] {
        let (t, mut r) = fixture();
        let mut replies = access();
        match case {
            "overgrant" => {
                let mut broad = token();
                broad["permissions"] = json!({"contents":"read","metadata":"read","pull_requests":"read","administration":"write","merge_queues":"read"});
                replies.truncate(1);
                replies.push(response(201, broad));
            }
            "duplicate" => {
                r.before.rules.push(r.before.rules[0].clone());
                let mut pages = round(&r, review());
                pages.truncate(3);
                replies.extend(pages);
            }
            "detail" => {
                r.before.rulesets[0].document.value.as_mut().unwrap()["source"] =
                    json!("foreign/repository");
                replies.extend(round(&r, review()));
            }
            _ => {
                replies.push(response(
                    503,
                    json!({"message":"synthetic provider failure"}),
                ));
            }
        }
        replies.push(revoked());
        let (outcome, requests) = observe(replies, &t, &r.target).await;
        assert!(outcome.is_err(), "{case}");
        assert!(
            request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "),
            "{case}"
        );
        assert!(
            requests
                .iter()
                .all(|r| !request_text(r).contains("mutation "))
        );
    }
}

#[tokio::test]
async fn candidate_configuration_is_collected_in_each_round_with_explicit_settings_scope() {
    for settings in [false, true] {
        let c = super::host_configuration_tests::fixture();
        let (mut t, r) = fixture();
        t.expected_rules = hp::rules_fingerprint(&c.profile.rules).unwrap();
        let mut replies = access();
        let mut minted = token();
        minted["permissions"] = json!({"contents":if settings {"write"}else{"read"},"metadata":"read","pull_requests":"read","administration":"read","merge_queues":"read"});
        replies[1] = response(201, minted);
        for _ in 0..2 {
            replies.push(response(200, group_tests::groups()));
            replies.extend(super::host_configuration_tests::round(
                &c,
                super::host_configuration_tests::graph(&c, settings),
            ));
            replies.push(response(200, review()));
        }
        replies.push(revoked());
        let (connection, h) = host(replies).await;
        let out = connection
            .observe_candidate_configuration(&binding(), &t, &r.target, settings)
            .await
            .unwrap();
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 17);
        assert_eq!(out.observation.version, 10);
        assert!(out.observation.token_revoked);
        let observed = out.observation.host_policy.unwrap();
        assert_eq!(observed.version, 2);
        assert_eq!(observed.before.visibility.as_deref(), Some(&c.visibility));
        assert_eq!(observed.before, observed.after);
        let captured = observed
            .before
            .configuration(&observed.repository_name, &t)
            .unwrap()
            .unwrap();
        if settings {
            assert_eq!(captured, c);
        } else {
            assert!(captured.profile.queue.is_none());
        }
        let mint: Value =
            serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1)
                .unwrap();
        assert_eq!(
            mint["permissions"]["contents"],
            if settings { "write" } else { "read" }
        );
        assert_eq!(mint["permissions"]["pull_requests"], "read");
        assert_eq!(mint["permissions"]["merge_queues"], "read");
        for request in &requests[4..16] {
            assert!(!request_text(request).contains("mutation "));
        }
        for index in [5, 11] {
            let v: Value = serde_json::from_str(
                request_text(&requests[index])
                    .split_once("\r\n\r\n")
                    .unwrap()
                    .1,
            )
            .unwrap();
            assert_eq!(v["variables"]["settings"], settings);
        }
        assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn candidate_configuration_drift_is_retained_but_incomplete_visibility_fails_and_revokes() {
    for case in ["drift", "missing_flag", "queue", "revocation"] {
        let c = super::host_configuration_tests::fixture();
        let (mut t, r) = fixture();
        t.expected_rules = hp::rules_fingerprint(&c.profile.rules).unwrap();
        let mut replies = access();
        let mut graph = super::host_configuration_tests::graph(&c, false);
        if case == "missing_flag" {
            graph["data"]["repository"]["ref"]["branchProtectionRule"]
                .as_object_mut()
                .unwrap()
                .remove("restrictsPushes");
        }
        if case == "queue" {
            graph["data"]["repository"]["mergeQueue"]["id"] = json!("MQ_other");
        }
        for round in 0..if matches!(case, "missing_flag" | "queue") {
            1
        } else {
            2
        } {
            replies.push(response(200, group_tests::groups()));
            if case == "drift" && round == 1 {
                graph["data"]["repository"]["ref"]["branchProtectionRule"]["id"] =
                    json!("BPR_replaced");
            }
            let mut pages = super::host_configuration_tests::round(&c, graph.clone());
            if case == "missing_flag" {
                pages.truncate(1);
            }
            replies.extend(pages);
            if case != "missing_flag" {
                replies.push(response(200, review()));
            }
        }
        replies.push(if case == "revocation" {
            response(503, json!({"message":"synthetic revoke failure"}))
        } else {
            revoked()
        });
        let (connection, h) = host(replies).await;
        let out = connection
            .observe_candidate_configuration(&binding(), &t, &r.target, false)
            .await;
        let requests = h.await.unwrap();
        assert!(request_text(requests.last().unwrap()).starts_with("DELETE /installation/token "));
        match case {
            "drift" => assert!(
                out.unwrap()
                    .observation
                    .host_policy
                    .unwrap()
                    .capture_reasons(&t)
                    .unwrap()
                    .contains("capture_changed")
            ),
            "revocation" => assert!(!out.unwrap().observation.token_revoked),
            _ => assert!(out.is_err(), "{case}"),
        }
    }
}
