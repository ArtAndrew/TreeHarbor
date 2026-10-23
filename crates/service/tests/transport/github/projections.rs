use super::*;
#[path = "publications.rs"]
mod publications;

fn github(f: &Fixture, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/no-executables")
        .args(["github", args[0], "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(&args[1..])
        .output()
        .unwrap()
}
fn raw_prepare(f: &Fixture, id: Uuid) -> Vec<u8> {
    let body=json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"key":"prepare","epoch":1,"projection":id.to_string()}).to_string().into_bytes();
    let mut request=format!("POST /v1/github/projections/prepare HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",f.credential().token.expose(),body.len()).into_bytes();
    request.extend(body);
    request
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual Connected CLI/export/Git, server kills, trusted run fixture (no live App)"]
async fn accepted_projection_recovers_before_and_after_commit_and_exports_exact_git_parent() {
    let mut f = Fixture::new_with_authority(Authority::Connected).await;
    f.importer();
    f.config.request_timeout_ms = 15000;
    f.config.pool_size = 3;
    f.save_config();
    let owner = f.credential_path.clone();
    let readonly = provision_bridge(&f);
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.read",
        "--permission",
        "host.publish",
    ]);
    let publisher = f.dir.path().join("publisher.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "bridge",
        "--permission",
        "host.publish",
        "--output",
        publisher.to_str().unwrap(),
    ]);
    let source = f.dir.path().join("source");
    git_history_pack(&f, &source);
    let manifest: Value =
        serde_json::from_slice(&fs::read(source.join("manifest.json")).unwrap()).unwrap();
    let original_git = manifest["source_revision"]
        .as_str()
        .unwrap()
        .strip_prefix("git:sha1:")
        .unwrap()
        .to_owned();
    let mut server = f.start().await;
    let imported = result(f.cli(&[
        "source-import",
        "--pack",
        source.to_str().unwrap(),
        "--state",
        f.dir.path().join("import.json").to_str().unwrap(),
        "--authority",
        "connected",
        "--epoch",
        "1",
    ]));
    let profile = result(f.cli(&[
        "profile-put",
        "--key",
        "profile",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--full",
    ]));
    let bind: Value = serde_json::from_str(include_str!(
        "../../../../metadata/tests/fixtures/github-bind-v1.json"
    ))
    .unwrap();
    result(request(&f, &bind));
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
    ]);
    f.credential_path = f.dir.path().join("proposal-agent.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
        "--output",
        f.credential_path.to_str().unwrap(),
    ]);
    let workspace = Uuid::new_v4();
    let run = Uuid::new_v4();
    result(f.cli(&[
        "workspace-create",
        "--key",
        "workspace",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--workspace",
        &workspace.to_string(),
        "--base",
        imported["revision"].as_str().unwrap(),
        "--profile-digest",
        profile["profile_digest"].as_str().unwrap(),
    ]));
    // This supplies lineage, not proof of execution; the actual runtime has its own CI harness.
    f.sql.execute("UPDATE th_workspaces SET state='in_use',runtime='fixture',node='fixture' WHERE workspace=$1",&[&workspace]).await.unwrap();
    f.sql.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state) SELECT tenant,repository,$1,workspace,generation,epoch,principal,'fixture','fixture',runtime,'running' FROM th_workspaces WHERE workspace=$2",&[&run,&workspace]).await.unwrap();
    let upload = Uuid::new_v4();
    let changeset = Uuid::new_v4();
    result(f.cli(&[
        "change-begin",
        "--key",
        "begin",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--upload",
        &upload.to_string(),
        "--run",
        &run.to_string(),
        "--generation",
        "1",
    ]));
    let new_bytes = b"fn main() { /* accepted */ }\n";
    let content = f.dir.path().join("new.rs");
    files::write_private(&content, new_bytes).unwrap();
    let blob = result(f.cli(&[
        "change-put",
        "--upload",
        &upload.to_string(),
        "--file",
        content.to_str().unwrap(),
    ]));
    let old = Object::Blob(b"fn main() {}\n".to_vec()).id().unwrap();
    let operations = f.dir.path().join("operations.json");
    files::write_private(&operations,json!([{"path_hex":"6d61696e2e7273","expected":{"mode":33188,"object":old.to_string()},"replacement":{"mode":33188,"object":blob["object"]}}]).to_string().as_bytes()).unwrap();
    let accepted = result(f.cli(&[
        "change-submit",
        "--key",
        "accept",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--upload",
        &upload.to_string(),
        "--changeset",
        &changeset.to_string(),
        "--operations",
        operations.to_str().unwrap(),
    ]));
    let projection = Uuid::new_v4();
    let requested = json!({"action":"projection_request","key":"projection","epoch":1,"projection":projection.to_string(),"binding":bind["binding"],"specification":{"changeset":changeset.to_string(),"version":1,"title":"Update main","ttl_seconds":3600}});
    assert!(!request(&f, &requested).status.success());
    f.credential_path = owner.clone();
    assert_eq!(result(request(&f, &requested))["state"], "pending");
    let wire = raw_prepare(&f, projection);
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION hold_projection() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({lock}::bigint); RETURN NEW; END $$; CREATE TRIGGER hold_projection BEFORE UPDATE ON th_objects FOR EACH ROW WHEN (OLD.state='uploading' AND NEW.state='live' AND NEW.kind=5) EXECUTE FUNCTION hold_projection()")).await.unwrap();
    let mut partial = UnixStream::connect(&f.config.socket).await.unwrap();
    partial.write_all(&wire).await.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting:bool=f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock & 0xffff_ffff)]).await.unwrap().get(0);
        if waiting {
            break;
        }
        assert!(
            Instant::now() < until,
            "projection did not reach publication boundary"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(partial);
    f.sql
        .query_one("SELECT pg_advisory_unlock($1)", &[&lock])
        .await
        .unwrap();
    f.sql
        .batch_execute(
            "DROP TRIGGER hold_projection ON th_objects; DROP FUNCTION hold_projection()",
        )
        .await
        .unwrap();
    assert_eq!(
        f.sql
            .query_one("SELECT state FROM th_github_projections", &[])
            .await
            .unwrap()
            .get::<_, String>(0),
        "pending"
    );
    let reservation = f
        .sql
        .query_one(
            "SELECT backend,generation,hold_token,native_commit FROM th_github_projections",
            &[],
        )
        .await
        .unwrap();
    let native =
        treeharbor_core::ObjectId::from_bytes(reservation.get::<_, Vec<u8>>(3).try_into().unwrap());
    let store = ManagedStore::open(
        &f.config.storage.as_ref().unwrap().root,
        TenantId::new(f.scope.tenant()).unwrap(),
        "source-1",
    )
    .unwrap();
    assert!(matches!(
        store.get(reservation.get(1), native).unwrap(),
        Object::NativeCommit(_)
    ));
    server = f.start().await;
    let mut lost = UnixStream::connect(&f.config.socket).await.unwrap();
    lost.write_all(&wire).await.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if f.sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE key='prepare'",
                &[],
            )
            .await
            .unwrap()
            .get::<_, i64>(0)
            == 1
        {
            break;
        }
        assert!(Instant::now() < until);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(lost);
    fs::remove_dir_all(source).unwrap();
    server = f.start().await;
    let ready = result(github(
        &f,
        &[
            "prepare",
            "--projection",
            &projection.to_string(),
            "--epoch",
            "1",
            "--key",
            "prepare",
        ],
    ));
    assert_eq!(ready["state"], "ready");
    assert_eq!(ready["snapshot"], accepted["result_snapshot"]);
    let current = f
        .sql
        .query_one(
            "SELECT backend,generation,hold_token,native_commit FROM th_github_projections",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(reservation.get::<_, i64>(1), current.get::<_, i64>(1));
    assert_eq!(reservation.get::<_, Uuid>(2), current.get::<_, Uuid>(2));
    let output = f.dir.path().join("projected");
    let args = [
        "export",
        "--projection",
        &projection.to_string(),
        "--epoch",
        "1",
        "--output",
        output.to_str().unwrap(),
    ];
    assert!(!github(&f, &args).status.success());
    f.credential_path = readonly;
    assert!(!github(&f, &args).status.success());
    f.credential_path = publisher.clone();
    let exported = github(&f, &args);
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    assert!(!github(&f, &args).status.success()); // no overwrite
    let pack = treeharbor_service::pack::Pack::open(&output).unwrap();
    assert_eq!(pack.commit(), native);
    let git_output = f.dir.path().join("git-output");
    let exported = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["git-export", "--pack"])
        .arg(&output)
        .arg("--output")
        .arg(&git_output)
        .args(["--git", "/usr/bin/git"])
        .output()
        .unwrap();
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let restored = f.dir.path().join("restored.git");
    git_reference::git(
        f.dir.path(),
        &[
            "clone",
            "--bare",
            git_output.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    git_reference::git(&restored, &["fsck", "--strict"], b"");
    assert_eq!(
        git_reference::git(&restored, &["show", "main:main.rs"], b""),
        new_bytes
    );
    assert_eq!(
        String::from_utf8(git_reference::git(&restored, &["rev-parse", "main^"], b""))
            .unwrap()
            .trim(),
        original_git
    );
    assert_eq!(
        String::from_utf8(git_reference::git(
            &restored,
            &["rev-list", "--count", "main"],
            b""
        ))
        .unwrap()
        .trim(),
        "3"
    );
    publications::exercise(
        &mut f,
        &mut server,
        &owner,
        &publisher,
        projection,
        native,
        &restored,
    )
    .await;
    f.credential_path = owner;
    result(request(
        &f,
        &json!({"action":"projection_cancel","key":"cancel","epoch":1,"projection":projection.to_string()}),
    ));
    let historical = result(github(
        &f,
        &[
            "prepare",
            "--projection",
            &projection.to_string(),
            "--epoch",
            "1",
            "--key",
            "prepare",
        ],
    ));
    assert_eq!(historical, ready);
    f.credential_path = publisher;
    let credential = f.credential();
    let body=credential.request(json!({"method":"github","request":{"action":"projection_context","epoch":1,"projection":projection.to_string()}}));
    let refused = client::send(
        &f.config.socket,
        &credential.token,
        serde_json::to_vec(&body).unwrap(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(refused.status.as_u16(), 409);
    assert_eq!(refused.body["error"]["code"], "host_projection_state");
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_native_refs", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_bases", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    drop(server);
    f.cleanup().await;
}
