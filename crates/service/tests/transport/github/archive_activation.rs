use super::{archive_observation::bridge_command, archive_stage::assert_rejected, *};
use crate::native::{ok, role};
use treeharbor_core::ObjectId;
use treeharbor_metadata::github::ArchiveInventoryDigest;

async fn observe(
    f: &Fixture,
    bridge: &Credential,
    baseline: &Value,
    key: &str,
    changed: bool,
) -> Value {
    let started = result(request(
        f,
        &json!({
            "action":"archive_observe_begin","epoch":1,"key":key,"proof_key":baseline["request"]["proof_key"],
        }),
    ));
    let mut observation = baseline["observation"].clone();
    observation["request_key"] = json!(key);
    observation["started_at_unix"] = started["observe_after_unix"].clone();
    observation["finished_at_unix"] = started["observe_after_unix"].clone();
    if changed {
        observation["open_pulls"] =
            json!(ArchiveInventoryDigest::open_pulls(&[] as &[Value]).unwrap());
    }
    let reply = bridge_command(
        f,
        bridge,
        json!({"action":"archive_observe_record","epoch":1,"observation":observation}),
    )
    .await;
    assert!(reply.status.is_success(), "{}", reply.body);
    reply.body["result"].clone()
}

fn mutation(action: &str) -> Value {
    json!({"action":action,"key":Uuid::new_v4(),"epoch":2})
}

pub(super) fn native(f: &Fixture, credential: &Path, fields: Value) -> Output {
    let path = f
        .dir
        .path()
        .join(format!("activation-native-{}.json", Uuid::new_v4()));
    files::write_private(&path, &serde_json::to_vec(&fields).unwrap()).unwrap();
    f.cli_with_credential(credential, &["native", "--request", path.to_str().unwrap()])
}

async fn assert_before_activation(f: &Fixture) {
    let row = f
        .sql
        .query_one(
            r#"
        SELECT authority,epoch,
            (SELECT count(*) FROM th_native_refs),
            (SELECT count(*) FROM th_changesets),
            (SELECT count(*) FROM th_object_pins WHERE holder LIKE 'change:%'),
            (SELECT count(*) FROM th_github_bindings WHERE state='active'),
            (SELECT count(*) FROM th_operations WHERE kind='github.archive.activate.v1')
        FROM th_repositories
        "#,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, &str>(0), "connected");
    assert_eq!(row.get::<_, i64>(1), 1);
    assert_eq!(row.get::<_, i64>(2), 0);
    assert_eq!(row.get::<_, i64>(3), 0);
    assert_eq!(row.get::<_, i64>(4), 0);
    assert_eq!(row.get::<_, i64>(5), 1);
    assert_eq!(row.get::<_, i64>(6), 0);
}

pub(super) async fn exercise(
    f: &mut Fixture,
    server: Process,
    bridge: &Path,
    branch_stage: &Value,
    baseline: &Value,
    retained: archive_payload::Retained,
) {
    let owner = role(
        f,
        "importer",
        None,
        &[
            "read",
            "repository.write",
            "workspace.write",
            "policy.write",
            "change.write",
            "queue.request",
        ],
    );
    let worker = role(
        f,
        "migration-worker",
        Some("integrator"),
        &["read", "integration.control"],
    );
    let ci = role(
        f,
        "migration-ci",
        Some("validator"),
        &["read", "validation.report"],
    );
    let reviewer = role(f, "migration-reviewer", Some("human"), &["read", "review"]);
    let bridge_credential = Credential::load(bridge).unwrap();
    let changed = observe(
        f,
        &bridge_credential,
        baseline,
        "changed-activation-inventory",
        true,
    )
    .await;
    let fresh = observe(
        f,
        &bridge_credential,
        baseline,
        "final-activation-inventory",
        false,
    )
    .await;
    let proposal_stage = &retained.proposal_stage;
    let digest = ObjectId::from_bytes([6; 32]).to_string();
    let mut activation = json!({"key":"activate-native-archive","epoch":1,"specification":{
        "version":1,"proposal_stage":proposal_stage["key"],"observation_key":fresh["request"]["observation_key"],
        "keep_host_archived":true,
        "policy":{"version":1,"checks":[{"name":"tests","definition":digest,"environment":digest,
                    "toolchain":digest,"dependencies":digest,"validators":["migration-ci"]}],
                    "reviewers":["migration-reviewer"],"approvals":1}
    }});
    let dir = f.dir.path().to_owned();
    let invoke = |request: &Value, credential: &Path| {
        let path = dir.join(format!("activation-{}.json", Uuid::new_v4()));
        files::write_private(&path, &serde_json::to_vec(request).unwrap()).unwrap();
        Command::new(&f.binary)
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["github", "archive-activate", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(credential)
            .arg("--request")
            .arg(path)
            .output()
            .unwrap()
    };
    assert_rejected(invoke(&activation, bridge), "forbidden");
    assert_rejected(invoke(&activation, &f.credential_path), "forbidden");
    let mut not_archived = activation.clone();
    not_archived["specification"]["keep_host_archived"] = json!(false);
    assert_rejected(invoke(&not_archived, &owner), "invalid_request");
    for stale in [baseline, &changed] {
        activation["specification"]["observation_key"] =
            stale["request"]["observation_key"].clone();
        assert_rejected(invoke(&activation, &owner), "archive_observation_mismatch");
    }
    activation["specification"]["observation_key"] = fresh["request"]["observation_key"].clone();
    let leased: i64 = f.sql.query_one(r#"
        UPDATE th_outbox SET state='running',worker=$1,lease_until=clock_timestamp()+interval '1 minute'
        WHERE kind='github.binding.configured' RETURNING id
        "#, &[&Uuid::new_v4()]).await.unwrap().get(0);
    assert_rejected(invoke(&activation, &owner), "archive_activation_busy");
    f.sql
        .execute(
            "UPDATE th_outbox SET state='pending',worker=NULL,lease_until=NULL WHERE id=$1",
            &[&leased],
        )
        .await
        .unwrap();

    let withheld = f.sql.query_one(r#"
        DELETE FROM th_object_pins WHERE holder LIKE 'archive-proposal-%' AND digest=decode($1,'hex')
        RETURNING holder,digest,generation
        "#, &[&proposal_stage["specification"]["manifest_object"].as_str().unwrap().strip_prefix("sha256:").unwrap()])
        .await.unwrap();
    assert_rejected(invoke(&activation, &owner), "upload_membership");
    f.sql.execute(r#"
        INSERT INTO th_object_pins(tenant,repository,holder,digest,generation) VALUES($1,$2,$3,$4,$5)
        "#, &[&f.scope.tenant(), &f.scope.repository(), &withheld.get::<_, String>(0),
            &withheld.get::<_, Vec<u8>>(1), &withheld.get::<_, i64>(2)]).await.unwrap();

    f.sql
        .batch_execute(
            r#"
        CREATE FUNCTION reject_archive_activation() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.kind='github.archive.activate.v1' THEN
                RAISE EXCEPTION 'injected authority receipt failure';
            END IF;
            RETURN NEW;
        END
        $$;
        CREATE TRIGGER reject_archive_activation BEFORE INSERT ON th_operations
        FOR EACH ROW EXECUTE FUNCTION reject_archive_activation();
        "#,
        )
        .await
        .unwrap();
    assert_rejected(invoke(&activation, &owner), "internal_error");
    assert_before_activation(f).await;
    f.sql.batch_execute("DROP TRIGGER reject_archive_activation ON th_operations; DROP FUNCTION reject_archive_activation();").await.unwrap();
    drop(server);
    let server = f.start().await;
    let activated = ok(invoke(&activation, &owner));
    assert_eq!(activated["authority"], "standalone");
    assert_eq!(activated["epoch"], 2);
    assert_eq!(activated["branches"], 3);
    assert_eq!(activated["proposals"][0]["number"], 17);
    assert_eq!(activated["unresolved"], json!([]));
    assert_eq!(ok(invoke(&activation, &owner)), activated);
    assert_rejected(
        request(
            f,
            &json!({"action":"revoke","key":"old-writer-attempt","epoch":1,"binding":branch_stage["binding"]}),
        ),
        "repository_fenced",
    );
    let row = f
        .sql
        .query_one(
            r#"
        SELECT authority,epoch,(SELECT count(*) FROM th_github_bindings WHERE state='active'),
            (SELECT count(*) FROM th_agent_runs),(SELECT count(*) FROM th_native_validations),
            (SELECT count(*) FROM th_native_queue),(SELECT count(*) FROM th_archive_pull_origins)
        FROM th_repositories
        "#,
            &[],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, &str>(0), "standalone");
    assert_eq!(row.get::<_, i64>(1), 2);
    assert_eq!(
        (2..=6).map(|i| row.get::<_, i64>(i)).collect::<Vec<_>>(),
        [0, 0, 0, 0, 1]
    );
    let snapshot = ok(native(f, &owner, json!({"action":"ref_snapshot"})));
    for branch in snapshot["refs"].as_array().unwrap() {
        let name = format!("refs/heads/{}", branch["name"].as_str().unwrap());
        assert_eq!(
            branch["head"],
            branch_stage["specification"]["branches"][name]["native_revision"]
        );
    }
    let main = &branch_stage["specification"]["branches"]["refs/heads/main"];
    let changeset = activated["proposals"][0]["changeset"].as_str().unwrap();
    let landed = Box::pin(check_and_land(
        f, &owner, &worker, &ci, &reviewer, changeset, main,
    ))
    .await;
    Box::pin(native_handoff::exercise(
        f, server, &owner, &worker, changeset, &landed,
    ))
    .await;
}

async fn check_and_land(
    f: &Fixture,
    owner: &Path,
    worker: &Path,
    ci: &Path,
    reviewer: &Path,
    changeset: &str,
    main: &Value,
) -> Value {
    let diff = ok(f.cli_with_credential(
        owner,
        &[
            "diff",
            "--authority",
            "standalone",
            "--epoch",
            "2",
            "change",
            "--changeset",
            changeset,
            "--version",
            "1",
        ],
    ));
    let origin = &diff["identity"]["github_pull"];
    assert!(diff["identity"]["run"].is_null());
    assert_eq!(origin["source"]["number"], 17);
    assert_eq!(origin["source"]["headRepository"]["databaseId"], 71);
    assert!(origin["discussion"].as_str().unwrap().ends_with("/pull/17"));
    assert_eq!(diff["changes"][0]["path_hex"], "7061796c6f61642e62696e");
    assert_eq!(diff["changes"].as_array().unwrap().len(), 2);
    let rebase = Uuid::new_v4().to_string();
    let started = ok(f.cli_with_credential(
        owner,
        &[
            "rebase-start",
            "--key",
            "start-archived-pull-rebase",
            "--epoch",
            "2",
            "--rebase",
            &rebase,
            "--changeset",
            changeset,
            "--source-version",
            "1",
            "--onto",
            main["native_revision"].as_str().unwrap(),
        ],
    ));
    assert_eq!(started["github_pull"], *origin);
    let rebased = ok(f.cli_with_credential(
        owner,
        &[
            "rebase-prepare",
            "--key",
            "finish-archived-pull-rebase",
            "--epoch",
            "2",
            "--rebase",
            &rebase,
        ],
    ));
    assert_eq!(rebased["result_version"], 2);
    assert_eq!(rebased["github_pull"], *origin);
    assert!(rebased["run"].is_null());
    let entry = Uuid::new_v4();
    let mut enqueue = mutation("enqueue");
    enqueue["entry"] = json!(entry);
    enqueue["target"] = json!("main");
    enqueue["changeset"] = json!(changeset);
    enqueue["version"] = json!(2);
    ok(native(f, owner, enqueue));
    let candidate = Uuid::new_v4().to_string();
    let mut start = mutation("candidate_start");
    start["entry"] = json!(entry);
    start["candidate"] = json!(candidate);
    ok(native(f, worker, start));
    let prepared = ok(f.cli_with_credential(
        worker,
        &[
            "native-prepare",
            "--key",
            "prepare-imported-candidate",
            "--epoch",
            "2",
            "--candidate",
            &candidate,
        ],
    ));
    assert_eq!(prepared["state"], "checking");
    let reviewed = ok(f.cli_with_credential(
        reviewer,
        &[
            "diff",
            "--authority",
            "standalone",
            "--epoch",
            "2",
            "candidate",
            "--candidate",
            &candidate,
        ],
    ));
    assert_eq!(reviewed["identity"]["github_pull"], *origin);
    assert_eq!(reviewed["identity"]["revision"], prepared["revision"]);
    assert_eq!(reviewed["identity"]["candidate_epoch"], 2);
    assert_eq!(
        reviewed["changes"][0]["after"]["object"],
        Object::Blob(b"proposal from a fork\x00\xff\n".to_vec())
            .id()
            .unwrap()
            .to_string()
    );
    let mut land = mutation("land");
    land["candidate"] = json!(candidate);
    assert!(!native(f, worker, land.clone()).status.success());
    let run = Uuid::new_v4();
    let mut assign = mutation("check_assign");
    assign["candidate"] = json!(candidate);
    assign["run"] = json!(run);
    assign["name"] = json!("tests");
    assign["validator"] = json!("migration-ci");
    ok(native(f, worker, assign));
    let mut report = mutation("validation_report");
    report["run"] = json!(run);
    report["receipt"] = json!(Uuid::new_v4());
    report["outcome"] = json!("passed");
    report["evidence"] = reviewed["changes"][0]["after"]["object"].clone();
    ok(native(f, ci, report));
    assert!(!native(f, worker, land.clone()).status.success());
    let mut review = mutation("review");
    review["candidate"] = json!(candidate);
    review["review"] = json!(Uuid::new_v4());
    review["decision"] = json!("approve");
    ok(native(f, reviewer, review));
    let landed = ok(native(f, worker, land));
    assert_eq!(landed["state"], "landed");
    assert_eq!(landed["revision"], prepared["revision"]);

    let export = f.dir.path().join("activated-git-export");
    let output = f.cli_with_credential(
        owner,
        &[
            "git-export",
            "--epoch",
            "2",
            "--output",
            export.to_str().unwrap(),
        ],
    );
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let restored = f.dir.path().join("activated-git-repo");
    git_reference::git(
        f.dir.path(),
        &[
            "clone",
            "--bare",
            export.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(
        git_reference::git(
            &restored,
            &["cat-file", "blob", "refs/heads/main:payload.bin"],
            b""
        ),
        b"proposal from a fork\x00\xff\n"
    );
    let base = main["git_revision"]
        .as_str()
        .unwrap()
        .strip_prefix("git:sha1:")
        .unwrap();
    git_reference::git(
        &restored,
        &["merge-base", "--is-ancestor", base, "refs/heads/main"],
        b"",
    );
    landed
}
