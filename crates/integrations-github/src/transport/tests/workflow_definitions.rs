use super::*;
use treeharbor_github_contract::workflow_definitions as wd;
fn fixture() -> wd::Report {
    let v: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/workflow-definition-v7.json"
    ))
    .unwrap();
    serde_json::from_value(v["workflow_definition"].clone()).unwrap()
}
fn graph() -> Value {
    let d = fixture();
    let f = d.file;
    json!({"data":{"repository":{"databaseId":11,"object":{"__typename":"Commit","oid":"33".repeat(20),"tree":{"oid":"66".repeat(20)},"file":{"path":f.source.path,"mode":f.mode,"oid":f.blob.strip_prefix("git:sha1:").unwrap(),"size":f.byte_size,"object":{"__typename":"Blob","oid":f.blob.strip_prefix("git:sha1:").unwrap(),"byteSize":f.byte_size,"isBinary":false,"isTruncated":false,"text":f.content}}}}}})
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
async fn workflow_file_uses_fixed_commit_query_read_only_token_and_golden_content() {
    let mut r = access();
    r.extend([response(200, graph()), revoked()]);
    let (c, h) = host(r).await;
    let out = c
        .observe_workflow_definition(&binding(), &target(), &fixture().target)
        .await
        .unwrap();
    assert_eq!(out.observation.workflow_definition, Some(fixture()));
    assert!(out.observation.token_revoked);
    let requests = h.await.unwrap();
    assert_eq!(requests.len(), 6);
    let body = |i: usize| {
        serde_json::from_str::<Value>(request_text(&requests[i]).split_once("\r\n\r\n").unwrap().1)
            .unwrap()
    };
    assert_eq!(
        body(1)["permissions"],
        json!({"contents":"read","metadata":"read","pull_requests":"read","checks":"read"})
    );
    assert_eq!(
        body(4)["variables"],
        json!({"owner":"example-org","name":"demo","revision":"33".repeat(20),"path":".github/workflows/check.yml"})
    );
    assert!(request_text(&requests[4]).starts_with("POST /graphql "));
    assert!(request_text(&requests[5]).starts_with("DELETE /installation/token "));
}
#[tokio::test]
async fn workflow_file_rejects_foreign_or_incomplete_provider_bytes_and_revokes_token() {
    for case in [
        "repo",
        "type",
        "commit",
        "path",
        "mode",
        "oid",
        "binary",
        "truncated",
        "text",
        "size",
        "blob-size",
        "no-text",
        "error",
        "missing",
    ] {
        let mut v = graph();
        match case {
            "repo" => v["data"]["repository"]["databaseId"] = json!(12),
            "error" => v["errors"] = json!([{"message":"partial"}]),
            "missing" => v["data"]["repository"]["object"] = Value::Null,
            other => {
                let c = &mut v["data"]["repository"]["object"];
                match other {
                    "type" => c["__typename"] = json!("Tree"),
                    "commit" => c["oid"] = json!("ab".repeat(20)),
                    other => {
                        let f = &mut c["file"];
                        match other {
                            "path" => f["path"] = json!(".github/workflows/other.yml"),
                            "mode" => f["mode"] = json!(0o120000),
                            "oid" => f["oid"] = json!("ab".repeat(20)),
                            "size" => f["size"] = json!(1),
                            other => {
                                let b = &mut f["object"];
                                match other {
                                    "binary" => b["isBinary"] = json!(true),
                                    "truncated" => b["isTruncated"] = json!(true),
                                    "text" => b["text"] = json!("changed"),
                                    "blob-size" => b["byteSize"] = json!(1),
                                    "no-text" => b["text"] = Value::Null,
                                    _ => unreachable!(),
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut r = access();
        r.extend([response(200, v), revoked()]);
        let (c, h) = host(r).await;
        assert!(
            c.observe_workflow_definition(&binding(), &target(), &fixture().target)
                .await
                .is_err(),
            "{case}"
        );
        assert_eq!(h.await.unwrap().len(), 6);
    }
}
