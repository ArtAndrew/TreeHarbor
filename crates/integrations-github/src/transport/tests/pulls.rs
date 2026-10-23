use super::*;
use crate::{ExpectedPull, PullIdentity, PullTarget};
use uuid::Uuid;
fn target() -> PullTarget {
    let operation = Uuid::from_bytes([3; 16]);
    PullTarget {
        operation,
        branch_id: Uuid::from_bytes([4; 16]),
        git_revision: format!("git:sha1:{}", "11".repeat(20)),
        base: "main".into(),
        title: "Update agent change".into(),
        body: format!("Selected change\n\n<!-- treeharbor-pull:{operation} -->"),
        draft: true,
        previous: None,
    }
}
fn pull(t: &PullTarget) -> Value {
    json!({"number":17,"node_id":"PR_test_17","title":t.title,"body":t.body,"draft":t.draft,"state":"open","merged":false,
        "head":{"ref":t.head(),"sha":t.git_revision.strip_prefix("git:sha1:").unwrap(),"repo":{"id":11}},
        "base":{"ref":t.base,"sha":"22".repeat(20),"repo":{"id":11}}})
}
fn access(write: bool) -> Vec<Vec<u8>> {
    let mut t = token();
    t["permissions"]["pull_requests"] = json!(if write { "write" } else { "read" });
    vec![
        response(200, installation()),
        response(201, t),
        response(200, json!({"total_count":1,"repositories":[{"id":11}]})),
        response(200, repository()),
    ]
}
fn reference(t: &PullTarget) -> Value {
    json!({"ref":format!("refs/heads/{}",t.head()),"object":{"type":"commit","sha":t.git_revision.strip_prefix("git:sha1:").unwrap()}})
}
fn body(request: &[u8]) -> Value {
    serde_json::from_str(request_text(request).split("\r\n\r\n").nth(1).unwrap()).unwrap()
}

#[tokio::test]
async fn pull_create_lost_response_reconciles_one_post_and_exact_scoped_identity() {
    let t = target();
    let created = pull(&t);
    let mut responses = access(true);
    responses.extend([
        response(200, json!([])),
        response(200, reference(&t)),
        vec![],
        response(200, json!([created.clone()])),
        response(200, created),
        revoked(),
    ]);
    let (c, host) = host(responses).await;
    let result = c
        .publish_pull(&binding(), &t, || async { Ok(()) })
        .await
        .unwrap();
    let p = result.pull.as_ref().unwrap();
    assert!(t.matches(p));
    assert!(result.token_revoked);
    assert_eq!(p.identity.number, 17);
    let requests = host.await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| request_text(r).starts_with("POST /repos/example-org/demo/pulls "))
            .count(),
        1
    );
    assert_eq!(
        body(&requests[1]),
        json!({"repository_ids":[11],"permissions":{"contents":"read","metadata":"read","pull_requests":"write"}})
    );
    assert_eq!(
        body(&requests[6]),
        json!({"head":t.head(),"base":"main","title":t.title,"body":t.body,"draft":true,"maintainer_can_modify":false})
    );
    assert!(request_text(&requests[4]).contains("state=all&head=example-org%3Atreeharbor%2F"));
    assert!(!serde_json::to_string(&result).unwrap().contains(TEST_TOKEN));
}

#[tokio::test]
async fn pull_recovery_reads_closed_and_merged_results_without_any_pr_mutation() {
    let t = target();
    let mut p = pull(&t);
    p["state"] = json!("closed");
    p["merged"] = json!(true);
    let mut responses = access(false);
    responses.extend([
        response(200, json!([p.clone()])),
        response(200, p),
        response(500, json!({"message":"revoke failed"})),
    ]);
    let (c, host) = host(responses).await;
    let result = c.observe_pull(&binding(), &t).await.unwrap();
    assert_eq!(result.pull.as_ref().unwrap().state, "closed");
    assert!(result.pull.as_ref().unwrap().merged);
    assert!(!result.token_revoked);
    let requests = host.await.unwrap();
    assert!(
        requests
            .iter()
            .all(|r| !request_text(r).starts_with("POST /repos/")
                && !request_text(r).starts_with("PATCH "))
    );
    assert_eq!(body(&requests[1])["permissions"]["pull_requests"], "read");
}

#[tokio::test]
async fn explicit_pull_update_patches_only_selected_text_and_recovers_lost_response() {
    let mut t = target();
    let old = pull(&t);
    t.previous = Some(ExpectedPull {
        identity: PullIdentity {
            number: 17,
            node_id: "PR_test_17".into(),
        },
        title: t.title.clone(),
        body: t.body.clone(),
        draft: true,
    });
    t.title = "Updated title".into();
    t.body = format!("New details\n\n{}", t.marker());
    let updated = pull(&t);
    let mut responses = access(true);
    responses.extend([
        response(200, old),
        vec![],
        response(200, updated),
        revoked(),
    ]);
    let (c, host) = host(responses).await;
    let result = c
        .publish_pull(&binding(), &t, || async { Ok(()) })
        .await
        .unwrap();
    assert!(t.matches(result.pull.as_ref().unwrap()));
    let requests = host.await.unwrap();
    assert!(request_text(&requests[5]).starts_with("PATCH /repos/example-org/demo/pulls/17 "));
    assert_eq!(body(&requests[5]), json!({"title":t.title,"body":t.body}));
    assert_eq!(
        requests
            .iter()
            .filter(|r| request_text(r).starts_with("PATCH "))
            .count(),
        1
    );
}

#[tokio::test]
async fn pull_update_refuses_changed_text_wrong_identity_closed_state_and_moved_head() {
    for field in ["title", "node_id", "state", "head"] {
        let mut t = target();
        let mut old = pull(&t);
        t.previous = Some(ExpectedPull {
            identity: PullIdentity {
                number: 17,
                node_id: "PR_test_17".into(),
            },
            title: t.title.clone(),
            body: t.body.clone(),
            draft: true,
        });
        t.title = "New title".into();
        match field {
            "title" => old["title"] = json!("Human edit"),
            "node_id" => old["node_id"] = json!("PR_other"),
            "state" => old["state"] = json!("closed"),
            _ => old["head"]["sha"] = json!("33".repeat(20)),
        };
        let mut responses = access(true);
        responses.extend([response(200, old), revoked()]);
        let (c, host) = host(responses).await;
        assert_eq!(
            c.publish_pull(&binding(), &t, || async { Ok(()) })
                .await
                .unwrap_err(),
            Error::PullChanged
        );
        assert_eq!(host.await.unwrap().len(), 6);
    }
}

#[tokio::test]
async fn pull_search_never_adopts_foreign_or_ambiguous_results_or_an_unrelated_open_pr() {
    for kind in ["foreign", "ambiguous", "unrelated", "head", "base"] {
        let t = target();
        let mut p = pull(&t);
        match kind {
            "foreign" => p["head"]["repo"]["id"] = json!(99),
            "unrelated" => p["body"] = json!("Human PR"),
            "head" => p["head"]["ref"] = json!("other"),
            "base" => p["base"]["ref"] = json!("other"),
            _ => {}
        }
        let list = if kind == "ambiguous" {
            let mut other = p.clone();
            other["number"] = json!(18);
            other["node_id"] = json!("PR_other");
            json!([p, other])
        } else {
            json!([p])
        };
        let mut responses = access(true);
        responses.extend([response(200, list), revoked()]);
        let (c, host) = host(responses).await;
        assert!(
            c.publish_pull(&binding(), &t, || async { Ok(()) })
                .await
                .is_err()
        );
        assert_eq!(host.await.unwrap().len(), 6);
    }
}

#[tokio::test]
async fn pull_search_paginates_closed_history_and_fails_on_repeated_entries() {
    for repeated in [false, true] {
        let t = target();
        let mut page = vec![];
        for number in 1..=5 {
            let mut p = pull(&t);
            p["number"] = json!(number);
            p["node_id"] = json!(format!("PR_{number}"));
            p["body"] = json!("previous intent");
            p["state"] = json!("closed");
            page.push(p);
        }
        let mut responses = access(false);
        responses.push(response(200, json!(page)));
        if repeated {
            responses.push(response(200, json!([page[0]])));
        } else {
            responses.push(response(200, json!([pull(&t)])));
            responses.push(response(200, pull(&t)));
        }
        responses.push(revoked());
        let (c, host) = host(responses).await;
        let result = c.observe_pull(&binding(), &t).await;
        if repeated {
            assert_eq!(result.unwrap_err(), Error::PullSearch);
        } else {
            assert!(t.matches(result.unwrap().pull.as_ref().unwrap()));
        }
        let requests = host.await.unwrap();
        assert!(request_text(&requests[5]).contains("page=2"));
    }
}

#[tokio::test]
async fn pull_guards_stop_before_mint_or_mutation_and_read_scope_cannot_write() {
    for stop in [1, 2] {
        let t = target();
        let mut responses = if stop == 1 { vec![] } else { access(true) };
        if stop == 2 {
            responses.extend([
                response(200, json!([])),
                response(200, reference(&t)),
                revoked(),
            ]);
        }
        let (c, host) = host(responses).await;
        let mut calls = 0;
        let result = c
            .publish_pull(&binding(), &t, || {
                calls += 1;
                std::future::ready(if calls == stop {
                    Err(Error::PublicationStopped)
                } else {
                    Ok(())
                })
            })
            .await;
        assert_eq!(result.unwrap_err(), Error::PublicationStopped);
        assert_eq!(calls, stop);
        assert_eq!(host.await.unwrap().len(), if stop == 1 { 0 } else { 7 });
    }
    let mut responses = access(false);
    responses.truncate(2);
    responses.push(revoked());
    let (c, host) = host(responses).await;
    assert_eq!(
        c.publish_pull(&binding(), &target(), || async { Ok(()) })
            .await
            .unwrap_err(),
        Error::TokenScope
    );
    assert_eq!(host.await.unwrap().len(), 3);
}

#[tokio::test]
async fn unknown_create_with_absent_result_is_not_repeated_and_metadata_never_retargets() {
    let t = target();
    let mut responses = access(true);
    responses.extend([
        response(200, json!([])),
        response(200, reference(&t)),
        vec![],
        response(200, json!([])),
        revoked(),
    ]);
    let (c, host) = host(responses).await;
    assert!(
        c.publish_pull(&binding(), &t, || async { Ok(()) })
            .await
            .unwrap()
            .pull
            .is_none()
    );
    assert_eq!(
        host.await
            .unwrap()
            .iter()
            .filter(|r| request_text(r).starts_with("POST /repos/"))
            .count(),
        1
    );
    for base in ["main?x=1", "../main", "main.lock", "/main", "main/../other"] {
        let mut t = t.clone();
        t.base = base.into();
        assert!(t.validate().is_err());
    }
    let mut t = t;
    t.body = "unmarked".into();
    assert!(t.validate().is_err());
}
