use super::*;
use treeharbor_core::{Entry, EntryMode, EntryRef, Name};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"].clone()
}
fn entry(name: &[u8], mode: EntryMode, object: &Object) -> Entry {
    Entry {
        name: Name::new(name.to_vec()).unwrap(),
        value: EntryRef {
            mode,
            target: object.id().unwrap(),
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; actual CLI/HTTP/storage, trusted runtime fixture"]
async fn immutable_change_cli_computes_tree_preserves_omitted_files_and_replays_after_server_death()
{
    proposal_roundtrip(Authority::Standalone, false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual API/CLI, no Git or runtime executables"]
async fn external_api_proposal_survives_process_loss_and_close_in_standalone() {
    proposal_roundtrip(Authority::Standalone, true).await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual API/CLI in Connected, no new GitHub credentials"]
async fn external_api_proposal_survives_process_loss_and_close_in_connected() {
    proposal_roundtrip(Authority::Connected, true).await;
}
async fn proposal_roundtrip(authority: Authority, external: bool) {
    let mut f = Fixture::new_with_authority(authority).await;
    f.importer();
    f.config.request_timeout_ms = 15000;
    f.config.pool_size = 3;
    f.save_config();
    let dir = f.dir.path().join("proposal-source");
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(dir.join("src/same.txt"), b"before").unwrap();
    fs::write(dir.join("keep.bin"), b"unchanged\0\xff").unwrap();
    fs::write(dir.join("obsolete.py"), b"gone").unwrap();
    let pack = f.dir.path().join("proposal-pack");
    let packed = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["source-pack", "--directory"])
        .arg(&dir)
        .arg("--output")
        .arg(&pack)
        .args([
            "--path",
            "src",
            "--path",
            "keep.bin",
            "--path",
            "obsolete.py",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(
        packed.status.success(),
        "{}",
        String::from_utf8_lossy(&packed.stderr)
    );
    let mut server = f.start().await;
    let imported = ok(f.cli(&[
        "source-import",
        "--pack",
        pack.to_str().unwrap(),
        "--state",
        f.dir.path().join("proposal-import.json").to_str().unwrap(),
        "--authority",
        authority.as_str(),
        "--epoch",
        "1",
    ]));
    let profile = ok(f.cli(&[
        "profile-put",
        "--key",
        "proposal-profile",
        "--authority",
        authority.as_str(),
        "--epoch",
        "1",
        "--full",
    ]));
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "agent-a",
        "--permission",
        "read",
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
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "change.write",
        "--output",
        f.credential_path.to_str().unwrap(),
    ]);
    let workspace = Uuid::new_v4();
    let run = Uuid::new_v4();
    if external {
        let args = [
            "api-run-create",
            "--key",
            "proposal-workspace",
            "--authority",
            authority.as_str(),
            "--epoch",
            "1",
            "--workspace",
            &workspace.to_string(),
            "--run",
            &run.to_string(),
            "--base",
            imported["revision"].as_str().unwrap(),
            "--profile-digest",
            profile["profile_digest"].as_str().unwrap(),
            "--agent",
            "controller/1",
            "--task",
            "edit-source",
        ];
        let created = ok(f.cli(&args));
        assert_eq!(created, ok(f.cli(&args)));
        assert_eq!(created["execution_observed"], false);
        assert_eq!(created["workspace_state"], "source_ready");
        let mut forged = json!({"method":"api_run_create","key":"forged","authority":authority.as_str(),"epoch":1,
            "workspace":Uuid::new_v4().to_string(),"run":Uuid::new_v4().to_string(),"base":imported["revision"],
            "profile_digest":profile["profile_digest"],"agent":"controller/1","task":"edit-source"});
        forged["runtime"] = json!("pretend-safe");
        assert_eq!(f.send(forged).await.status.as_u16(), 400);
        let download = f.dir.path().join("api-baseline.txt");
        ok(f.cli(&[
            "source-file",
            "--workspace",
            &workspace.to_string(),
            "--generation",
            "1",
            "--authority",
            authority.as_str(),
            "--epoch",
            "1",
            "--path",
            "src/same.txt",
            "--output",
            download.to_str().unwrap(),
        ]));
        assert_eq!(fs::read(download).unwrap(), b"before");
    } else {
        ok(f.cli(&[
            "workspace-create",
            "--key",
            "proposal-workspace",
            "--authority",
            authority.as_str(),
            "--epoch",
            "1",
            "--workspace",
            &workspace.to_string(),
            "--base",
            imported["revision"].as_str().unwrap(),
            "--profile-digest",
            profile["profile_digest"].as_str().unwrap(),
        ]));
        // Transport fixture only. Actual managed execution/removal is tested by the
        // separate disposable Linux gVisor harness, not fabricated by this test.
        f.sql.execute("UPDATE th_workspaces SET state='in_use',runtime='fixture',node='fixture' WHERE workspace=$1",&[&workspace]).await.unwrap();
        f.sql.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state) SELECT tenant,repository,$1,workspace,generation,epoch,principal,'fixture','fixture',runtime,'running' FROM th_workspaces WHERE workspace=$2",&[&run,&workspace]).await.unwrap();
    }
    let upload = Uuid::new_v4();
    let changeset = Uuid::new_v4();
    let begin = [
        "change-begin",
        "--key",
        "proposal-begin",
        "--authority",
        authority.as_str(),
        "--epoch",
        "1",
        "--upload",
        &upload.to_string(),
        "--run",
        &run.to_string(),
        "--generation",
        "1",
    ];
    let started = ok(f.cli(&begin));
    assert_eq!(started, ok(f.cli(&begin)));
    let bytes = b"new contents\0\xff";
    let file = f.dir.path().join("new-bytes");
    files::write_private(&file, bytes).unwrap();
    let put_args = [
        "change-put",
        "--upload",
        &upload.to_string(),
        "--file",
        file.to_str().unwrap(),
    ];
    let blob = ok(f.cli(&put_args));
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    server = f.start().await;
    assert_eq!(ok(f.cli(&put_args)), blob);
    let uploaded = ok(f.cli(&["change-status", "--upload", &upload.to_string()]));
    assert_eq!(uploaded["reserved_objects"], 1);
    let replacement = Object::Blob(bytes.to_vec());
    assert_eq!(blob["object"], replacement.id().unwrap().to_string());
    let old = Object::Blob(b"before".to_vec());
    let gone = Object::Blob(b"gone".to_vec());
    let operations = json!([
        {"path_hex":hex(b"src/same.txt"),"expected":{"mode":33188,"object":old.id().unwrap().to_string()},"replacement":{"mode":33188,"object":blob["object"]}},
        {"path_hex":hex(b"obsolete.py"),"expected":{"mode":33188,"object":gone.id().unwrap().to_string()},"replacement":null},
        {"path_hex":hex(b"new/run"),"expected":null,"replacement":{"mode":33261,"object":blob["object"]}}
    ]);
    let manifest = f.dir.path().join("proposal-operations.json");
    files::write_private(&manifest, operations.to_string().as_bytes()).unwrap();
    let body = json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"key":"proposal-submit","authority":authority.as_str(),"epoch":1,"upload":upload.to_string(),"changeset":changeset.to_string(),"previous_version":0,"operations":operations});
    let credential = f.credential();
    let mut invalid = body.clone();
    invalid["operations"][0]["expected"]["object"] = json!(gone.id().unwrap().to_string());
    let refused = client::send_proposal(
        &f.config.socket,
        &credential.token,
        invalid.to_string().into_bytes(),
        Duration::from_secs(10),
    )
    .await
    .unwrap();
    assert_eq!(refused.status.as_u16(), 409);
    assert_eq!(refused.body["error"]["code"], "change_base_mismatch");
    let mut forged = body.clone();
    forged["result_snapshot"] = json!(old.id().unwrap().to_string());
    assert_eq!(
        client::send_proposal(
            &f.config.socket,
            &credential.token,
            forged.to_string().into_bytes(),
            Duration::from_secs(10)
        )
        .await
        .unwrap()
        .status
        .as_u16(),
        400
    );
    // Do not consume the accepted response. Kill the server only after PostgreSQL
    // has recorded the atomic version/receipt, then recover through public CLI.
    let body_bytes = body.to_string().into_bytes();
    let mut request=format!("POST /v1/changes/submit HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n",credential.token.expose(),body_bytes.len()).into_bytes();
    request.extend(body_bytes);
    // First lose the process after a derived directory is fsynced but before its
    // catalog publication commits. Its uploading reservation must be recoverable.
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION hold_derived() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({lock}::bigint); RETURN NEW; END $$; CREATE TRIGGER hold_derived BEFORE UPDATE ON th_objects FOR EACH ROW WHEN (OLD.state='uploading' AND NEW.state='live' AND NEW.kind=2) EXECUTE FUNCTION hold_derived()")).await.unwrap();
    let mut partial = UnixStream::connect(&f.config.socket).await.unwrap();
    partial.write_all(&request).await.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let waiting:bool=f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock & 0xffff_ffff)]).await.unwrap().get(0);
        if waiting {
            break;
        }
        assert!(
            Instant::now() < until,
            "derived publication did not reach fault boundary"
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
        .batch_execute("DROP TRIGGER hold_derived ON th_objects; DROP FUNCTION hold_derived()")
        .await
        .unwrap();
    assert!(
        f.sql
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM th_objects WHERE kind=2 AND state='uploading')",
                &[]
            )
            .await
            .unwrap()
            .get::<_, bool>(0)
    );
    assert_eq!(
        f.sql
            .query_one("SELECT count(*) FROM th_change_versions", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    server = f.start().await;
    let mut lost = UnixStream::connect(&f.config.socket).await.unwrap();
    lost.write_all(&request).await.unwrap();
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let count: i64 = f
            .sql
            .query_one(
                "SELECT count(*) FROM th_operations WHERE key='proposal-submit'",
                &[],
            )
            .await
            .unwrap()
            .get(0);
        if count == 1 {
            break;
        }
        assert!(Instant::now() < until, "proposal never committed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(lost);
    server = f.start().await;
    let submit = [
        "change-submit",
        "--key",
        "proposal-submit",
        "--authority",
        authority.as_str(),
        "--epoch",
        "1",
        "--upload",
        &upload.to_string(),
        "--changeset",
        &changeset.to_string(),
        "--operations",
        manifest.to_str().unwrap(),
    ];
    let accepted = ok(f.cli(&submit));
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(accepted["run"], run.to_string());
    if external {
        assert_eq!(accepted["run_origin"], "external_api");
        assert_eq!(accepted["execution_observed"], false);
    }

    // Independently construct the expected full result. Unmentioned keep.bin
    // survives, deletion is explicit, and executable mode is preserved.
    let src = Object::Directory(
        Directory::new(vec![entry(b"same.txt", EntryMode::File, &replacement)]).unwrap(),
    );
    let new = Object::Directory(
        Directory::new(vec![entry(b"run", EntryMode::Executable, &replacement)]).unwrap(),
    );
    let keep = Object::Blob(b"unchanged\0\xff".to_vec());
    let root = Object::Directory(
        Directory::new(vec![
            entry(b"src", EntryMode::Directory, &src),
            entry(b"new", EntryMode::Directory, &new),
            entry(b"keep.bin", EntryMode::File, &keep),
        ])
        .unwrap(),
    );
    let expected = Object::Snapshot {
        root: root.id().unwrap(),
    };
    assert_eq!(
        accepted["result_snapshot"],
        expected.id().unwrap().to_string()
    );
    let stored = f.config.storage.as_ref().unwrap();
    let depot = ManagedStore::open(
        &stored.root,
        TenantId::new(f.scope.tenant()).unwrap(),
        &stored.backend,
    )
    .unwrap();
    for object in [&expected, &root, &src, &new, &replacement, &keep] {
        let generation: i64 = f
            .sql
            .query_one(
                "SELECT generation FROM th_objects WHERE digest=$1 AND state='live'",
                &[&object.id().unwrap().as_bytes().as_slice()],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(
            &depot.get(generation, object.id().unwrap()).unwrap(),
            object
        );
    }
    ok(f.cli(&[
        "workspace-delete",
        "--key",
        "proposal-drain",
        "--authority",
        authority.as_str(),
        "--epoch",
        "1",
        "--workspace",
        &workspace.to_string(),
        "--generation",
        "1",
    ]));
    if external {
        let status = ok(f.cli(&["api-run-status", "--run", &run.to_string()]));
        assert_eq!(status["state"], "closed");
        assert_eq!(status["workspace_state"], "deleted");
        assert_eq!(status["workspace_generation"], 2);
        assert!(status.get("runtime_removed").is_none());
        let late = json!({"method":"change_begin","key":"late-upload","authority":authority.as_str(),"epoch":1,
            "upload":Uuid::new_v4().to_string(),"run":run.to_string(),"generation":1,
            "max_objects":3,"max_bytes":1024,"ttl_seconds":60});
        assert_eq!(f.send(late).await.status.as_u16(), 409);
        assert_eq!(f.sql.query_one("SELECT count(*) FROM th_outbox WHERE kind IN ('workspace.prepare','workspace.drain','run.execute')",&[]).await.unwrap().get::<_,i64>(0),0);
        assert_eq!(
            f.sql
                .query_one("SELECT count(*) FROM th_managed_runs", &[])
                .await
                .unwrap()
                .get::<_, i64>(0),
            0
        );
    }
    assert_eq!(ok(f.cli(&submit)), accepted);
    let status = ok(f.cli(&["operation-status", "--key", "proposal-submit"]));
    assert_eq!(status["result"], accepted);
    let counts=f.sql.query_one("SELECT (SELECT count(*) FROM th_change_versions),(SELECT count(*) FROM th_outbox WHERE kind='change.accepted')",&[]).await.unwrap();
    assert_eq!((counts.get::<_, i64>(0), counts.get::<_, i64>(1)), (1, 1));
    drop(server);
    f.cleanup().await;
}
