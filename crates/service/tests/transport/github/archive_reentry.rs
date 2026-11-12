use super::{
    archive_activation::native,
    archive_stage::{assert_rejected, write_archive_spool},
    *,
};
use crate::native::ok;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use treeharbor_github::{ArchivedBranches, InventoryHead, InventoryObject};

const EPOCH: &str = "4";
const BASELINE: &str = "observe-returned-github";
const PROOF: &str = "verify-returned-github";

fn returned_bundle(f: &Fixture, previous: &Path, target: &Value) -> (PathBuf, ArchivedBranches) {
    let dir = f.dir.path();
    let repo = dir.join("returned-github.git");
    git_reference::git(
        dir,
        &[
            "clone",
            "--mirror",
            previous.join("history.bundle").to_str().unwrap(),
            repo.to_str().unwrap(),
        ],
        b"",
    );
    let git = |args: &[&str]| String::from_utf8(git_reference::git(&repo, args, b"")).unwrap();
    let main = git(&["rev-parse", "refs/heads/main"]);
    let topic = git(&["rev-parse", "refs/heads/topic"]);
    let content = git_reference::hash(
        &repo,
        "blob",
        b"a change committed while GitHub owned main\n",
    );
    let listing = git(&["ls-tree", "refs/heads/main"]);
    let tree = git_reference::git(
        &repo,
        &["mktree"],
        format!("{listing}100644 blob {content}\thost.txt\n").as_bytes(),
    );
    let tree = String::from_utf8(tree).unwrap();
    let merge = git_reference::hash(&repo, "commit", format!(
        "tree {}\nparent {}\nparent {}\nauthor Host <host@example.invalid> 1234567890 +0000\ncommitter Host <host@example.invalid> 1234567890 +0000\n\nadvance while Connected\n",
        tree.trim(), main.trim(), topic.trim(),
    ).as_bytes());
    git(&["update-ref", "refs/heads/main", &merge]);
    git(&["update-ref", "refs/heads/host-work", &merge]);
    let bundle = dir.join("returned-github.bundle");
    git(&["bundle", "create", bundle.to_str().unwrap(), "--branches"]);
    let heads = git(&[
        "for-each-ref",
        "--format=%(refname:short) %(objectname)",
        "refs/heads",
    ])
    .lines()
    .map(|line| {
        let (name, oid) = line.split_once(' ').unwrap();
        InventoryHead {
            id: format!("return-ref-{name}"),
            name: name.into(),
            prefix: "refs/heads/".into(),
            target: InventoryObject {
                kind: "Commit".into(),
                oid: oid.into(),
            },
        }
    })
    .collect();
    let spec = &target["specification"];
    let archive = ArchivedBranches {
        version: 1,
        binding: serde_json::from_value(spec["upstream"].clone()).unwrap(),
        app_id: spec["app_id"].as_u64().unwrap(),
        repository_node_id: target["repository_node_id"].as_str().unwrap().into(),
        default_branch: "main".into(),
        started_at_unix: 1,
        finished_at_unix: 2,
        heads,
        token_revoked: true,
    };
    (bundle, archive)
}

fn observe(
    f: &Fixture,
    bridge: &Path,
    binding: &Value,
    archive: &ArchivedBranches,
    key: &str,
) -> (PathBuf, Value) {
    let begun = result(f.cli(&[
        "archive-observe-begin",
        "--epoch",
        EPOCH,
        "--key",
        key,
        "--proof-key",
        PROOF,
    ]));
    let state = f.dir.path().join(key);
    fs::create_dir(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();
    let mut archive = json!(archive);
    archive["started_at_unix"] = begun["observe_after_unix"].clone();
    archive["finished_at_unix"] = begun["observe_after_unix"].clone();
    let capture = json!({"binding":binding,"inventory":{"version":1,"archive":archive,"tags":[],"open_pulls":[]}});
    files::write_private(
        &state.join("inventory.json"),
        &serde_json::to_vec(&capture).unwrap(),
    )
    .unwrap();
    let recorded = archive_payload::success(
        Command::new(&f.binary)
            .args(["github", "archive-observe", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(bridge)
            .args(["--epoch", EPOCH, "--request-key", key, "--config"])
            .arg(f.dir.path().join("absent-reentry-app-config.json"))
            .arg("--state")
            .arg(&state)
            .output()
            .unwrap(),
    );
    assert_eq!(recorded["state"], "observed");
    (state, recorded)
}

fn stage(
    f: &Fixture,
    bridge: &Path,
    bundle: &Path,
    archive: &ArchivedBranches,
    binding: &Value,
) -> Value {
    let dir = f.dir.path();
    let spool = dir.join("reentry-spool");
    let spec = write_archive_spool(&spool, bundle, archive);
    let receipt = result(f.cli(&[
        "archive-stage",
        "--spool",
        spool.to_str().unwrap(),
        "--archive-fingerprint",
        spec["spool_fingerprint"].as_str().unwrap(),
        "--binding",
        binding.as_str().unwrap(),
        "--epoch",
        EPOCH,
        "--state",
        dir.join("reentry-stage.json").to_str().unwrap(),
    ]));
    let key = format!("archive-{}-stage", receipt["upload"].as_str().unwrap());
    assert_eq!(
        result(f.cli(&[
            "archive-verify",
            "--stage-key",
            &key,
            "--key",
            PROOF,
            "--epoch",
            EPOCH
        ]))["state"],
        "verified"
    );
    let (state, _) = observe(f, bridge, binding, archive, BASELINE);
    let preserved = dir.join("reentry-preserved");
    archive_payload::success(
        Command::new(&f.binary)
            .args(["github", "archive-preserve", "--state"])
            .arg(state)
            .arg("--bundle")
            .arg(bundle)
            .arg("--output")
            .arg(&preserved)
            .output()
            .unwrap(),
    );
    let github = |action: &str| {
        let mut command = Command::new(&f.binary);
        command
            .args(["github", action, "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(&f.credential_path)
            .args(["--epoch", EPOCH]);
        command
    };
    let retained = ok(github("archive-retain")
        .arg("--archive")
        .arg(&preserved)
        .arg("--state")
        .arg(dir.join("reentry-retain.json"))
        .output()
        .unwrap());
    let prepared = dir.join("reentry-proposals");
    let proposals = archive_payload::success(
        Command::new(&f.binary)
            .args(["github", "archive-proposals", "--archive"])
            .arg(preserved)
            .arg("--archive-fingerprint")
            .arg(
                retained["specification"]["manifest_sha256"]
                    .as_str()
                    .unwrap(),
            )
            .arg("--output")
            .arg(&prepared)
            .output()
            .unwrap(),
    );
    assert_eq!(proposals["open_pulls"], 0);
    ok(github("archive-proposals-stage")
        .arg("--retained-key")
        .arg(retained["key"].as_str().unwrap())
        .arg("--prepared")
        .arg(prepared)
        .arg("--fingerprint")
        .arg(proposals["manifest_sha256"].as_str().unwrap())
        .arg("--state")
        .arg(dir.join("reentry-proposal-stage.json"))
        .output()
        .unwrap())
}

async fn native_rows(f: &Fixture) -> Value {
    f.sql
        .query_one(
            r#"
        SELECT jsonb_build_object('authority',authority,'epoch',epoch,
            'refs',(SELECT jsonb_agg(to_jsonb(r) ORDER BY r.name) FROM th_native_refs r),
            'history',(SELECT count(*) FROM th_native_ref_updates),
            'changes',(SELECT count(*) FROM th_change_versions)) FROM th_repositories
        "#,
            &[],
        )
        .await
        .unwrap()
        .get(0)
}

async fn upgrade_populated_schema(f: &Fixture) {
    let before = native_rows(f).await;
    // Reconstruct the published v34 constraints on the populated fixture.
    f.sql
        .batch_execute(
            r#"
        ALTER TABLE th_native_ref_updates DROP CONSTRAINT th_native_archive_ref_update_check;
        ALTER TABLE th_native_ref_updates DROP CONSTRAINT th_native_ref_updates_kind_check;
        ALTER TABLE th_native_ref_updates ADD CHECK(kind IN ('create','policy','land'));
        "#,
        )
        .await
        .unwrap();
    let mut migrations: Vec<_> = fs::read_dir(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../metadata/migrations"
    ))
    .unwrap()
    .map(|entry| entry.unwrap().path())
    .filter(|path| path.extension().is_some_and(|ext| ext == "sql"))
    .collect();
    migrations.sort();
    let mut digest = Vec::new();
    for migration in migrations.iter().take(34) {
        digest = Sha256::digest([digest, fs::read(migration).unwrap()].concat()).to_vec();
    }
    f.sql
        .execute(
            "UPDATE th_schema_version SET version=34,migration_digest=$1",
            &[&digest],
        )
        .await
        .unwrap();
    f.admin_ok(&["migrate"]);
    assert_eq!(
        f.sql
            .query_one("SELECT version FROM th_schema_version", &[])
            .await
            .unwrap()
            .get::<_, i32>(0),
        35
    );
    assert_eq!(native_rows(f).await, before);
}

pub(super) async fn exercise(
    f: &Fixture,
    server: Process,
    owner: &Path,
    bridge: &Path,
    previous: &Path,
    target: &Value,
    freeze: &Value,
) {
    drop(server);
    upgrade_populated_schema(f).await;
    let _upgraded = f.start().await;
    let (bundle, archive) = returned_bundle(f, previous, target);
    let binding = &target["binding"];
    let staged = stage(f, bridge, &bundle, &archive, binding);
    let (_, fresh) = observe(f, bridge, binding, &archive, "verify-reentry-inventory");
    let policy = freeze["snapshot"]["refs"][0]["policy"].as_str().unwrap();
    let old_policy: Vec<u8> = f
        .sql
        .query_one(
            "SELECT definition FROM th_native_policies WHERE digest=decode($1,'hex')",
            &[&policy.strip_prefix("sha256:").unwrap()],
        )
        .await
        .unwrap()
        .get(0);
    let request = json!({"key":"reenter-native", "epoch":4, "specification":{
        "version":1,"proposal_stage":staged["key"],"observation_key":fresh["request"]["observation_key"],
        "keep_host_archived":true,"policy":serde_json::from_slice::<Value>(&old_policy).unwrap()}});
    let path = f.dir.path().join("reentry-request.json");
    files::write_private(&path, &serde_json::to_vec(&request).unwrap()).unwrap();
    let activate = || {
        Command::new(&f.binary)
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["github", "archive-activate", "--socket"])
            .arg(&f.config.socket)
            .arg("--credential-file")
            .arg(owner)
            .arg("--request")
            .arg(&path)
            .output()
            .unwrap()
    };
    let before = native_rows(f).await;
    f.sql.batch_execute(r#"
        CREATE FUNCTION refuse_reentry() RETURNS trigger LANGUAGE plpgsql AS $$
        BEGIN
            IF NEW.key='reenter-native' THEN RAISE EXCEPTION 'injected re-entry receipt failure'; END IF;
            RETURN NEW;
        END $$;
        CREATE TRIGGER refuse_reentry BEFORE INSERT ON th_operations FOR EACH ROW EXECUTE FUNCTION refuse_reentry();
        "#).await.unwrap();
    assert_rejected(activate(), "internal_error");
    assert_eq!(native_rows(f).await, before);
    f.sql
        .batch_execute(
            "DROP TRIGGER refuse_reentry ON th_operations; DROP FUNCTION refuse_reentry();",
        )
        .await
        .unwrap();
    let activated = ok(activate());
    assert_eq!(activated["epoch"], 5);
    assert_eq!(activated["branches"], 4);
    assert_eq!(activated["proposals"], json!([]));
    assert_eq!(ok(activate()), activated);
    let after = native_rows(f).await;
    assert_eq!(after["authority"], "standalone");
    assert_eq!(after["changes"], before["changes"]);
    assert_eq!(
        after["history"].as_i64().unwrap(),
        before["history"].as_i64().unwrap() + 4
    );
    let audited: i64 = f
        .sql
        .query_one(
            "SELECT count(*) FROM th_native_ref_updates WHERE epoch=5 AND kind='archive'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(audited, 3);
    let snapshot = ok(native(f, owner, json!({"action":"ref_snapshot"})));
    let refs = |value: &Value| {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|item| (item["name"].as_str().unwrap().to_owned(), item.clone()))
            .collect::<BTreeMap<_, _>>()
    };
    let old = refs(&freeze["snapshot"]["refs"]);
    let current = refs(&snapshot["refs"]);
    assert_eq!(current["host-work"]["head"], current["main"]["head"]);
    for name in ["stable", "topic"] {
        assert_eq!(current[name]["head"], old[name]["head"]);
    }
    for (name, prior) in &old {
        assert_eq!(
            current[name]["generation"].as_i64().unwrap(),
            prior["generation"].as_i64().unwrap() + 1
        );
        assert_eq!(current[name]["epoch"], 5);
    }
    let hex = |value: &Value| {
        value
            .as_str()
            .unwrap()
            .strip_prefix("native:sha256:")
            .unwrap()
            .to_owned()
    };
    let parents: Vec<Vec<u8>> = f.sql.query_one(r#"
        SELECT array_agg(e.child ORDER BY e.child) FROM th_object_edges e JOIN th_objects c ON c.tenant=e.tenant AND c.digest=e.child
        WHERE e.tenant=$1 AND e.parent=decode($2,'hex') AND c.kind=5
        "#, &[&f.scope.tenant(), &hex(&current["main"]["head"])]).await.unwrap().get(0);
    let native_bytes = |name: &str| {
        let RevisionRef::Native(id) = old[name]["head"].as_str().unwrap().parse().unwrap() else {
            panic!("native ref expected")
        };
        id.as_bytes().to_vec()
    };
    let mut expected = [native_bytes("main"), native_bytes("topic")];
    expected.sort();
    assert_eq!(parents, expected);

    let output = f.dir.path().join("reentry-export");
    let projection = archive_payload::success(f.cli_with_credential(
        owner,
        &[
            "git-export",
            "--epoch",
            "5",
            "--output",
            output.to_str().unwrap(),
        ],
    ));
    for head in &archive.heads {
        let name = format!("refs/heads/{}", head.name);
        assert_eq!(projection["export"]["refs"][name]["git"], head.target.oid);
    }
    let restored = f.dir.path().join("reentry-restored.git");
    git_reference::git(
        f.dir.path(),
        &[
            "clone",
            "--mirror",
            output.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(
        git_reference::git(&restored, &["show", "main:host.txt"], b""),
        b"a change committed while GitHub owned main\n"
    );
}
