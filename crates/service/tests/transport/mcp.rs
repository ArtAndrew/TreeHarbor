use super::*;
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, RoleClient,
    model::{CallToolRequestParams, ClientConfig, ProtocolVersion},
    service::RunningService,
    transport::TokioChildProcess,
};
type Mcp = RunningService<RoleClient, ClientConfig>;
async fn connect(f: &Fixture, credential: &std::path::Path, modern: bool) -> Mcp {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    command
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["mcp", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(credential)
        .kill_on_drop(true);
    ClientConfig::default()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve_with_lifecycle(
            TokioChildProcess::new(command).unwrap(),
            if modern {
                ClientLifecycleMode::Discover {
                    preferred_versions: vec![ProtocolVersion::V_2026_07_28],
                }
            } else {
                ClientLifecycleMode::Initialize
            },
        )
        .await
        .unwrap()
}
async fn call(m: &Mcp, name: &str, args: Value) -> Value {
    let reply = m
        .call_tool(
            CallToolRequestParams::new(name.to_owned())
                .with_arguments(args.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let value = reply.structured_content.unwrap();
    assert_eq!(
        reply.is_error.unwrap_or(false),
        value["http_status"].as_u64().is_none_or(|s| s >= 400),
        "{value}"
    );
    assert_eq!(value["adapter_version"], 1);
    value
}
async fn good(m: &Mcp, name: &str, args: Value) -> Value {
    let r = call(m, name, args).await;
    assert_eq!(r["http_status"], 200, "{r}");
    r["response"]["result"].clone()
}
fn agent(f: &Fixture, name: &str) -> PathBuf {
    if name != "agent-a" {
        f.admin_ok(&[
            "principal-create",
            "--tenant",
            f.scope.tenant(),
            "--principal",
            name,
            "--kind",
            "agent",
        ]);
    }
    let path = f.dir.path().join(format!("mcp-{name}.json"));
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        name,
        "--permission",
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
        "--permission",
        "queue.request",
    ]);
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        name,
        "--permission",
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
        "--permission",
        "queue.request",
        "--output",
        path.to_str().unwrap(),
    ]);
    path
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires PostgreSQL: two actual MCP child clients, HTTP/server restart and immutable proposals; runtime state is an explicit fixture"]
async fn mcp_two_clients_share_api_authority_and_preserve_exact_proposals_after_restart() {
    let mut f = Fixture::new().await;
    f.importer();
    f.config.pool_size = 4;
    f.config.request_timeout_ms = 15000;
    f.save_config();
    let source = f.dir.path().join("mcp-source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("same.txt"), b"before").unwrap();
    let pack = f.dir.path().join("mcp-pack");
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["source-pack", "--directory"])
        .arg(&source)
        .arg("--output")
        .arg(&pack)
        .args([
            "--path",
            "same.txt",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let mut server = f.start().await;
    let output = f.cli(&[
        "source-import",
        "--pack",
        pack.to_str().unwrap(),
        "--state",
        f.dir.path().join("mcp-import.json").to_str().unwrap(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ]);
    assert!(output.status.success());
    let base: Value = serde_json::from_slice(&output.stdout).unwrap();
    // Only the separately authorized operator establishes policy and main.
    f.credential_path = native::role(
        &mut f,
        "importer",
        None,
        &["read", "repository.write", "policy.write"],
    );
    let digest = treeharbor_core::ObjectId::from_bytes([7; 32]).to_string();
    let policy = native::ok(native::native(
        &f,
        json!({"action":"policy_put","key":"mcp-policy","epoch":1,"policy":{"version":1,"checks":[{"name":"tests","definition":digest,"environment":digest,"toolchain":digest,"dependencies":digest,"validators":["ci"]}],"reviewers":["reviewer"],"approvals":1}}),
    ));
    native::ok(native::native(
        &f,
        json!({"action":"ref_create","key":"mcp-ref","epoch":1,"name":"main","head":base["result"]["revision"],"policy":policy["policy"]}),
    ));
    let a_path = agent(&f, "agent-a");
    let b_path = agent(&f, "agent-b");
    let a = connect(&f, &a_path, true).await;
    let b = connect(&f, &b_path, false).await;
    let tools = a.list_all_tools().await.unwrap();
    assert_eq!(tools.len(), 20);
    for name in [
        "validation_report",
        "land",
        "run_request",
        "admin",
        "command",
    ] {
        assert!(!tools.iter().any(|t| t.name == name));
        assert!(a.call_tool(CallToolRequestParams::new(name)).await.is_err());
    }
    let credential = Credential::load(&a_path).unwrap();
    let direct = client::send(
        &f.config.socket,
        &credential.token,
        credential
            .request(json!({"method":"repository_info"}))
            .to_string()
            .into_bytes(),
        Duration::from_secs(5),
    )
    .await
    .unwrap();
    assert_eq!(
        call(&a, "repository_info", json!({})).await["response"],
        direct.body
    );
    assert!(
        a.call_tool(
            CallToolRequestParams::new("repository_info")
                .with_arguments(json!({"tenant":"foreign"}).as_object().unwrap().clone())
        )
        .await
        .is_err()
    );
    good(&a,"history",json!({"authority":"standalone","epoch":1,"history_query":{"kind":"log","revision":base["result"]["revision"],"after":null,"limit":20,"first_parent":false}})).await;
    good(&a,"review_diff",json!({"authority":"standalone","epoch":1,"diff_query":{"kind":"revisions","before":base["result"]["revision"],"after":base["result"]["revision"]},"after":null,"limit":20,"text":false})).await;
    let profile = good(
        &a,
        "source_profile_put",
        json!({"key":"mcp-profile","authority":"standalone","epoch":1,"profile":{"kind":"full"}}),
    )
    .await;
    let mut workspaces = vec![];
    let mut runs = vec![];
    let mut uploads = vec![];
    let old = Object::Blob(b"before".to_vec()).id().unwrap().to_string();
    for (i, m) in [&a, &b].iter().enumerate() {
        let w = Uuid::new_v4();
        let run = Uuid::new_v4();
        let upload = Uuid::new_v4();
        let create = json!({"key":format!("mcp-w-{i}"),"authority":"standalone","epoch":1,"workspace":w,"base":base["result"]["revision"],"profile_digest":profile["profile_digest"]});
        let created = good(m, "workspace_create", create.clone()).await;
        assert_eq!(created, good(m, "workspace_create", create).await);
        let file=call(m,"workspace_read_files",json!({"workspace":w,"generation":1,"authority":"standalone","epoch":1,"operation":{"kind":"file","path_hex":"73616d652e747874"}})).await;
        assert_eq!(file["response"]["content_hex"], "6265666f7265");
        // Only the transport test supplies runtime facts. Actual isolation and execution
        // remain separate mandatory gVisor acceptance, not claimed by this fixture.
        f.sql.execute("UPDATE th_workspaces SET state='in_use',runtime='fixture',node='fixture' WHERE workspace=$1",&[&w]).await.unwrap();
        f.sql.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state) SELECT tenant,repository,$1,workspace,generation,epoch,principal,'fixture','fixture',runtime,'running' FROM th_workspaces WHERE workspace=$2",&[&run,&w]).await.unwrap();
        let begin = json!({"key":format!("mcp-b-{i}"),"authority":"standalone","epoch":1,"upload":upload,"run":run,"generation":1,"max_objects":4,"max_bytes":4096,"ttl_seconds":600});
        good(m, "change_begin", begin).await;
        let blob = good(
            m,
            "change_put",
            json!({"upload":upload,"content_hex":if i==0 {"6100ff"} else {"6200ff"}}),
        )
        .await;
        let submit = json!({"key":format!("mcp-s-{i}"),"authority":"standalone","epoch":1,"upload":upload,"changeset":Uuid::new_v4(),"previous_version":0,"operations":[{"path_hex":"73616d652e747874","expected":{"mode":33188,"object":old},"replacement":{"mode":33188,"object":blob["object"]}}]});
        let accepted = good(m, "changeset_submit", submit.clone()).await;
        assert_eq!(accepted, good(m, "changeset_submit", submit.clone()).await);
        let entry = Uuid::new_v4();
        let enqueue = json!({"key":format!("mcp-q-{i}"),"epoch":1,"entry":entry,"target":"main","changeset":submit["changeset"],"version":1});
        let queued = good(m, "queue_request", enqueue.clone()).await;
        assert_eq!(queued["state"], "queued");
        assert_eq!(queued, good(m, "queue_request", enqueue).await);
        assert_eq!(
            queued,
            good(m, "queue_status", json!({"entry":entry})).await
        );
        let cancelled = good(
            m,
            "queue_cancel",
            json!({"key":format!("mcp-cancel-{i}"),"epoch":1,"entry":entry}),
        )
        .await;
        assert_eq!(cancelled["state"], "cancelled");
        assert_eq!(
            native::ok(native::native(
                &f,
                json!({"action":"ref_resolve","name":"main"})
            ))["head"],
            base["result"]["revision"]
        );
        let mut forged = submit;
        forged["validation"] = json!("passed");
        assert!(
            m.call_tool(
                CallToolRequestParams::new("changeset_submit")
                    .with_arguments(forged.as_object().unwrap().clone())
            )
            .await
            .is_err()
        );
        workspaces.push(w);
        runs.push(run);
        uploads.push(upload);
    }
    let other=call(&a,"workspace_read_files",json!({"workspace":workspaces[1],"generation":1,"authority":"standalone","epoch":1,"operation":{"kind":"file","path_hex":"73616d652e747874"}})).await;
    assert!(other["http_status"].as_u64().unwrap() >= 400);
    let stale=call(&a,"workspace_read_files",json!({"workspace":workspaces[0],"generation":2,"authority":"standalone","epoch":1,"operation":{"kind":"file","path_hex":"73616d652e747874"}})).await;
    assert!(stale["http_status"].as_u64().unwrap() >= 400);
    let before = good(&a, "operation_status", json!({"key":"mcp-s-0"})).await;
    a.cancel().await.unwrap();
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    server = f.start().await;
    let a = connect(&f, &a_path, true).await;
    assert_eq!(
        before,
        good(&a, "operation_status", json!({"key":"mcp-s-0"})).await
    );
    for (i, m) in [&a, &b].iter().enumerate() {
        good(m,"workspace_delete",json!({"key":format!("mcp-delete-{i}"),"authority":"standalone","epoch":1,"workspace":workspaces[i],"generation":1})).await;
        let row = f
            .sql
            .query_one(
                "SELECT state FROM th_workspaces WHERE workspace=$1",
                &[&workspaces[i]],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, String>(0), "draining");
        let refused=call(m,"change_begin",json!({"key":format!("mcp-stale-{i}"),"authority":"standalone","epoch":1,"upload":Uuid::new_v4(),"run":runs[i],"generation":1,"max_objects":4,"max_bytes":4096,"ttl_seconds":600})).await;
        assert!(refused["http_status"].as_u64().unwrap() >= 400);
        good(m, "changeset_status", json!({"upload":uploads[i]})).await;
    }
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--revoke-all",
    ]);
    assert_eq!(
        call(&a, "repository_info", json!({})).await["http_status"],
        403
    );
    a.cancel().await.unwrap();
    b.cancel().await.unwrap();
    drop(server);
    f.sql
        .batch_execute(&format!("DROP SCHEMA {} CASCADE", f.config.schema))
        .await
        .unwrap();
}
