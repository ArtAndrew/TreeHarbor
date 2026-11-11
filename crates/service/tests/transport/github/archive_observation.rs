use super::*;
use std::collections::BTreeMap;
use treeharbor_metadata::github::ArchiveInventoryDigest;

const REQUEST_KEY: &str = "confirm-archive-source";

async fn bridge_command(f: &Fixture, credential: &Credential, fields: Value) -> client::Reply {
    client::send(
        &f.config.socket,
        &credential.token,
        credential
            .request(json!({"method":"github","request":fields}))
            .to_string()
            .into_bytes(),
        Duration::from_secs(5),
    )
    .await
    .unwrap()
}

pub(super) fn replay(f: &Fixture, bridge: &Path, state: &Path) -> Value {
    let output = Command::new(&f.binary)
        .env("PATH", "/treeharbor-test-no-executables")
        .args(["github", "archive-observe", "--socket"])
        .arg(&f.config.socket)
        .arg("--credential-file")
        .arg(bridge)
        .args(["--epoch", "1", "--request-key", REQUEST_KEY, "--config"])
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

pub(super) async fn exercise(
    f: &Fixture,
    bridge: &Path,
    stage: &Value,
    proof_key: &str,
) -> (PathBuf, Value) {
    let start = |key: &str, proof: &str| json!({"action":"archive_observe_begin","epoch":1,"key":key,"proof_key":proof});
    let owner = f.credential();
    let bridge_credential = Credential::load(bridge).unwrap();
    let wrong_bridge = Credential::load(&provision_read_bridge(f, "unassigned")).unwrap();
    let missing = bridge_command(f, &owner, start("invalid-proof", "missing-proof")).await;
    assert_eq!(missing.body["error"]["code"], "not_found");
    let begin = start(REQUEST_KEY, proof_key);
    let request_state = result(f.cli(&[
        "archive-observe-begin",
        "--epoch",
        "1",
        "--key",
        REQUEST_KEY,
        "--proof-key",
        proof_key,
    ]));
    assert_eq!(result(request(f, &begin)), request_state);
    assert_eq!(request_state["binding"], stage["binding"]);
    assert_eq!(request_state["upload"], stage["upload"]);
    assert_eq!(
        request_state["spool_fingerprint"],
        stage["specification"]["spool_fingerprint"]
    );
    let context = json!({"action":"archive_observe_context","epoch":1,"request_key":REQUEST_KEY});
    for forbidden in [&owner, &wrong_bridge] {
        let rejected = bridge_command(f, forbidden, context.clone()).await;
        assert_eq!(rejected.status.as_u16(), 403);
    }
    let found = bridge_command(f, &bridge_credential, context).await;
    assert!(found.status.is_success(), "{}", found.body);
    assert_eq!(found.body["result"]["request"], request_state);

    let expired = result(request(f, &start("expired-observation", proof_key)));
    f.sql
        .execute(
            "UPDATE th_operations SET result=jsonb_set(result,'{expires_at_unix}',
         to_jsonb((result->>'observe_after_unix')::bigint)) WHERE key='expired-observation'",
            &[],
        )
        .await
        .unwrap();
    let rejected = bridge_command(
        f,
        &bridge_credential,
        json!({"action":"archive_observe_context","epoch":1,"request_key":expired["key"]}),
    )
    .await;
    assert_eq!(
        rejected.body["error"]["code"],
        "archive_observation_mismatch"
    );

    let spec = &stage["specification"];
    let mut heads = BTreeMap::new();
    let mut inventory_heads = Vec::new();
    for (name, tip) in spec["branches"].as_object().unwrap() {
        let git = tip["git_revision"].as_str().unwrap();
        let short = name.strip_prefix("refs/heads/").unwrap();
        heads.insert(name.clone(), git);
        inventory_heads.push(
            json!({"id":format!("ref-{short}"),"name":short,"prefix":"refs/heads/",
            "target":{"__typename":"Commit","oid":git.strip_prefix("git:sha1:").unwrap()}}),
        );
    }
    let started = request_state["observe_after_unix"].as_u64().unwrap();
    let observation = json!({
        "version":1,"request_key":REQUEST_KEY,"binding":stage["binding"],"app_id":spec["app_id"],
        "upstream":spec["upstream"],"repository_node_id":"fixture-archived-repo",
        "default_branch":spec["default_branch"],"started_at_unix":started,"finished_at_unix":started,
        "token_revoked":true,"heads":heads,
        "tags":ArchiveInventoryDigest::tags(&[] as &[Value]).unwrap(),
        "open_pulls":ArchiveInventoryDigest::open_pulls(&[] as &[Value]).unwrap(),
    });
    let record = |observation: &Value| json!({"action":"archive_observe_record","epoch":1,"observation":observation});
    for (field, value) in [
        ("started_at_unix", json!(started - 1)),
        ("binding", json!(Uuid::new_v4())),
        ("app_id", json!(99)),
        ("token_revoked", json!(false)),
        (
            "heads",
            json!({"refs/heads/main":format!("git:sha1:{}", "ab".repeat(20))}),
        ),
    ] {
        let mut invalid = observation.clone();
        invalid[field] = value;
        let rejected = bridge_command(f, &bridge_credential, record(&invalid)).await;
        assert_eq!(rejected.status.as_u16(), 409, "{field}: {}", rejected.body);
        assert_eq!(
            rejected.body["error"]["code"],
            "archive_observation_mismatch"
        );
    }
    let refused = bridge_command(f, &owner, record(&observation)).await;
    assert_eq!(refused.status.as_u16(), 403);
    let mut changed = observation.clone();
    changed["heads"]["refs/heads/topic"] = json!(format!("git:sha1:{}", "cd".repeat(20)));
    let rejected = bridge_command(f, &bridge_credential, record(&changed)).await;
    assert_eq!(
        rejected.body["error"]["code"],
        "archive_observation_mismatch"
    );

    // Model a captured and fsynced inventory after a lost record response.
    // The normal CLI can resume with the App file deliberately unavailable.
    let state = f.dir.path().join("archive-observation");
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let capture = json!({
        "binding":stage["binding"],
        "inventory":{
            "version":1,
            "archive":{"version":1,"binding":spec["upstream"],"app_id":spec["app_id"],
                "repository_node_id":"fixture-archived-repo","default_branch":spec["default_branch"],
                "started_at_unix":started,"finished_at_unix":started,"token_revoked":true,
                "heads":inventory_heads},
            "tags":[{"id":"tag-one","name":"v1","prefix":"refs/tags/",
                "target":{"__typename":"Tag","oid":"44".repeat(20)}}],
            "open_pulls":[{"id":"pull-one","number":17,"state":"OPEN","isDraft":false,"merged":false,
                "headRefName":"fix","headRefOid":"55".repeat(20),"baseRefName":"main",
                "baseRefOid":heads["refs/heads/main"].strip_prefix("git:sha1:").unwrap(),
                "headRepository":{"databaseId":71},"baseRepository":{"databaseId":spec["upstream"]["repository_id"]},
                "mergeCommit":null}],
        }
    });
    files::write_private(
        &state.join("inventory.json"),
        &serde_json::to_vec(&capture).unwrap(),
    )
    .unwrap();
    let observed = replay(f, bridge, &state);
    assert_eq!(observed["state"], "observed");
    assert_eq!(observed["request"], request_state);
    assert_eq!(observed["observation"]["heads"], json!(heads));
    assert_eq!(observed["observation"]["tags"]["count"], 1);
    assert_eq!(observed["observation"]["open_pulls"]["count"], 1);
    assert_eq!(replay(f, bridge, &state), observed);
    assert!(state.join("receipt.json").is_file());
    let collision = bridge_command(f, &bridge_credential, record(&observation)).await;
    assert_eq!(collision.body["error"]["code"], "idempotency_conflict");
    let count = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_operations WHERE kind='github.archive.observe.record.v1'",
            &[],
        )
        .await
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(count, 1);
    (state, observed)
}
