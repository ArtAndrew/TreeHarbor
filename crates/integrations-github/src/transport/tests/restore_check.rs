use super::*;
use treeharbor_github_contract::restore_check::CheckIdentity;
fn identity() -> CheckIdentity {
    CheckIdentity {
        id: 73,
        node_id: "opaque-check-fixture-73".into(),
    }
}

#[tokio::test]
async fn restore_check_retains_changed_results_without_writing_or_validating_new_code() {
    let t = target();
    for case in [
        "matches", "changed", "pending", "failure", "head", "external", "app",
    ] {
        let mut after = run(&t);
        match case {
            "changed" | "failure" => after["conclusion"] = json!("failure"),
            "pending" => {
                after["status"] = json!("in_progress");
                after["conclusion"] = Value::Null;
            }
            "head" => after["head_sha"] = json!("44".repeat(20)),
            "external" => after["external_id"] = Value::Null,
            "app" => after["app"]["id"] = json!(8),
            _ => (),
        }
        let before = if case == "changed" {
            run(&t)
        } else {
            after.clone()
        };
        let mut replies = access(false);
        replies.extend([response(200, before), response(200, after), revoked()]);
        let (c, h) = host(replies).await;
        let result = c
            .observe_restore_check(&binding(), &t, &identity())
            .await
            .unwrap();
        assert_eq!(
            result.report.classification(&t, &identity()).unwrap(),
            match case {
                "matches" => "matches_retained_target",
                "changed" => "changed_during_capture",
                _ => "differs_from_retained_target",
            }
        );
        assert!(result.report.token_revoked);
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 7);
        for r in &requests[4..6] {
            assert!(request_text(r).starts_with("GET /repos/example-org/demo/check-runs/73 "));
        }
        assert_eq!(
            body(&requests[1])["permissions"],
            json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read"})
        );
        assert!(request_text(&requests[6]).starts_with("DELETE /installation/token "));
    }
}

#[tokio::test]
async fn restore_check_rejects_missing_or_foreign_exact_identity_and_revokes() {
    let t = target();
    for case in [
        "id",
        "node",
        "external_id",
        "conclusion",
        "revision",
        "404",
        "network",
    ] {
        let mut r = run(&t);
        match case {
            "id" => r["id"] = json!(74),
            "node" => r["node_id"] = json!("foreign-node"),
            "external_id" | "conclusion" => {
                r.as_object_mut().unwrap().remove(case);
            }
            "revision" => r["head_sha"] = json!("bad"),
            _ => (),
        }
        let mut replies = access(false);
        replies.push(response(200, run(&t)));
        replies.push(if case == "network" {
            Vec::new()
        } else {
            response(if case == "404" { 404 } else { 200 }, r)
        });
        replies.push(revoked());
        let (c, h) = host(replies).await;
        assert!(
            c.observe_restore_check(&binding(), &t, &identity())
                .await
                .is_err(),
            "{case}"
        );
        let requests = h.await.unwrap();
        assert_eq!(requests.len(), 7);
        assert!(request_text(&requests[6]).starts_with("DELETE /installation/token "));
    }
}
