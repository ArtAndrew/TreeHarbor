use super::*;

#[tokio::test]
async fn required_group_check_targets_candidate_and_stops_on_membership_drift() {
    use super::super::check_publication_tests as checks;
    use treeharbor_github_contract::{
        check_publication as cp, publication, queue::QueueObservation,
    };
    let f = checks::fixture();
    let retained: QueueObservation = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../github-contract/tests/fixtures/queue-groups-v4.json"
    )))
    .unwrap();
    let mut q = target();
    q.expected_rules =
        serde_json::from_value(f["report"]["preflight"]["policy_before"]["rules"].clone()).unwrap();
    let mut t = checks::target();
    t.source = cp::Source::Group {
        queue: Box::new(q),
        target: Box::new(publication::Target {
            membership_sequence: 1,
            member: retained.group_capture.unwrap().before.members.remove(0),
        }),
    };
    for drift in [false, true] {
        let mut responses = checks::access(true);
        let mut after = groups();
        if drift {
            let p = &mut after["data"]["repository"]["pullRequest"];
            p["isInMergeQueue"] = json!(false);
            p["mergeQueueEntry"] = Value::Null;
            p["mergeQueue"]["entries"]["totalCount"] = json!(0);
            p["mergeQueue"]["entries"]["nodes"] = json!([]);
        }
        for g in [groups(), after] {
            responses.extend([
                response(200, g),
                response(200, f["raw_protection"].clone()),
                response(200, f["raw_rules"].clone()),
            ]);
        }
        if !drift {
            responses.extend([
                response(201, checks::run(&t)),
                response(200, checks::run(&t)),
            ]);
        }
        responses.push(revoked());
        let (c, h) = host(responses).await;
        let result = c
            .publish_check(&binding(), &t, |p| {
                assert!(p.eligible(&t));
                assert!(!drift);
                async { Ok(()) }
            })
            .await;
        if drift {
            assert!(result.err().unwrap().mutation_not_sent);
        } else {
            let report = result.unwrap().report;
            assert_eq!(
                report.check.unwrap().head_revision,
                format!("git:sha1:{}", "33".repeat(20))
            );
            // This is the queue's combined candidate, never the PR source head.
            assert_ne!(t.source.head().unwrap(), t.source.pull().head_revision);
        }
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), if drift { 11 } else { 13 });
        for i in [4, 7] {
            assert!(request_text(&requests[i]).starts_with("POST /graphql "));
            assert!(!request_text(&requests[i]).contains("mutation "));
        }
    }
}

pub(super) fn groups() -> Value {
    let mut g = queued();
    let r = &mut g["data"]["repository"];
    r["ref"] = json!({"target":{"oid":"88".repeat(20),"tree":{"oid":"99".repeat(20)}}});
    let p = &mut r["pullRequest"];
    p["mergeQueue"]
        .as_object_mut()
        .unwrap()
        .remove("configuration");
    let mut e = p["mergeQueueEntry"].clone();
    e["position"] = json!(1);
    e["pullRequest"]["baseRefName"] = json!("main");
    e["pullRequest"]["headRepository"] = json!({"databaseId":11});
    e["pullRequest"]["baseRepository"] = json!({"databaseId":11});
    e["headCommit"]["parents"] = json!({"totalCount":2,"pageInfo":{"hasNextPage":false,"hasPreviousPage":false},"nodes":[e["baseCommit"],{"oid":"11".repeat(20),"tree":{"oid":"77".repeat(20)}}]});
    p["mergeQueueEntry"] = e.clone();
    p["mergeQueue"]["entries"] = json!({"totalCount":1,"pageInfo":{"hasNextPage":false,"hasPreviousPage":false},"nodes":[e]});
    g
}
#[tokio::test]
async fn groups_read_complete_membership_twice_and_do_not_request_write_settings() {
    let mut responses = access(false);
    responses.extend([response(200, groups()), response(200, groups()), revoked()]);
    let (c, h) = host(responses).await;
    let result = c.observe_queue_groups(&binding(), &target()).await.unwrap();
    let g = result.observation.group_capture.as_ref().unwrap();
    assert!(g.consistent(&target()));
    assert!(g.after.members[0].has_exact_merge_parents());
    assert_eq!(
        g.after.main.revision,
        format!("git:sha1:{}", "88".repeat(20))
    );
    assert!(result.observation.token_revoked);
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 7);
    for i in [4, 5] {
        let text = request_text(&requests[i]);
        let v: Value = serde_json::from_str(text.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(v["variables"]["base"], "refs/heads/main");
        let q = v["query"].as_str().unwrap();
        assert!(!q.contains("mutation"));
        assert!(!q.contains("configuration"));
    }
    let mint: Value =
        serde_json::from_str(request_text(&requests[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert!(
        mint["permissions"]
            .as_object()
            .unwrap()
            .values()
            .all(|v| v == "read")
    );
}
#[tokio::test]
async fn groups_reject_truncated_null_foreign_and_ambiguous_connections() {
    for case in [
        "page",
        "previous",
        "count",
        "null",
        "queue",
        "base-repo",
        "parents",
        "duplicate",
        "missing-target",
        "errors",
    ] {
        let mut g = groups();
        let q = &mut g["data"]["repository"]["pullRequest"]["mergeQueue"]["entries"];
        match case {
            "page" => q["pageInfo"]["hasNextPage"] = json!(true),
            "previous" => q["pageInfo"]["hasPreviousPage"] = json!(true),
            "count" => q["totalCount"] = json!(2),
            "null" => q["nodes"][0] = Value::Null,
            "queue" => q["nodes"][0]["mergeQueue"]["id"] = json!("MQ_foreign"),
            "base-repo" => q["nodes"][0]["pullRequest"]["baseRepository"]["databaseId"] = json!(12),
            "parents" => {
                q["nodes"][0]["headCommit"]["parents"]["pageInfo"]["hasNextPage"] = json!(true)
            }
            "duplicate" => {
                q["totalCount"] = json!(2);
                let e = q["nodes"][0].clone();
                q["nodes"].as_array_mut().unwrap().push(e);
            }
            "missing-target" => {
                q["totalCount"] = json!(0);
                q["nodes"] = json!([]);
            }
            "errors" => g["errors"] = json!([{"message":"incomplete"}]),
            _ => unreachable!(),
        }
        let mut responses = access(false);
        responses.extend([response(200, g), revoked()]);
        let (c, h) = host(responses).await;
        assert!(
            c.observe_queue_groups(&binding(), &target()).await.is_err(),
            "{case}"
        );
        let req = h.await.unwrap();
        assert_eq!(req.len(), 6);
        assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn groups_preserve_drift_and_empty_queue_without_claiming_old_membership() {
    let mut after = groups();
    let p = &mut after["data"]["repository"]["pullRequest"];
    p["isInMergeQueue"] = json!(false);
    p["mergeQueueEntry"] = Value::Null;
    p["mergeQueue"]["entries"]["totalCount"] = json!(0);
    p["mergeQueue"]["entries"]["nodes"] = json!([]);
    let mut responses = access(false);
    responses.extend([response(200, groups()), response(200, after), revoked()]);
    let (c, h) = host(responses).await;
    let r = c.observe_queue_groups(&binding(), &target()).await.unwrap();
    let g = r.observation.group_capture.as_ref().unwrap();
    assert!(!g.consistent(&target()));
    assert_eq!(g.before.members.len(), 1);
    assert!(g.after.members.is_empty());
    assert_eq!(r.observation.state(&target()), "uncertain");
    h.await.unwrap();
}
