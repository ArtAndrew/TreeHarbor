#[path = "native/rebases.rs"]
mod rebases;
#[path = "native/seals.rs"]
mod seals;
use super::*;
use treeharbor_core::{ObjectId, ObjectStore};

fn ok(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice::<Value>(&output.stdout).unwrap()["result"].clone()
}
fn role(f: &mut Fixture, name: &str, kind: Option<&str>, permissions: &[&str]) -> PathBuf {
    if let Some(kind) = kind {
        f.admin_ok(&[
            "principal-create",
            "--tenant",
            f.scope.tenant(),
            "--principal",
            name,
            "--kind",
            kind,
        ]);
    }
    let repository = f.scope.repository().to_string();
    let mut args = vec![
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &repository,
        "--principal",
        name,
    ];
    for p in permissions {
        args.extend(["--permission", p]);
    }
    f.admin_ok(&args);
    let path = f.dir.path().join(format!("{name}-native.json"));
    args[0] = "token-issue";
    args.extend(["--output", path.to_str().unwrap()]);
    f.admin_ok(&args);
    path
}
fn native(f: &Fixture, request: Value) -> Output {
    let path = f
        .dir
        .path()
        .join(format!("request-{}.json", Uuid::new_v4()));
    files::write_private(&path, request.to_string().as_bytes()).unwrap();
    f.cli(&["native", "--request", path.to_str().unwrap()])
}
fn mutation(action: &str) -> Value {
    json!({"action":action,"key":Uuid::new_v4().to_string(),"epoch":1})
}
async fn proposal(
    f: &mut Fixture,
    actor: &str,
    base: &Value,
    profile: &Value,
    bytes: &[u8],
) -> Uuid {
    let workspace = Uuid::new_v4();
    let run = Uuid::new_v4();
    let upload = Uuid::new_v4();
    let change = Uuid::new_v4();
    ok(f.cli(&[
        "workspace-create",
        "--key",
        &workspace.to_string(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--workspace",
        &workspace.to_string(),
        "--base",
        base["revision"].as_str().unwrap(),
        "--profile-digest",
        profile["profile_digest"].as_str().unwrap(),
    ]));
    // Explicit transport fixture: source lineage is real; executing the agent is
    // covered by Linux runtime CI, not fabricated as validation here.
    f.sql.execute("UPDATE th_workspaces SET state='in_use',runtime='fixture',node='fixture' WHERE workspace=$1",&[&workspace]).await.unwrap();
    f.sql.execute("INSERT INTO th_agent_runs(tenant,repository,run,workspace,workspace_generation,epoch,principal,agent,task,runtime,state) SELECT tenant,repository,$1,workspace,generation,epoch,principal,$3,'fixture',runtime,'running' FROM th_workspaces WHERE workspace=$2",&[&run,&workspace,&actor]).await.unwrap();
    ok(f.cli(&[
        "change-begin",
        "--key",
        &upload.to_string(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--upload",
        &upload.to_string(),
        "--run",
        &run.to_string(),
        "--generation",
        "1",
    ]));
    let file = f.dir.path().join(format!("bytes-{change}"));
    files::write_private(&file, bytes).unwrap();
    let blob = ok(f.cli(&[
        "change-put",
        "--upload",
        &upload.to_string(),
        "--file",
        file.to_str().unwrap(),
    ]));
    let mut ops = json!([{"path_hex":"66696c652e747874","expected":{"mode":33188,"object":Object::Blob(b"a\nb\nc\nd\ne\n".to_vec()).id().unwrap().to_string()},"replacement":{"mode":33188,"object":blob["object"]}}]);
    if actor == "agent-a" {
        // A leaf-only public proposal creates a fresh directory. Its protected
        // integration delta must use a server-derived v2 subtree replacement.
        ops.as_array_mut().unwrap().push(json!({"path_hex":"61646465642f66696c652e747874","expected":null,"replacement":{"mode":33188,"object":blob["object"]}}));
    }
    let path = f.dir.path().join(format!("ops-{change}"));
    files::write_private(&path, ops.to_string().as_bytes()).unwrap();
    ok(f.cli(&[
        "change-submit",
        "--key",
        &change.to_string(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--upload",
        &upload.to_string(),
        "--changeset",
        &change.to_string(),
        "--operations",
        path.to_str().unwrap(),
    ]));
    let entry = Uuid::new_v4();
    let mut q = mutation("enqueue");
    q["entry"] = json!(entry.to_string());
    q["target"] = json!("main");
    q["changeset"] = json!(change.to_string());
    q["version"] = json!(1);
    ok(native(f, q));
    entry
}
fn start(f: &Fixture, entry: Uuid) -> Uuid {
    let id = Uuid::new_v4();
    let mut r = mutation("candidate_start");
    r["entry"] = json!(entry.to_string());
    r["candidate"] = json!(id.to_string());
    ok(native(f, r));
    id
}
async fn green(
    f: &mut Fixture,
    candidate: Uuid,
    worker: &std::path::Path,
    validator: &std::path::Path,
    reviewer: &std::path::Path,
) {
    f.credential_path = worker.into();
    let run = Uuid::new_v4();
    let mut r = mutation("check_assign");
    r["candidate"] = json!(candidate.to_string());
    r["name"] = json!("tests");
    r["run"] = json!(run.to_string());
    r["validator"] = json!("ci");
    ok(native(f, r));
    f.credential_path = validator.into();
    let mut r = mutation("validation_report");
    r["run"] = json!(run.to_string());
    r["receipt"] = json!(Uuid::new_v4().to_string());
    r["outcome"] = json!("passed");
    r["evidence"] = json!(ObjectId::from_bytes([9; 32]).to_string());
    ok(native(f, r));
    f.credential_path = reviewer.into();
    let mut r = mutation("review");
    r["candidate"] = json!(candidate.to_string());
    r["review"] = json!(Uuid::new_v4().to_string());
    r["decision"] = json!("approve");
    ok(native(f, r));
    f.credential_path = worker.into();
}
fn prepare_args(id: &str) -> [&str; 7] {
    [
        "native-prepare",
        "--key",
        id,
        "--epoch",
        "1",
        "--candidate",
        id,
    ]
}
fn candidate_diff(f: &Fixture, id: Uuid) -> Output {
    f.cli(&[
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "candidate",
        "--candidate",
        &id.to_string(),
    ])
}
async fn raw_prepare(f: &Fixture, id: Uuid) -> UnixStream {
    let body=json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"key":id.to_string(),"epoch":1,"candidate":id.to_string()}).to_string();
    let credential = f.credential();
    let mut stream = UnixStream::connect(&f.config.socket).await.unwrap();
    let request = format!(
        "POST /v1/native/prepare HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}",
        credential.token.expose(),
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; real CLI/service/process death/storage, trusted runtime and CI fixtures"]
async fn native_queue_cli_merges_agents_fences_green_and_recovers_fsync_and_committed_response_loss()
 {
    let mut f = Fixture::new().await;
    f.importer();
    f.config.request_timeout_ms = 20000;
    f.config.pool_size = 3;
    f.save_config();
    let admin = role(
        &mut f,
        "importer",
        None,
        &[
            "read",
            "repository.write",
            "workspace.write",
            "policy.write",
        ],
    );
    let agent_a = role(
        &mut f,
        "agent-a",
        None,
        &["read", "workspace.write", "change.write", "queue.request"],
    );
    let agent_b = role(
        &mut f,
        "agent-b",
        Some("agent"),
        &["read", "workspace.write", "change.write", "queue.request"],
    );
    let agent_c = role(
        &mut f,
        "agent-c",
        Some("agent"),
        &["read", "workspace.write", "change.write", "queue.request"],
    );
    let worker = role(
        &mut f,
        "integrator",
        Some("integrator"),
        &["read", "integration.control"],
    );
    let validator = role(
        &mut f,
        "ci",
        Some("validator"),
        &["read", "validation.report"],
    );
    let reviewer = role(&mut f, "reviewer", Some("human"), &["read", "review"]);
    let observer = role(&mut f, "observer", Some("human"), &["read"]);
    f.credential_path = admin.clone();
    let source = f.dir.path().join("native-source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file.txt"), b"a\nb\nc\nd\ne\n").unwrap();
    fs::write(source.join("keep.bin"), vec![42; 2 * 1024 * 1024]).unwrap();
    let pack = f.dir.path().join("native-pack");
    assert!(
        Command::new(env!("CARGO_BIN_EXE_treeharbor"))
            .env("PATH", "/no-executables")
            .args(["source-pack", "--directory"])
            .arg(&source)
            .arg("--output")
            .arg(&pack)
            .args([
                "--path",
                "file.txt",
                "--path",
                "keep.bin",
                "--author-name",
                "Test",
                "--author-email",
                "test@example.invalid"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let mut server = f.start().await;
    let base = ok(f.cli(&[
        "source-import",
        "--pack",
        pack.to_str().unwrap(),
        "--state",
        f.dir.path().join("import.json").to_str().unwrap(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ]));
    let profile = ok(f.cli(&[
        "profile-put",
        "--key",
        "full",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--full",
    ]));
    let digest = ObjectId::from_bytes([7; 32]).to_string();
    let mut r = mutation("policy_put");
    r["policy"] = json!({"version":1,"checks":[{"name":"tests","definition":digest,"environment":digest,"toolchain":digest,"dependencies":digest,"validators":["ci"]}],"reviewers":["reviewer"],"approvals":1});
    let policy = ok(native(&f, r));
    let mut r = mutation("ref_create");
    r["name"] = json!("main");
    r["head"] = base["revision"].clone();
    r["policy"] = policy["policy"].clone();
    ok(native(&f, r));
    f.credential_path = agent_a.clone();
    let first = proposal(&mut f, "agent-a", &base, &profile, b"A\nb\nc\nd\ne\n").await;
    f.credential_path = agent_b;
    let second = proposal(&mut f, "agent-b", &base, &profile, b"a\nb\nc\nd\nE\n").await;
    f.credential_path = agent_c;
    let third = proposal(&mut f, "agent-c", &base, &profile, b"C\nb\nc\nd\ne\n").await;
    f.credential_path = worker.clone();
    let a = start(&f, first);
    f.credential_path = observer.clone();
    let unavailable = candidate_diff(&f, a);
    assert!(!unavailable.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&unavailable.stdout).unwrap()["error"]["code"],
        "review_unavailable"
    );
    f.credential_path = worker.clone();
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION hold_native() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({lock}::bigint); RETURN NEW; END $$; CREATE TRIGGER hold_native BEFORE UPDATE ON th_objects FOR EACH ROW WHEN(OLD.state='uploading' AND NEW.state='live' AND NEW.kind=5) EXECUTE FUNCTION hold_native()")).await.unwrap();
    let partial = raw_prepare(&f, a).await;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let blocked:bool=f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock&0xffff_ffff)]).await.unwrap().get(0);
        if blocked {
            break;
        }
        assert!(
            Instant::now() < until,
            "native commit publication not reached"
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
        .batch_execute("DROP TRIGGER hold_native ON th_objects; DROP FUNCTION hold_native()")
        .await
        .unwrap();
    let reservations:Value=f.sql.query_one("SELECT jsonb_agg(jsonb_build_array(encode(digest,'hex'),generation,hold_token) ORDER BY digest) FROM th_candidate_objects WHERE candidate=$1",&[&a]).await.unwrap().get(0);
    server = f.start().await;
    let response_lost = raw_prepare(&f, a).await;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let state: String = f
            .sql
            .query_one(
                "SELECT state FROM th_native_candidates WHERE candidate=$1",
                &[&a],
            )
            .await
            .unwrap()
            .get(0);
        if state == "checking" {
            break;
        }
        assert!(Instant::now() < until, "native receipt not committed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(response_lost);
    server = f.start().await;
    let prepared = ok(f.cli(&prepare_args(&a.to_string())));
    assert_eq!(prepared["state"], "checking");
    f.credential_path = observer.clone();
    let reviewed_a = ok(candidate_diff(&f, a));
    assert_eq!(reviewed_a["identity"]["revision"], prepared["revision"]);
    assert_eq!(reviewed_a["identity"]["expected_head"], base["revision"]);
    assert_eq!(reviewed_a["after_snapshot"], prepared["snapshot"]);
    f.credential_path = worker.clone();
    for row in reservations.as_array().unwrap() {
        let current:Value=f.sql.query_one("SELECT jsonb_build_array(encode(digest,'hex'),generation,hold_token) FROM th_candidate_objects WHERE candidate=$1 AND digest=decode($2,'hex')",&[&a,&row[0].as_str().unwrap()]).await.unwrap().get(0);
        assert_eq!(*row, current);
    }
    let mut land = mutation("land");
    land["candidate"] = json!(a.to_string());
    assert!(!native(&f, land.clone()).status.success());
    f.credential_path = agent_a;
    assert!(!native(&f, land.clone()).status.success());
    green(&mut f, a, &worker, &validator, &reviewer).await;
    let landed = ok(native(&f, land.clone()));
    assert_eq!(landed["state"], "landed");
    assert_eq!(ok(native(&f, land)), landed);
    let b = start(&f, second);
    let prepared_b = ok(f.cli(&prepare_args(&b.to_string())));
    assert_eq!(prepared_b["state"], "checking");
    assert_eq!(prepared_b["expected_head"], landed["revision"]);
    f.credential_path = observer.clone();
    assert_eq!(ok(candidate_diff(&f, a)), reviewed_a);
    let reviewed_b = ok(candidate_diff(&f, b));
    assert_eq!(reviewed_b["identity"]["expected_head"], landed["revision"]);
    assert_eq!(
        reviewed_b["identity"]["proposal_base_revision"],
        base["revision"]
    );
    assert_eq!(reviewed_b["identity"]["expected_generation"], 2);
    assert_eq!(reviewed_b["identity"]["revision"], prepared_b["revision"]);
    assert_eq!(reviewed_b["identity"]["policy"], prepared_b["policy"]);
    assert_eq!(
        reviewed_b["identity"]["merge_profile"],
        prepared_b["merge_profile"]
    );
    assert_eq!(reviewed_b["before_snapshot"], landed["snapshot"]);
    assert_eq!(reviewed_b["after_snapshot"], prepared_b["snapshot"]);
    assert_eq!(reviewed_b["total_changes"], 1);
    let row = &reviewed_b["changes"][0];
    assert_eq!(row["path_hex"], "66696c652e747874");
    assert_eq!(
        row["before"]["object"],
        Object::Blob(b"A\nb\nc\nd\ne\n".to_vec())
            .id()
            .unwrap()
            .to_string()
    );
    assert_eq!(
        row["after"]["object"],
        Object::Blob(b"A\nb\nc\nd\nE\n".to_vec())
            .id()
            .unwrap()
            .to_string()
    );
    let patch = diffy::Patch::from_str(row["text"]["patch"].as_str().unwrap()).unwrap();
    assert_eq!(
        diffy::apply("A\nb\nc\nd\ne\n", &patch).unwrap(),
        "A\nb\nc\nd\nE\n"
    );
    // The source proposal is different from the exact integration result.
    let proposal_diff = ok(f.cli(&[
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "change",
        "--changeset",
        reviewed_b["identity"]["changeset"].as_str().unwrap(),
        "--version",
        "1",
    ]));
    assert_ne!(
        proposal_diff["changes"][0]["before"]["object"],
        row["before"]["object"]
    );
    assert_ne!(
        proposal_diff["changes"][0]["after"]["object"],
        row["after"]["object"]
    );
    assert_eq!(
        proposal_diff["identity"]["run"],
        reviewed_b["identity"]["run"]
    );
    let pair = ok(f.cli(&[
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "revisions",
        "--before",
        prepared_b["expected_head"].as_str().unwrap(),
        "--after",
        prepared_b["revision"].as_str().unwrap(),
    ]));
    assert_eq!(pair["changes"], reviewed_b["changes"]);
    f.credential_path = worker.clone();
    let mut land_b = mutation("land");
    land_b["candidate"] = json!(b.to_string());
    assert!(!native(&f, land_b.clone()).status.success());
    green(&mut f, b, &worker, &validator, &reviewer).await;
    let final_head = ok(native(&f, land_b));
    let c = start(&f, third);
    let conflict = ok(f.cli(&prepare_args(&c.to_string())));
    assert_eq!(conflict["state"], "conflicted");
    assert_eq!(
        conflict["failure"]["details"]["conflicts"][0]["path_hex"],
        "66696c652e747874"
    );
    assert_eq!(conflict, ok(f.cli(&prepare_args(&c.to_string()))));
    f.credential_path = observer.clone();
    let unavailable = candidate_diff(&f, c);
    assert!(!unavailable.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&unavailable.stdout).unwrap()["error"]["code"],
        "review_unavailable"
    );
    assert_eq!(ok(candidate_diff(&f, b)), reviewed_b);
    f.credential_path = worker.clone();
    assert_eq!(
        ok(native(&f, json!({"action":"ref_resolve","name":"main"})))["head"],
        final_head["revision"]
    );
    // Read the entire durable published DAG through catalog leases after process
    // death. This verifies actual object bytes/parents, not only metadata IDs.
    let mut db = Database::new(f.config.clone())
        .unwrap()
        .connect(true)
        .await
        .unwrap();
    let depot = ManagedStore::open(
        &f.config.storage.as_ref().unwrap().root,
        TenantId::new(f.scope.tenant()).unwrap(),
        "source-1",
    )
    .unwrap();
    let RevisionRef::Native(head) = final_head["revision"].as_str().unwrap().parse().unwrap()
    else {
        panic!()
    };
    let session = db
        .metadata
        .open_read_session(&f.scope, &format!("native-published:{}", head.hex()), 60)
        .await
        .unwrap();
    let copy = treeharbor_storage::LocalStore::open(
        f.dir.path().join("dag"),
        &TenantId::new(f.scope.tenant()).unwrap(),
    )
    .unwrap();
    let mut pending = vec![session.root()];
    let mut seen = std::collections::BTreeSet::new();
    while let Some(node) = pending.pop() {
        if !seen.insert(node.id()) {
            continue;
        }
        let read = db
            .metadata
            .open_read_batch(&session, &depot, &[node], 128 * 1024 * 1024)
            .await
            .unwrap()
            .read()
            .unwrap()
            .pop()
            .unwrap();
        pending.extend(read.children());
        copy.put(read.object()).unwrap();
    }
    db.metadata.close_read_session(session).await.unwrap();
    let Object::NativeCommit(commit) = copy.get(head).unwrap() else {
        panic!()
    };
    assert_eq!(commit.parents.len(), 2);
    let Object::Snapshot { root } = copy.get(commit.snapshot).unwrap() else {
        panic!()
    };
    let Object::Directory(dir) = copy.get(root).unwrap() else {
        panic!()
    };
    assert_eq!(
        copy.get(dir.find(b"file.txt").unwrap().target).unwrap(),
        Object::Blob(b"A\nb\nc\nd\nE\n".to_vec())
    );
    assert_eq!(
        copy.get(dir.find(b"keep.bin").unwrap().target).unwrap(),
        Object::Blob(vec![42; 2 * 1024 * 1024])
    );
    let added = treeharbor_core::lookup(
        &copy,
        root,
        &treeharbor_core::RepoPath::new(b"added/file.txt".to_vec()).unwrap(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        copy.get(added.target).unwrap(),
        Object::Blob(b"A\nb\nc\nd\ne\n".to_vec())
    );
    assert!(seen.iter().any(|id| matches!(copy.get(*id).unwrap(), Object::ChangeOperations(ops) if ops.format_version() == 2)));
    let RevisionRef::Native(base) = base["revision"].as_str().unwrap().parse().unwrap() else {
        panic!()
    };
    assert!(seen.contains(&base));
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_native_ref_updates WHERE kind='land'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    // Export the actual newly landed graph through public CLI after restart.
    // Original input packs/files are gone; the local source path is no oracle.
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    fs::remove_dir_all(&source).unwrap();
    fs::remove_dir_all(&pack).unwrap();
    server = f.start().await;
    f.credential_path = observer;
    assert_eq!(ok(candidate_diff(&f, b)), reviewed_b);
    f.credential_path = worker;
    let exported_path = f.dir.path().join("server-export");
    let export_args = [
        "source-export",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--revision",
        final_head["revision"].as_str().unwrap(),
        "--output",
        exported_path.to_str().unwrap(),
    ];
    let exported = f.cli(&export_args);
    assert!(
        exported.status.success(),
        "{} {}",
        String::from_utf8_lossy(&exported.stdout),
        String::from_utf8_lossy(&exported.stderr)
    );
    let exported = treeharbor_service::pack::Pack::open(&exported_path).unwrap();
    assert_eq!(exported.commit(), head);
    assert_eq!(exported.object_count(), seen.len());
    let history = ok(f.cli(&[
        "history",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "log",
        "--revision",
        final_head["revision"].as_str().unwrap(),
        "--limit",
        "256",
    ]));
    let rows = history["history"]["commits"].as_array().unwrap();
    let expected: std::collections::BTreeSet<_> = seen
        .iter()
        .filter(|id| matches!(copy.get(**id).unwrap(), Object::NativeCommit(_)))
        .map(|id| RevisionRef::Native(*id).to_string())
        .collect();
    assert_eq!(
        rows.iter()
            .map(|r| r["revision"].as_str().unwrap().to_owned())
            .collect::<std::collections::BTreeSet<_>>(),
        expected
    );
    assert_eq!(rows[0]["parents"].as_array().unwrap().len(), 2);
    for id in &seen {
        assert_eq!(
            exported.frame(*id).unwrap(),
            copy.get(*id).unwrap().encode().unwrap()
        );
    }
    assert!(!f.cli(&export_args).status.success());
    let git_dir = f.dir.path().join("git-export");
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["git-export", "--pack"])
        .arg(&exported_path)
        .arg("--output")
        .arg(&git_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let restored = f.dir.path().join("restored.git");
    git_reference::git(
        f.dir.path(),
        &[
            "clone",
            "--bare",
            git_dir.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(
        git_reference::git(&restored, &["show", "refs/heads/main:file.txt"], b""),
        b"A\nb\nc\nd\nE\n"
    );
    assert_eq!(
        git_reference::git(&restored, &["show", "refs/heads/main:keep.bin"], b""),
        vec![42; 2 * 1024 * 1024]
    );
    // Exported pack can reenter the native upload path with exact revision identity.
    f.credential_path = admin;
    let receipt = ok(f.cli(&[
        "source-import",
        "--pack",
        exported_path.to_str().unwrap(),
        "--state",
        f.dir.path().join("reimport.json").to_str().unwrap(),
        "--authority",
        "standalone",
        "--epoch",
        "1",
    ]));
    assert_eq!(receipt["revision"], final_head["revision"]);
    drop(server);
    drop(db);
    f.cleanup().await;
}
