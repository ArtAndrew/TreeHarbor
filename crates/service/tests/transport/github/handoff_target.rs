use super::{
    archive_activation::native, archive_observation::bridge_command,
    archive_stage::assert_rejected, *,
};
use crate::native::ok;
use treeharbor_metadata::github::ArchiveInventoryDigest;

const PLAN: &str = "return-to-github-plan";
const FREEZE: &str = "freeze-for-destination";
const PROJECTION: &str = "project-for-destination";

fn replay_observation(f: &Fixture, bridge: &Path, state: &Path) -> Value {
    let output = Command::new(&f.binary)
        .args(["github", "handoff-observe", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(bridge)
        .args(["--epoch", "3", "--request-key", PLAN, "--config"])
        .arg(f.dir.path().join("absent-app-config.json"))
        .arg("--state")
        .arg(state)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

async fn owner_command(f: &Fixture, owner: &Credential, request: Value) -> Value {
    let reply = bridge_command(f, owner, request).await;
    assert!(reply.status.is_success(), "{}", reply.body);
    reply.body["result"].clone()
}

async fn catalogue_state(f: &Fixture, entry: Uuid) -> Value {
    f.sql.query_one(r#"
        SELECT jsonb_build_object('authority',authority,'epoch',epoch,'state',state,
          'active_bindings',(SELECT count(*) FROM th_github_bindings WHERE state='active'),
          'pending_events',(SELECT count(*) FROM th_outbox WHERE state='pending'),
          'queue_state',(SELECT state FROM th_native_queue WHERE entry=$1),
          'native_refs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.name) FROM th_native_refs r),
          'change_versions',(SELECT jsonb_agg(to_jsonb(v) ORDER BY v.changeset,v.version) FROM th_change_versions v),
          'validations',(SELECT count(*) FROM th_native_validations))
        FROM th_repositories
        "#, &[&entry]).await.unwrap().get(0)
}

fn stage_capture(f: &Fixture, target: &Value, plan: &Value, heads: &Value) -> PathBuf {
    let inventory_heads = heads.as_object().unwrap().iter().map(|(name, revision)| {
        let short = name.strip_prefix("refs/heads/").unwrap();
        json!({"id":format!("ref-{short}"),"name":short,"prefix":"refs/heads/",
            "target":{"__typename":"Commit","oid":revision.as_str().unwrap().strip_prefix("git:sha1:").unwrap()}})
    }).collect::<Vec<_>>();
    let state = f.dir.path().join("target-bridge-journal");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let config = &target["specification"];
    let capture = json!({"binding":target["binding"],"inventory":{"version":1,
        "archive":{"version":1,"binding":config["upstream"],"app_id":config["app_id"],
        "repository_node_id":target["repository_node_id"],"default_branch":target["default_branch"],
        "started_at_unix":plan["observe_after_unix"],"finished_at_unix":plan["observe_after_unix"],
        "token_revoked":true,"heads":inventory_heads},"tags":[],"open_pulls":[]}});
    files::write_private(
        &state.join("inventory.json"),
        &serde_json::to_vec(&capture).unwrap(),
    )
    .unwrap();
    state
}

pub(super) async fn exercise(
    f: &Fixture,
    server: Process,
    owner_file: &Path,
    bridge_file: &Path,
    changeset: &str,
) {
    let entry = Uuid::new_v4();
    ok(native(
        f,
        owner_file,
        json!({"action":"enqueue","key":"queue-before-connected","epoch":3,
        "entry":entry,"target":"stable","changeset":changeset,"version":2}),
    ));
    let freeze = ok(native(
        f,
        owner_file,
        json!({"action":"handoff_freeze","key":FREEZE,"epoch":3}),
    ));
    let destination = f.dir.path().join("destination-bundle");
    let path = destination.to_str().unwrap();
    let exported = f.cli_with_credential(
        owner_file,
        &[
            "git-export",
            "--epoch",
            "3",
            "--freeze-key",
            FREEZE,
            "--output",
            path,
        ],
    );
    assert!(
        exported.status.success(),
        "{}",
        String::from_utf8_lossy(&exported.stderr)
    );
    let projection = ok(f.cli_with_credential(
        owner_file,
        &[
            "handoff-verify",
            "--epoch",
            "3",
            "--key",
            PROJECTION,
            "--handoff",
            path,
        ],
    ));
    let heads = &projection["specification"]["refs"];

    let previous = f
        .sql
        .query_one(
            "SELECT binding,specification FROM th_github_bindings WHERE state='revoked'",
            &[],
        )
        .await
        .unwrap();
    let old_binding: Uuid = previous.get(0);
    let config: Value = previous.get(1);
    let target = json!({"version":1,"projection_key":PROJECTION,"binding":Uuid::new_v4(),
        "specification":config,"repository_node_id":"fixture-archived-repo","default_branch":"main"});
    let owner = Credential::load(owner_file).unwrap();
    let bridge = Credential::load(bridge_file).unwrap();
    let stranger = Credential::load(&provision_read_bridge(f, "not-destination-bridge")).unwrap();
    let plan_request = json!({"action":"handoff_plan","epoch":3,"key":PLAN,"target":target});
    let mut reused = plan_request.clone();
    reused["target"]["binding"] = json!(old_binding);
    let refused = bridge_command(f, &owner, reused).await;
    assert_eq!(refused.body["error"]["code"], "host_binding_state");
    let plan = owner_command(f, &owner, plan_request.clone()).await;
    assert_eq!(plan["snapshot_sha256"], freeze["snapshot_sha256"]);
    assert_eq!(plan["target"], target);
    assert_eq!(owner_command(f, &owner, plan_request).await, plan);
    let context = json!({"action":"handoff_context","epoch":3,"plan_key":PLAN});
    for unauthorized in [&owner, &stranger] {
        let rejected = bridge_command(f, unauthorized, context.clone()).await;
        assert_eq!(rejected.status.as_u16(), 403);
    }
    assert_eq!(owner_command(f, &bridge, context.clone()).await, plan);
    let activate = json!({"action":"handoff_activate","key":"give-github-authority","epoch":3,
        "plan_key":PLAN,"keep_target_archived":true});
    let premature = bridge_command(f, &owner, activate.clone()).await;
    assert_eq!(premature.body["error"]["code"], "not_found");
    let initial = catalogue_state(f, entry).await;
    assert_eq!(initial["authority"], "standalone");
    assert_eq!(initial["state"], "frozen");
    assert_eq!(initial["active_bindings"], 0);

    let starts = plan["observe_after_unix"].as_u64().unwrap();
    let observation = json!({"version":1,"request_key":PLAN,"binding":target["binding"],
        "app_id":config["app_id"],"upstream":config["upstream"],"repository_node_id":target["repository_node_id"],
        "default_branch":target["default_branch"],"started_at_unix":starts,"finished_at_unix":starts,
        "token_revoked":true,"heads":heads,"tags":ArchiveInventoryDigest::tags(&[] as &[Value]).unwrap(),
        "open_pulls":ArchiveInventoryDigest::open_pulls(&[] as &[Value]).unwrap()});
    for (field, value) in [
        ("heads", json!({"refs/heads/main":heads["refs/heads/main"]})),
        ("repository_node_id", json!("same-name-other-repository")),
        ("default_branch", json!("stable")),
        ("started_at_unix", json!(starts - 1)),
        ("token_revoked", json!(false)),
    ] {
        let mut changed = observation.clone();
        changed[field] = value;
        let refused = bridge_command(
            f,
            &bridge,
            json!({"action":"handoff_observe","epoch":3,"observation":changed}),
        )
        .await;
        assert_eq!(
            refused.body["error"]["code"], "archive_observation_mismatch",
            "{field}: {}",
            refused.body
        );
    }
    let state = stage_capture(f, &target, &plan, heads);
    let recorded = replay_observation(f, bridge_file, &state);
    assert_eq!(recorded["plan"], plan);
    assert_eq!(recorded["observation"], observation);
    assert_eq!(replay_observation(f, bridge_file, &state), recorded);

    let mut unsafe_activation = activate.clone();
    unsafe_activation["keep_target_archived"] = json!(false);
    let refused = bridge_command(f, &owner, unsafe_activation).await;
    assert_eq!(refused.body["error"]["code"], "invalid_request");
    f.sql.batch_execute(r#"
        CREATE FUNCTION reject_target_activation() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.kind='github.handoff.activate.v1' THEN RAISE EXCEPTION 'injected activation failure'; END IF;
            RETURN NEW;
        END
        $$;
        CREATE TRIGGER reject_target_activation BEFORE INSERT ON th_operations
        FOR EACH ROW EXECUTE FUNCTION reject_target_activation();
        "#).await.unwrap();
    let failed = bridge_command(f, &owner, activate.clone()).await;
    assert_eq!(failed.body["error"]["code"], "internal_error");
    assert_eq!(catalogue_state(f, entry).await, initial);
    f.sql.batch_execute("DROP TRIGGER reject_target_activation ON th_operations; DROP FUNCTION reject_target_activation();").await.unwrap();

    let switched = owner_command(f, &owner, activate.clone()).await;
    assert_eq!(switched["authority"], "connected");
    assert_eq!(switched["epoch"], 4);
    assert_eq!(switched["binding"], target["binding"]);
    assert_eq!(switched["target_archived"], true);
    assert_eq!(switched["publication_ready"], false);
    drop(server);
    let _restarted = f.start().await;
    assert_eq!(owner_command(f, &owner, activate).await, switched);
    let mut expected = initial;
    expected["authority"] = json!("connected");
    expected["epoch"] = json!(4);
    expected["state"] = json!("active");
    expected["active_bindings"] = json!(1);
    expected["pending_events"] = json!(1);
    expected["queue_state"] = json!("stale");
    assert_eq!(catalogue_state(f, entry).await, expected);
    let old = bridge_command(f, &bridge, context).await;
    assert_eq!(old.body["error"]["code"], "repository_fenced");
    assert_rejected(
        native(
            f,
            owner_file,
            json!({"action":"handoff_resume","key":"resume-after-connected",
        "epoch":3,"freeze_key":FREEZE,"target_stays_fenced":true}),
        ),
        "repository_fenced",
    );

    let begin = |binding: Value| {
        json!({"action":"probe_begin","epoch":4,"key":Uuid::new_v4(),
        "binding":binding,"probe":Uuid::new_v4()})
    };
    let previous_binding = bridge_command(f, &bridge, begin(json!(old_binding))).await;
    assert_eq!(previous_binding.body["error"]["code"], "host_binding_state");
    let begun = owner_command(f, &bridge, begin(target["binding"].clone())).await;
    assert_eq!(begun["outcome"], "requested");
    let probe = owner_command(
        f,
        &bridge,
        json!({"action":"probe_context","epoch":4,
        "binding":target["binding"],"probe":begun["probe"]}),
    )
    .await;
    assert_eq!(probe["epoch"], 4);
    assert_eq!(probe["specification"], config);
    assert_eq!(probe["state"], "pending");
    assert!(probe["observation"].is_null());
    assert_eq!(probe["publication_ready"], false);

    let projection_id = Uuid::new_v4();
    let requested = owner_command(f, &owner, json!({"action":"projection_request","epoch":4,
        "key":"project-carried-change","projection":projection_id,"binding":target["binding"],
        "specification":{"changeset":changeset,"version":2,"title":"Carry retained change","ttl_seconds":600}})).await;
    assert_eq!(requested["state"], "pending");
    let prepared = result(
        Command::new(&f.binary)
            .args(["github", "prepare", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(owner_file)
            .args([
                "--epoch",
                "4",
                "--key",
                "prepare-carried-change",
                "--projection",
                &projection_id.to_string(),
            ])
            .output()
            .unwrap(),
    );
    assert_eq!(prepared["state"], "ready");
    assert_eq!(prepared["change_version"], 2);
    assert_eq!(prepared["github_pull"], requested["github_pull"]);
    assert!(prepared["github_pull"].is_object());
    assert_eq!(prepared["publication_ready"], false);
}
