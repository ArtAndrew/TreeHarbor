use super::*;
fn rebase_prepare(id: &str) -> [&str; 7] {
    [
        "rebase-prepare",
        "--key",
        id,
        "--epoch",
        "1",
        "--rebase",
        id,
    ]
}
async fn raw_rebase(f: &Fixture, id: Uuid) -> UnixStream {
    let body=json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository().to_string(),"key":id.to_string(),"epoch":1,"rebase":id.to_string()}).to_string();
    let credential = f.credential();
    let mut stream = UnixStream::connect(&f.config.socket).await.unwrap();
    let request = format!(
        "POST /v1/changes/rebase HTTP/1.1\r\nHost: treeharbor.local\r\nContent-Type: application/json\r\nAuthorization: Bearer {}\r\nContent-Length: {}\r\n\r\n{body}",
        credential.token.expose(),
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    stream
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires disposable PostgreSQL; real CLI/rebase/process crash/immutable source, CI fixtures"]
async fn rebase_cli_survives_fsync_and_response_loss_and_requires_new_evidence_without_git() {
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
    f.credential_path = agent_b.clone();
    let second = proposal(&mut f, "agent-b", &base, &profile, b"a\nb\nc\nd\nE\n").await;
    f.credential_path = agent_c.clone();
    let third = proposal(&mut f, "agent-c", &base, &profile, b"C\nb\nc\nd\ne\n").await;
    f.credential_path = worker.clone();
    let a = start(&f, first);
    ok(f.cli(&prepare_args(&a.to_string())));
    green(&mut f, a, &worker, &validator, &reviewer).await;
    let mut land = mutation("land");
    land["candidate"] = json!(a.to_string());
    let landed = ok(native(&f, land));
    let b = start(&f, second);
    ok(f.cli(&prepare_args(&b.to_string())));
    green(&mut f, b, &worker, &validator, &reviewer).await;
    let before:Value=f.sql.query_one("SELECT to_jsonb(v) FROM th_change_versions v JOIN th_native_queue q USING(tenant,repository,changeset,version) WHERE q.entry=$1",&[&second]).await.unwrap().get(0);
    let change = before["changeset"].as_str().unwrap();
    let rebase = Uuid::new_v4();
    let rebase_text = rebase.to_string();
    f.credential_path = agent_b.clone();
    let start_args = [
        "rebase-start",
        "--key",
        "start-rebase",
        "--epoch",
        "1",
        "--rebase",
        &rebase_text,
        "--changeset",
        change,
        "--source-version",
        "1",
        "--onto",
        landed["revision"].as_str().unwrap(),
    ];
    let started = ok(f.cli(&start_args));
    assert_eq!(started["state"], "preparing");
    assert_eq!(started, ok(f.cli(&start_args)));
    // An owner cannot bypass the integration policy with a rebase request.
    let mut old_land = mutation("land");
    old_land["candidate"] = json!(b.to_string());
    assert!(!native(&f, old_land.clone()).status.success());
    // Crash after durable object fsync but before its catalogue publication.
    let lock = (Uuid::new_v4().as_u128() & 0x7fff_ffff_ffff_ffff) as i64;
    f.sql
        .query_one("SELECT pg_advisory_lock($1)", &[&lock])
        .await
        .unwrap();
    f.sql.batch_execute(&format!("CREATE FUNCTION hold_rebase() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_advisory_xact_lock({lock}::bigint); RETURN NEW; END $$; CREATE TRIGGER hold_rebase BEFORE UPDATE ON th_objects FOR EACH ROW WHEN(OLD.state='uploading' AND NEW.state='live' AND NEW.kind=5) EXECUTE FUNCTION hold_rebase()")).await.unwrap();
    let partial = raw_rebase(&f, rebase).await;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let blocked:bool=f.sql.query_one("SELECT EXISTS(SELECT 1 FROM pg_locks WHERE locktype='advisory' AND NOT granted AND classid::bigint=$1 AND objid::bigint=$2)",&[&(lock>>32),&(lock&0xffff_ffff)]).await.unwrap().get(0);
        if blocked {
            break;
        }
        assert!(Instant::now() < until, "rebase fsync not reached");
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
        .batch_execute("DROP TRIGGER hold_rebase ON th_objects; DROP FUNCTION hold_rebase()")
        .await
        .unwrap();
    let reservations:Value=f.sql.query_one("SELECT jsonb_agg(jsonb_build_array(encode(digest,'hex'),generation,hold_token) ORDER BY digest) FROM th_change_rebase_objects WHERE rebase=$1",&[&rebase]).await.unwrap().get(0);
    // Retry, then lose the committed response and terminate the service again.
    server = f.start().await;
    let response_lost = raw_rebase(&f, rebase).await;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let state: String = f
            .sql
            .query_one(
                "SELECT state FROM th_change_rebases WHERE rebase=$1",
                &[&rebase],
            )
            .await
            .unwrap()
            .get(0);
        if state == "ready" {
            break;
        }
        assert!(Instant::now() < until, "rebase receipt not committed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    drop(response_lost);
    server = f.start().await;
    let rebased = ok(f.cli(&rebase_prepare(&rebase_text)));
    assert_eq!(rebased["state"], "ready");
    assert_eq!(rebased["result_version"], 2);
    assert_eq!(rebased["run"], before["run"]);
    assert_eq!(rebased, ok(f.cli(&rebase_prepare(&rebase_text))));
    for row in reservations.as_array().unwrap() {
        let current:Value=f.sql.query_one("SELECT jsonb_build_array(encode(digest,'hex'),generation,hold_token) FROM th_change_rebase_objects WHERE rebase=$1 AND digest=decode($2,'hex')",&[&rebase,&row[0].as_str().unwrap()]).await.unwrap().get(0);
        assert_eq!(*row, current);
    }
    assert_eq!(before,f.sql.query_one("SELECT to_jsonb(v) FROM th_change_versions v JOIN th_native_queue q USING(tenant,repository,changeset,version) WHERE q.entry=$1",&[&second]).await.unwrap().get::<_,Value>(0));
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_change_versions WHERE changeset=$1",
                &[&change.parse::<Uuid>().unwrap()]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        2
    );
    assert_eq!(
        ok(f.cli(&["rebase-status", "--epoch", "1", "--rebase", &rebase_text]))["revision"],
        rebased["revision"]
    );
    assert!(
        !f.cli(&[
            "rebase-cancel",
            "--key",
            "late-cancel",
            "--epoch",
            "1",
            "--rebase",
            &rebase_text
        ])
        .status
        .success()
    );
    f.credential_path = worker.clone();
    assert!(!native(&f, old_land).status.success());
    assert_eq!(
        ok(native(
            &f,
            json!({"action":"candidate_status","candidate":b.to_string()})
        ))["state"],
        "stale"
    );
    assert_eq!(
        ok(native(&f, json!({"action":"ref_resolve","name":"main"})))["head"],
        landed["revision"]
    );
    // Export the actual durable single-parent rebase, not a locally fabricated graph.
    let destination = f.dir.path().join("rebased-export");
    assert!(
        f.cli(&[
            "source-export",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--revision",
            rebased["revision"].as_str().unwrap(),
            "--output",
            destination.to_str().unwrap()
        ])
        .status
        .success()
    );
    let exported = treeharbor_service::pack::Pack::open(&destination).unwrap();
    let Object::NativeCommit(commit) =
        Object::decode(&exported.frame(exported.commit()).unwrap()).unwrap()
    else {
        panic!()
    };
    let RevisionRef::Native(onto) = landed["revision"].as_str().unwrap().parse().unwrap() else {
        panic!()
    };
    assert_eq!(commit.parents, vec![onto]);
    assert!(commit.git_origin.is_none());
    let Object::Snapshot { root } =
        Object::decode(&exported.frame(commit.snapshot).unwrap()).unwrap()
    else {
        panic!()
    };
    let Object::Directory(dir) = Object::decode(&exported.frame(root).unwrap()).unwrap() else {
        panic!()
    };
    assert_eq!(
        Object::decode(
            &exported
                .frame(dir.find(b"file.txt").unwrap().target)
                .unwrap()
        )
        .unwrap(),
        Object::Blob(b"A\nb\nc\nd\nE\n".to_vec())
    );
    assert_eq!(
        Object::decode(
            &exported
                .frame(dir.find(b"keep.bin").unwrap().target)
                .unwrap()
        )
        .unwrap(),
        Object::Blob(vec![42; 2 * 1024 * 1024])
    );
    let history = ok(f.cli(&[
        "history",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "log",
        "--revision",
        rebased["revision"].as_str().unwrap(),
        "--limit",
        "256",
    ]));
    assert_eq!(
        history["history"]["commits"][0]["revision"],
        rebased["revision"]
    );
    // The third agent changes the same line as main: explicit terminal conflict.
    f.credential_path = agent_c;
    let third_change: Uuid = f
        .sql
        .query_one(
            "SELECT changeset FROM th_native_queue WHERE entry=$1",
            &[&third],
        )
        .await
        .unwrap()
        .get(0);
    let conflict = Uuid::new_v4().to_string();
    ok(f.cli(&[
        "rebase-start",
        "--key",
        "conflict-start",
        "--epoch",
        "1",
        "--rebase",
        &conflict,
        "--changeset",
        &third_change.to_string(),
        "--source-version",
        "1",
        "--onto",
        landed["revision"].as_str().unwrap(),
    ]));
    let conflicted = ok(f.cli(&rebase_prepare(&conflict)));
    assert_eq!(conflicted["state"], "conflicted");
    assert_eq!(
        conflicted["failure"]["details"]["conflicts"][0]["path_hex"],
        "66696c652e747874"
    );
    assert_eq!(conflicted, ok(f.cli(&rebase_prepare(&conflict))));
    let mut cancel = mutation("cancel");
    cancel["entry"] = json!(third.to_string());
    ok(native(&f, cancel));
    // A new queue entry/check/review set is required for version two.
    f.credential_path = agent_b;
    let entry = Uuid::new_v4();
    let mut enqueue = mutation("enqueue");
    enqueue["entry"] = json!(entry.to_string());
    enqueue["target"] = json!("main");
    enqueue["changeset"] = json!(change);
    enqueue["version"] = json!(2);
    ok(native(&f, enqueue));
    f.credential_path = worker.clone();
    let fresh = start(&f, entry);
    ok(f.cli(&prepare_args(&fresh.to_string())));
    let mut land = mutation("land");
    land["candidate"] = json!(fresh.to_string());
    assert!(!native(&f, land.clone()).status.success());
    green(&mut f, fresh, &worker, &validator, &reviewer).await;
    assert_eq!(ok(native(&f, land))["state"], "landed");
    drop(server);
    f.cleanup().await;
}
