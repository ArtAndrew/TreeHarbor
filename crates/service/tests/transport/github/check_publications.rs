//! Real coordinator/CLI process loss with actual Git import. Host observations
//! and terminal runtime output are explicitly trusted fixtures, not a live App
//! or sandbox acceptance. No test-specific remote URL is enabled in production.
use super::*;
use treeharbor_core::{ObjectId, SourceProfile};
use treeharbor_github_contract::{RequiredCheck, check_publication as cp};
use treeharbor_metadata::{
    Metadata,
    access::{Authorization, Permission as P, PrincipalKind as K},
    preparations::{PreparedFiles, StorageSlot},
    runs::RuntimeVolume,
};
use treeharbor_runtime::{Definition, Profile};

async fn actor(
    f: &Fixture,
    db: &mut Metadata,
    name: &str,
    kind: K,
    permissions: &[P],
) -> (PathBuf, Authorization) {
    db.provision_principal(f.scope.tenant(), name, kind, "fixture-operator")
        .await
        .unwrap();
    db.set_repository_access(&f.scope, name, permissions, "fixture-operator")
        .await
        .unwrap();
    let token = db
        .issue_repository_token(&f.scope, name, permissions, 3600, "fixture-operator")
        .await
        .unwrap();
    let path = f.dir.path().join(format!("check-{name}.json"));
    files::write_private(&path, json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository(),"token_id":token.id,"token":token.secret.expose()}).to_string().as_bytes()).unwrap();
    let auth = db
        .authorize(&token.secret, &f.scope, permissions[0])
        .await
        .unwrap();
    (path, auth)
}
async fn stamp(f: &Fixture) -> i64 {
    f.sql
        .query_one(
            "SELECT (extract(epoch from clock_timestamp())*1000)::bigint",
            &[],
        )
        .await
        .unwrap()
        .get(0)
}
fn cli(f: &Fixture, id: Uuid, state: &Path, app: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_treeharbor"));
    c.env("PATH", "/no-executables")
        .args(["github", "check-publish", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(&f.credential_path)
        .args(["--publication", &id.to_string(), "--epoch", "1", "--state"])
        .arg(state)
        .arg("--config")
        .arg(app);
    c
}
async fn proxy(
    f: &mut Fixture,
    action: &'static str,
    expected: &'static str,
) -> (
    PathBuf,
    tokio::task::JoinHandle<()>,
    tokio::sync::oneshot::Receiver<()>,
) {
    let path = f
        .dir
        .path()
        .join(format!("cp-{}.sock", Uuid::new_v4().simple()));
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let original = f.config.socket.clone();
    let upstream = original.clone();
    f.config.socket = path;
    let (signal, received) = tokio::sync::oneshot::channel();
    let handle = tokio::spawn(async move {
        let mut signal = Some(signal);
        let mut clients = tokio::task::JoinSet::new();
        loop {
            let (mut incoming, _) = listener.accept().await.unwrap();
            let bytes = message(&mut incoming).await;
            let signal = if String::from_utf8_lossy(&bytes).contains(action) {
                signal.take()
            } else {
                None
            };
            let upstream = upstream.clone();
            clients.spawn(async move {
                let mut outgoing = UnixStream::connect(upstream).await.unwrap();
                outgoing.write_all(&bytes).await.unwrap();
                if let Some(signal) = signal {
                    let response = message(&mut outgoing).await;
                    assert!(
                        String::from_utf8_lossy(&response).contains(expected),
                        "{}",
                        String::from_utf8_lossy(&response)
                    );
                    signal.send(()).unwrap();
                    std::future::pending::<()>().await;
                } else {
                    let _ = tokio::io::copy_bidirectional(&mut incoming, &mut outgoing).await;
                }
            });
        }
    });
    (original, handle, received)
}
async fn lost_reply(
    f: &mut Fixture,
    server: &mut Process,
    id: Uuid,
    state: &Path,
    app: &Path,
    action: &'static str,
    expected: &'static str,
) {
    let (original, proxy, received) = proxy(f, action, expected).await;
    let diagnostic = f.dir.path().join("check-driver.stderr");
    let mut child = Process(
        cli(f, id, state, app)
            .stdout(std::process::Stdio::null())
            .stderr(fs::File::create(&diagnostic).unwrap())
            .spawn()
            .unwrap(),
    );
    let signal = tokio::time::timeout(Duration::from_secs(30), received).await;
    assert!(
        signal.is_ok(),
        "{action}: CLI {:?}: {}",
        child.0.try_wait().unwrap(),
        fs::read_to_string(&diagnostic).unwrap()
    );
    signal.unwrap().unwrap();
    f.config.socket = original;
    let duplicate = cli(f, id, state, app).output().unwrap();
    assert!(!duplicate.status.success());
    assert!(String::from_utf8_lossy(&duplicate.stderr).contains("lock"));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    proxy.abort();
    let _ = proxy.await;
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    *server = f.start().await;
}

pub(super) async fn exercise(
    f: &mut Fixture,
    server: &mut Process,
    owner: &Path,
    publisher: &Path,
    publication: Uuid,
    restored: &Path,
) {
    f.credential_path = owner.to_owned();
    let pull = Uuid::new_v4();
    result(request(
        f,
        &json!({"action":"pull_request","key":"check-pull","epoch":1,"pull":pull,"specification":{"publication":publication,"base":"main","title":"Managed check fixture","body":"","draft":true,"previous":null,"ttl_seconds":600}}),
    ));
    f.credential_path = publisher.to_owned();
    let attempt = Uuid::new_v4();
    result(request(
        f,
        &json!({"action":"pull_begin","key":"check-pull-begin","epoch":1,"pull":pull,"attempt":attempt}),
    ));
    let c = result(request(
        f,
        &json!({"action":"pull_context","epoch":1,"pull":pull}),
    ));
    let branch: Uuid = c["target"]["branch_id"].as_str().unwrap().parse().unwrap();
    let snapshot = json!({"identity":{"number":18,"node_id":"PR_check_18"},"head_repository":c["specification"]["upstream"]["repository_id"],"base_repository":c["specification"]["upstream"]["repository_id"],"head_ref":format!("treeharbor/{}",branch.simple()),"base_ref":"main","head_revision":c["target"]["git_revision"],"base_revision":c["target"]["git_revision"],"title":c["target"]["title"],"body":c["target"]["body"],"draft":true,"state":"open","merged":false});
    result(request(
        f,
        &json!({"action":"pull_report","key":"check-pull-report","epoch":1,"pull":pull,"report":{"attempt":attempt,"binding":c["binding"],"git_revision":c["target"]["git_revision"],"pull":snapshot,"observed_at_unix_ms":stamp(f).await,"token_revoked":true}}),
    ));

    let database = Database::new(f.config.clone()).unwrap();
    let mut session = database.connect(true).await.unwrap();
    let db = &mut session.metadata;
    let (bridge, _) = actor(
        f,
        db,
        "bridge",
        K::Bridge,
        &[
            P::HostCheck,
            P::HostRead,
            P::HostPublish,
            P::HostMerge,
            P::Read,
        ],
    )
    .await;
    let (policy, _) = actor(f, db, "check-policy", K::Human, &[P::PolicyWrite, P::Read]).await;
    let (validator, _) = actor(
        f,
        db,
        "check-validator",
        K::Validator,
        &[P::ValidationReport, P::Read],
    )
    .await;
    let (release, _) = actor(f, db, "check-release", K::Human, &[P::HostQueue, P::Read]).await;
    let (_, node) = actor(f, db, "check-node", K::Node, &[P::RuntimeControl]).await;

    // Import an actual exported Git history, not fabricated conversion lineage.
    let revision = c["target"]["git_revision"].as_str().unwrap();
    let bundle = f.dir.path().join("check.bundle");
    git_reference::bundle(
        restored,
        &bundle,
        revision.strip_prefix("git:sha1:").unwrap(),
    );
    let RevisionRef::Git(commit) = revision.parse().unwrap() else {
        panic!("Git source required")
    };
    let pack_path = f.dir.path().join("check-source");
    let pack = treeharbor_service::pack::capture_git(treeharbor_service::pack::GitCapture {
        git: Path::new("/usr/bin/git"),
        bundle: &bundle,
        commit,
        output: &pack_path,
        limits: Default::default(),
    })
    .unwrap();
    let manifest: Value =
        serde_json::from_slice(&fs::read(pack_path.join("manifest.json")).unwrap()).unwrap();
    let import = Uuid::new_v4();
    f.credential_path = owner.to_owned();
    result(request(
        f,
        &json!({"action":"import_request","key":"check-import","epoch":1,"import":import,"binding":c["binding"],"specification":{"git_revision":revision,"max_objects":pack.object_count(),"max_bytes":pack.encoded_bytes(),"ttl_seconds":600}}),
    ));
    f.credential_path = bridge.clone();
    result(request(
        f,
        &json!({"action":"import_begin","key":"check-import-begin","epoch":1,"import":import,"native_commit":pack.commit().to_string()}),
    ));
    let credential = f.credential();
    for object in manifest["objects"].as_array().unwrap() {
        let id = object.as_str().unwrap().parse().unwrap();
        let reply = client::send_github_source_object(
            &f.config.socket,
            &credential,
            import,
            id,
            pack.frame(id).unwrap(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert!(reply.status.is_success(), "{:?}", reply.body);
    }
    result(request(
        f,
        &json!({"action":"import_finish","key":"check-import-finish","epoch":1,"import":import}),
    ));
    let profile = Profile::new(Definition {
        version: 1,
        image: format!("sha256:{}", "11".repeat(32)),
        uid: 1001,
        gid: 1001,
        memory_bytes: 268435456,
        nano_cpus: 1_000_000_000,
        max_processes: 64,
        timeout_ms: 10000,
        workspace_bytes: 134217728,
    })
    .unwrap();
    db.register_runtime_profile(&f.scope, &profile, "fixture-operator")
        .await
        .unwrap();
    let check = Uuid::new_v4();
    f.credential_path = policy;
    result(request(
        f,
        &json!({"action":"managed_check_approve_pull","key":"check-approve","epoch":1,"check_run":check,"specification":{"pull":pull,"import":import,"validator":"check-validator","program":{"version":1,"runtime_profile":ObjectId::from_bytes(profile.digest()).to_string(),"argv":["/check"]}}}),
    ));
    f.credential_path = validator;
    result(request(
        f,
        &json!({"action":"managed_check_prepare","key":"check-prepare","epoch":1,"check_run":check}),
    ));
    let storage = Uuid::new_v4();
    let slot = StorageSlot {
        storage,
        slot: Uuid::new_v4(),
    };
    db.register_node_storage(f.scope.tenant(), "check-node", storage, "fixture-operator")
        .await
        .unwrap();
    db.register_runtime_volume(
        f.scope.tenant(),
        "check-node",
        &RuntimeVolume {
            slot,
            digest: [71; 32],
            limit_bytes: 134217728,
        },
        "fixture-operator",
    )
    .await
    .unwrap();
    let job = db
        .claim_preparation(&node, storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    let prep = db.reserve_preparation(&node, &job, slot).await.unwrap();
    db.complete_preparation(
        &node,
        &prep,
        &PreparedFiles {
            snapshot: prep.snapshot(),
            profile: SourceProfile::full().id(),
            manifest_digest: [91; 32],
        },
    )
    .await
    .unwrap();
    result(request(
        f,
        &json!({"action":"managed_check_start","key":"check-start","epoch":1,"check_run":check}),
    ));
    let plan = db
        .claim_run(&node, storage, Uuid::new_v4(), 60)
        .await
        .unwrap()
        .unwrap();
    db.admit_run_start(&node, &plan).await.unwrap();
    // Explicit runtime fixture only; this test proves process/journal recovery.
    f.sql.execute("UPDATE th_managed_runs SET engine='fixture',container=$2,exit_code=0,oom_killed=false,runtime_started_at='2026-10-25T00:00:00Z',runtime_finished_at='2026-10-25T00:00:01Z' WHERE run=$1", &[&plan.run(),&"a".repeat(64)]).await.unwrap();
    db.finish_run(&node, &plan).await.unwrap();
    let collected = result(request(
        f,
        &json!({"action":"managed_check_collect","key":"check-collect","epoch":1,"check_run":check}),
    ));
    assert_eq!(collected["outcome"], "passed");

    f.credential_path = release.clone();
    let id = Uuid::new_v4();
    result(request(
        f,
        &json!({"action":"check_publication_request","key":"check-request","epoch":1,"publication":id,"specification":{"check_run":check,"ttl_seconds":600}}),
    ));
    f.credential_path = bridge.clone();
    let c = result(request(
        f,
        &json!({"action":"check_publication_context","epoch":1,"publication":id}),
    ));
    let state = f.dir.path().join("check-driver");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let app = f.dir.path().join("check-fixture.json");
    let pem = f.dir.path().join("check-fixture.pem");
    files::write_private(
        &pem,
        include_bytes!("../../../../integrations-github/tests/fixtures/test-app.pem"),
    )
    .unwrap();
    files::write_private(&app, json!({"version":1,"app_id":c["specification"]["app_id"],"client_id":c["specification"]["client_id"],"private_key_file":pem,"repository":c["specification"]["upstream"]}).to_string().as_bytes()).unwrap();
    // The grant is committed but withheld from the real CLI. It cannot POST.
    lost_reply(
        f,
        server,
        id,
        &state,
        &app,
        "\"action\":\"check_publication_begin\"",
        "\"dispatch_granted\":true",
    )
    .await;
    fs::remove_file(&app).unwrap();
    fs::remove_file(&pem).unwrap();
    let recovered = cli(f, id, &state, &app).output().unwrap();
    assert!(!recovered.status.success()); // not_sent is not a successful delivery
    let recovered: Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(recovered["state"], "not_sent");
    assert!(recovered["preflight"].is_null());
    assert_eq!(recovered["report_sequence"], 1);
    assert!(state.join("no-dispatch-ack.json").is_file());
    assert!(!request(f, &json!({"action":"check_publication_begin","key":"check-late","epoch":1,"publication":id,"attempt":Uuid::new_v4()})).status.success());

    // A separately authorized intent and host preflight. The retained report is
    // a trusted provider fixture; replay never needs the absent App key.
    f.credential_path = release;
    let id = Uuid::new_v4();
    result(request(
        f,
        &json!({"action":"check_publication_request","key":"check-next","epoch":1,"publication":id,"specification":{"check_run":check,"ttl_seconds":600}}),
    ));
    f.credential_path = bridge;
    let attempt = Uuid::new_v4();
    result(request(
        f,
        &json!({"action":"check_publication_begin","key":"check-next-begin","epoch":1,"publication":id,"attempt":attempt}),
    ));
    let c = result(request(
        f,
        &json!({"action":"check_publication_context","epoch":1,"publication":id}),
    ));
    let t: cp::Target = serde_json::from_value(c["target"].clone()).unwrap();
    let golden: Value = serde_json::from_str(include_str!(
        "../../../../github-contract/tests/fixtures/check-publication-v1.json"
    ))
    .unwrap();
    let mut policy: cp::Policy =
        serde_json::from_value(golden["report"]["preflight"]["policy_before"].clone()).unwrap();
    policy.protection.app_bound_checks = vec![RequiredCheck {
        name: t.name().unwrap(),
        app_id: t.app_id,
    }];
    let p = t.source.pull();
    let view = cp::SourceView::Pull(treeharbor_github_contract::PullState {
        repository_id: p.repository_id,
        number: p.pull_number,
        node_id: p.pull_node.clone(),
        head_ref: p.head_ref.clone(),
        base_ref: p.base_ref.clone(),
        head_revision: p.head_revision.clone(),
        base_revision: p.head_revision.clone(),
        state: "open".into(),
        draft: true,
        merged: false,
    });
    let pref = cp::Preflight {
        before: view.clone(),
        after: view,
        policy_before: policy.clone(),
        policy_after: policy,
    };
    result(request(
        f,
        &json!({"action":"check_publication_dispatch","key":"check-next-dispatch","epoch":1,"publication":id,"attempt":attempt,"preflight":pref}),
    ));
    let now = stamp(f).await;
    let report = cp::Report {
        version: 1,
        started_at_unix_ms: now,
        finished_at_unix_ms: now,
        outcome: cp::Outcome::Observed,
        preflight: Some(pref),
        check: Some(cp::PublishedCheck {
            id: 73,
            app_id: t.app_id,
            name: t.name().unwrap(),
            head_revision: t.source.head().unwrap().into(),
            external_id: t.external_id().unwrap(),
            status: "completed".into(),
            conclusion: t.conclusion,
        }),
        token_revoked: true,
    };
    let state = f.dir.path().join("check-observed-driver");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let intent = json!({"version":1,"tenant":f.scope.tenant(),"repository":f.scope.repository(),"publication":id,"epoch":1,"check_run":c["check_run"],"binding":c["binding"],"target":t,"expires_at_unix_ms":c["expires_at_unix_ms"],"specification":c["specification"]});
    files::write_private(&state.join("intent.json"), intent.to_string().as_bytes()).unwrap();
    files::write_private(
        &state.join("report-0000.json"),
        json!({"version":1,"key":"github-publish-report-check-replay","report":report,"token_expires_at":null})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    lost_reply(
        f,
        server,
        id,
        &state,
        &app,
        "\"action\":\"check_publication_report\"",
        "\"recorded_observation\"",
    )
    .await;
    let recovered = cli(f, id, &state, &app).output().unwrap();
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let v: Value = serde_json::from_slice(&recovered.stdout).unwrap();
    assert_eq!(v["state"], "observed");
    assert_eq!(v["report_sequence"], 1);
    assert_eq!(v["admission_verified"], false);
    assert!(state.join("ack-0000.json").is_file());
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_github_check_publication_reports WHERE publication=$1",
                &[&id]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    assert_eq!(
        f.sql
            .query_one(
                "SELECT count(*) FROM th_outbox WHERE kind='github.check_publication.dispatched'",
                &[]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        1
    );
    let replay = cli(f, id, &state, &app).output().unwrap();
    assert!(replay.status.success());
    drop(session);
}
