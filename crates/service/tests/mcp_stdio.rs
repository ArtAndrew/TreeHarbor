#![cfg(unix)]
//! Independent JSON-RPC wire client: does not use the rmcp client/model encoder.
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
async fn read(reader: &mut BufReader<tokio::process::ChildStdout>) -> Value {
    let mut line = String::new();
    let n = tokio::time::timeout(Duration::from_secs(10), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0 && n < 256 * 1024);
    assert!(!line.contains("thv1_"));
    serde_json::from_str(&line).unwrap()
}
#[tokio::test]
async fn independent_stdio_wire_is_bounded_scoped_and_keeps_stdout_protocol_only() {
    let dir = tempfile::tempdir().unwrap();
    fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let credential = dir.path().join("credential.json");
    treeharbor_service::files::write_private(&credential,json!({"version":1,"tenant":"wire-test","repository":"00000000-0000-4000-8000-000000000001","token_id":"00000000-0000-4000-8000-000000000002","token":format!("thv1_{}","1".repeat(64))}).to_string().as_bytes()).unwrap();
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["mcp", "--socket"])
        .arg(dir.path().join("absent.sock"))
        .arg("--credential-file")
        .arg(&credential)
        .env("PATH", "/treeharbor-no-executables")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let vectors: Value = serde_json::from_str(include_str!("fixtures/mcp-stdio-v1.json")).unwrap();
    input
        .write_all(format!("{}\n", vectors["initialize"]).as_bytes())
        .await
        .unwrap();
    let initialized = read(&mut output).await;
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");
    input
        .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n")
        .await
        .unwrap();
    input
        .write_all(format!("{}\n", vectors["list"]).as_bytes())
        .await
        .unwrap();
    let list = read(&mut output).await;
    assert_eq!(list["id"], 2);
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["name"].clone())
        .collect();
    assert_eq!(json!(names), vectors["tool_names"]);
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        let p = tool["inputSchema"]["properties"].as_object().unwrap();
        for forbidden in [
            "tenant",
            "repository",
            "credential",
            "token",
            "socket",
            "method",
            "action",
        ] {
            assert!(!p.contains_key(forbidden));
        }
    }
    for (name, args) in [
        ("repository_info", json!({"token":"do-not-echo"})),
        ("land", json!({})),
        ("changeset_submit", json!({"validation":"passed"})),
        (
            "change_put",
            json!({"upload":"00000000-0000-4000-8000-000000000001","content_hex":"Aa"}),
        ),
    ] {
        input.write_all(format!("{}\n",json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":args}})).as_bytes()).await.unwrap();
        let refused = read(&mut output).await;
        assert_eq!(refused["error"]["code"], -32602);
        assert!(!refused.to_string().contains("do-not-echo"));
    }
    // A frame beyond the configured cap is discarded without buffering its tail.
    // The next independent message remains usable; no unbounded retry is sent.
    input
        .write_all(format!("{}\n", "x".repeat(256 * 1024)).as_bytes())
        .await
        .unwrap();
    input
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":4,\"method\":\"ping\"}\n")
        .await
        .unwrap();
    assert_eq!(read(&mut output).await["id"], 4);
    input.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"tools/call\",\"params\":{\"name\":\"repository_info\",\"arguments\":{}}}\n").await.unwrap();
    let unavailable = read(&mut output).await;
    assert_eq!(unavailable["result"]["isError"], true);
    assert_eq!(
        unavailable["result"]["structuredContent"]["response"]["outcome_unknown"],
        false
    );
    drop(input);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .await
        .unwrap();
    assert!(!stderr.contains("thv1_") && !stderr.contains("do-not-echo"));
}
