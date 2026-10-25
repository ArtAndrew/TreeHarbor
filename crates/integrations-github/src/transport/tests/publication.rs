use super::*;
use treeharbor_github_contract::publication as p;
fn fixture() -> p::Report {
    let v: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/publication-v8.json"
    ))
    .unwrap();
    serde_json::from_value(v["publication"].clone()).unwrap()
}
fn oid(s: &str) -> &str {
    s.strip_prefix("git:sha1:").unwrap()
}
fn commit(c: &treeharbor_github_contract::queue::GitCommitObservation) -> Value {
    json!({"oid":oid(&c.revision),"tree":{"oid":oid(&c.tree)}})
}
fn graph() -> Value {
    let f = fixture();
    let s = f.before;
    let pull = s.pull;
    let c = s.final_commit.unwrap();
    let mut final_commit = commit(&c.commit);
    final_commit["parents"] = json!({"totalCount":c.parents.len(),"pageInfo":{"hasNextPage":false,"hasPreviousPage":false},"nodes":c.parents.iter().map(commit).collect::<Vec<_>>()});
    let mut main = commit(&s.main);
    main["__typename"] = json!("Commit");
    json!({"data":{"repository":{"databaseId":11,"ref":{"name":"main","prefix":"refs/heads/","target":main},"pullRequest":{"id":pull.node_id,"number":pull.number,"headRefName":pull.head_ref,"headRefOid":oid(&pull.head_revision),"baseRefName":pull.base_ref,"baseRefOid":oid(&pull.base_revision),"state":"MERGED","isDraft":false,"merged":true,"headRepository":{"databaseId":11},"baseRepository":{"databaseId":11},"mergeCommit":final_commit}}}})
}
fn comparison() -> Value {
    let f = fixture();
    let c = f.comparison;
    let rest = |v: &treeharbor_github_contract::queue::GitCommitObservation| json!({"sha":oid(&v.revision),"commit":{"tree":{"sha":oid(&v.tree)}}});
    json!({"base_commit":rest(&c.base),"merge_base_commit":rest(&c.merge_base),"status":c.status,"ahead_by":0,"behind_by":0,"total_commits":0})
}
fn access() -> Vec<Vec<u8>> {
    let mut r = good_responses();
    r.pop();
    let mut t = token();
    t["permissions"]["pull_requests"] = json!("read");
    t["permissions"]["checks"] = json!("read");
    r[1] = response(201, t);
    r
}
#[tokio::test]
async fn publication_uses_immutable_compare_read_scope_and_revokes_token() {
    let mut r = access();
    r.extend([
        response(200, graph()),
        response(200, comparison()),
        response(200, graph()),
        revoked(),
    ]);
    let (c, h) = host(r).await;
    let out = c
        .observe_publication(&binding(), &target(), &fixture().target)
        .await
        .unwrap();
    assert_eq!(out.observation.publication, Some(fixture()));
    assert!(out.observation.token_revoked);
    assert!(
        out.observation
            .publication
            .unwrap()
            .exact_candidate_published(&target())
    );
    let req = h.await.unwrap();
    assert_eq!(req.len(), 8);
    let permissions: Value =
        serde_json::from_str(request_text(&req[1]).split_once("\r\n\r\n").unwrap().1).unwrap();
    assert_eq!(
        permissions["permissions"],
        json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read"})
    );
    let sha = fixture().comparison.base.revision;
    let sha = oid(&sha);
    assert!(request_text(&req[5]).starts_with(&format!(
        "GET /repos/example-org/demo/compare/{sha}...{sha}?per_page=1&page=1 "
    )));
    assert!(request_text(&req[7]).starts_with("DELETE /installation/token "));
}
#[tokio::test]
async fn publication_rejects_incomplete_or_foreign_provider_data() {
    for case in [
        "repo", "pull", "tag", "ref", "kind", "parents", "missing", "partial", "base", "counts",
    ] {
        let mut g = graph();
        let mut compare = comparison();
        match case {
            "repo" => g["data"]["repository"]["databaseId"] = json!(12),
            "pull" => {
                g["data"]["repository"]["pullRequest"]["headRepository"]["databaseId"] = json!(12)
            }
            "tag" => g["data"]["repository"]["ref"]["prefix"] = json!("refs/tags/"),
            "ref" => g["data"]["repository"]["ref"]["name"] = json!("other"),
            "kind" => g["data"]["repository"]["ref"]["target"]["__typename"] = json!("Tag"),
            "parents" => {
                g["data"]["repository"]["pullRequest"]["mergeCommit"]["parents"]["pageInfo"]["hasNextPage"] =
                    json!(true)
            }
            "missing" => g["data"]["repository"]["ref"] = Value::Null,
            "partial" => g["errors"] = json!([{"message":"partial"}]),
            "base" => compare["base_commit"]["sha"] = json!("cd".repeat(20)),
            "counts" => compare["ahead_by"] = json!(1),
            _ => unreachable!(),
        }
        let mut r = access();
        r.push(response(200, g));
        if matches!(case, "base" | "counts") {
            r.extend([response(200, compare), response(200, graph())]);
        }
        r.push(revoked());
        let (c, h) = host(r).await;
        assert!(
            c.observe_publication(&binding(), &target(), &fixture().target)
                .await
                .is_err(),
            "{case}"
        );
        let req = h.await.unwrap();
        assert!(request_text(req.last().unwrap()).starts_with("DELETE /installation/token "));
    }
}
#[tokio::test]
async fn publication_keeps_later_descendants_and_ref_drift_distinct() {
    for drift in [false, true] {
        let mut before = graph();
        before["data"]["repository"]["ref"]["target"]["oid"] = json!("ab".repeat(20));
        before["data"]["repository"]["ref"]["target"]["tree"]["oid"] = json!("ac".repeat(20));
        let mut after = before.clone();
        if drift {
            after["data"]["repository"]["ref"]["target"]["oid"] = json!("ad".repeat(20));
        }
        let mut compare = comparison();
        compare["status"] = json!("ahead");
        compare["ahead_by"] = json!(9);
        compare["total_commits"] = json!(9);
        let mut r = access();
        r.extend([
            response(200, before),
            response(200, compare),
            response(200, after),
            revoked(),
        ]);
        let (c, h) = host(r).await;
        let out = c
            .observe_publication(&binding(), &target(), &fixture().target)
            .await
            .unwrap();
        assert_eq!(
            out.observation
                .publication
                .unwrap()
                .exact_candidate_published(&target()),
            !drift
        );
        let req = h.await.unwrap();
        assert!(
            request_text(&req[5]).contains(&format!("...{}?per_page=1&page=1", "ab".repeat(20)))
        );
    }
}
#[tokio::test]
async fn publication_missing_compare_or_final_read_never_certifies_and_still_revokes() {
    for missing_compare in [true, false] {
        let mut r = access();
        r.push(response(200, graph()));
        if !missing_compare {
            r.push(response(200, comparison()));
        }
        r.extend([response(404, json!({"message":"not found"})), revoked()]);
        let (c, h) = host(r).await;
        assert!(
            c.observe_publication(&binding(), &target(), &fixture().target)
                .await
                .is_err()
        );
        assert!(
            request_text(h.await.unwrap().last().unwrap())
                .starts_with("DELETE /installation/token ")
        );
    }
}
