#![cfg(unix)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
};
use treeharbor_core::{Object, ObjectId, ObjectStore, prepare_change};
use treeharbor_github::ArchivedRepository;
use treeharbor_metadata::github::ArchiveInventoryDigest;
use treeharbor_service::files;
use treeharbor_storage::{LocalStore, TenantId};

fn git(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("/usr/bin/git")
        .current_dir(dir)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("LC_ALL", "C")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn oid(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim_end().to_owned()
}

fn put(dir: &Path, kind: &str, bytes: &[u8]) -> String {
    oid(git(
        dir,
        &["hash-object", "-t", kind, "-w", "--stdin"],
        bytes,
    ))
}

fn capture_state(state: &Path, mut record: Value, inventory: Value) {
    fs::create_dir(state).unwrap();
    fs::set_permissions(state, fs::Permissions::from_mode(0o700)).unwrap();
    let full: ArchivedRepository = serde_json::from_value(inventory.clone()).unwrap();
    let obs = &mut record["observation"];
    obs["heads"]["refs/heads/main"] =
        json!(format!("git:sha1:{}", full.archive.heads[0].target.oid));
    obs["tags"] = json!(ArchiveInventoryDigest::tags(&full.tags).unwrap());
    obs["open_pulls"] = json!(ArchiveInventoryDigest::open_pulls(&full.open_pulls).unwrap());
    let request = &record["request"];
    let intent = json!({
        "version":1,"kind":"archive_observation","tenant":request["tenant"],
        "repository":request["repository"],"epoch":request["epoch"],"request_key":request["key"],
    });
    let capture = json!({"binding":request["binding"],"inventory":inventory});
    for (name, value) in [
        ("intent.json", intent),
        ("inventory.json", capture),
        ("receipt.json", record),
    ] {
        files::write_private(&state.join(name), &serde_json::to_vec(&value).unwrap()).unwrap();
    }
}

fn preserve(state: &Path, input: &Path, destination: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env_clear()
        .args(["github", "archive-preserve", "--state"])
        .arg(state)
        .arg("--bundle")
        .arg(input)
        .arg("--output")
        .arg(destination)
        .output()
        .unwrap()
}

fn proposals(archive: &Path, fingerprint: &str, output: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env_clear()
        .args(["github", "archive-proposals", "--archive"])
        .arg(archive)
        .args(["--archive-fingerprint", fingerprint, "--output"])
        .arg(output)
        .output()
        .unwrap()
}

fn check_prepared_proposals(archive: &Path, fingerprint: &str, restored: &Path) {
    let parent = archive.parent().unwrap();
    let output = parent.join("prepared-proposals");
    let command = proposals(archive, fingerprint, &output);
    assert!(
        command.status.success(),
        "{}",
        String::from_utf8_lossy(&command.stderr)
    );
    let receipt: Value = serde_json::from_slice(&command.stdout).unwrap();
    assert_eq!(receipt["state"], "prepared");
    assert_eq!(receipt["ready"], 1);
    assert_eq!(receipt["needs_resolution"], 2);
    let raw = fs::read(output.join("manifest.json")).unwrap();
    let manifest: Value = serde_json::from_slice(&raw).unwrap();
    assert_eq!(manifest["archive_manifest_sha256"], fingerprint);
    let mut digest = Sha256::new();
    digest.update(b"treeharbor-archive-proposals\0v1\0");
    digest.update(&raw);
    assert_eq!(
        receipt["manifest_sha256"],
        ObjectId::from_bytes(digest.finalize().into()).to_string()
    );
    let original: Value =
        serde_json::from_slice(&fs::read(archive.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(
        manifest["archive_bundle_sha256"],
        original["bundle"]["sha256"]
    );
    assert_eq!(manifest["observation"], original["receipt"]["request"]);
    let prepared = manifest["proposals"].as_array().unwrap();
    assert_eq!(prepared.len(), 3);
    for (proposal, source) in prepared
        .iter()
        .zip(original["inventory"]["open_pulls"].as_array().unwrap())
    {
        assert_eq!(&proposal["source"], source);
        assert_eq!(
            proposal["head"]["git_revision"],
            format!("git:sha1:{}", source["headRefOid"].as_str().unwrap())
        );
        assert_eq!(
            proposal["base_tip"]["git_revision"],
            format!("git:sha1:{}", source["baseRefOid"].as_str().unwrap())
        );
    }
    let ready = &prepared[0];
    assert_eq!(ready["target_present"], true);
    assert_eq!(
        ready["discussion"],
        "https://github.com/example-org/demo/pull/17"
    );
    assert_eq!(ready["patch"]["state"], "ready");
    let base = oid(git(
        restored,
        &["merge-base", "refs/heads/main", "refs/pull/17/head"],
        b"",
    ));
    assert_eq!(
        ready["patch"]["merge_base"]["git_revision"],
        format!("git:sha1:{base}")
    );
    assert_ne!(ready["base_tip"], ready["patch"]["merge_base"]);
    assert_eq!(
        git(
            restored,
            &["diff", "--name-only", &base, "refs/pull/17/head", "--"],
            b""
        ),
        b"payload.bin\n"
    );
    assert_eq!(
        git(
            restored,
            &[
                "diff",
                "--name-only",
                "refs/heads/main",
                "refs/pull/17/head",
                "--"
            ],
            b""
        ),
        b"main-only.txt\npayload.bin\n"
    );
    let store = LocalStore::open(&output, &TenantId::new("archive-proposals").unwrap()).unwrap();
    let mut seen = BTreeSet::new();
    let mut encoded_bytes = 0usize;
    for entry in manifest["objects"].as_array().unwrap() {
        let id: ObjectId = entry.as_str().unwrap().parse().unwrap();
        let object = store.get(id).unwrap();
        assert!(
            object
                .references()
                .iter()
                .all(|(child, _)| seen.contains(child))
        );
        assert!(seen.insert(id));
        encoded_bytes += object.encode().unwrap().len();
    }
    assert_eq!(receipt["object_count"], seen.len());
    assert_eq!(manifest["encoded_bytes"], encoded_bytes);
    let operations = ready["patch"]["operations"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(seen.contains(&operations));
    let Object::ChangeOperations(operations) = store.get(operations).unwrap() else {
        panic!("expected native operations");
    };
    assert_eq!(operations.operations().len(), 1);
    assert_eq!(operations.operations()[0].path.as_bytes(), b"payload.bin");
    let base_snapshot = ready["patch"]["merge_base"]["snapshot"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(
        prepare_change(&store, base_snapshot, &operations)
            .unwrap()
            .result_snapshot()
            .to_string(),
        ready["head"]["snapshot"].as_str().unwrap()
    );
    let ambiguous = &prepared[1]["patch"];
    assert_eq!(ambiguous["state"], "needs_resolution");
    assert_eq!(ambiguous["reason"], "multiple_merge_bases");
    let bases = String::from_utf8(git(
        restored,
        &[
            "merge-base",
            "--all",
            "refs/heads/main",
            "refs/pull/18/head",
        ],
        b"",
    ))
    .unwrap();
    let expected: BTreeSet<_> = bases.lines().map(|oid| format!("git:sha1:{oid}")).collect();
    let actual: BTreeSet<_> = ambiguous["merge_bases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tip| tip["git_revision"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(expected.len(), 2);
    assert_eq!(actual, expected);
    assert_eq!(prepared[2]["target_present"], false);
    assert_eq!(
        prepared[2]["patch"],
        json!({"state":"needs_resolution","reason":"unrelated_history","merge_bases":[]})
    );
    assert!(!proposals(archive, fingerprint, &output).status.success());
    assert_eq!(fs::read(output.join("manifest.json")).unwrap(), raw);
    let wrong = parent.join("wrong-archive-fingerprint");
    assert!(
        !proposals(archive, &format!("sha256:{}", "ab".repeat(32)), &wrong)
            .status
            .success()
    );
    assert!(!wrong.exists());
}

#[test]
fn archive_preserve_restores_exact_tags_and_fork_head_after_source_loss() {
    let root = tempfile::tempdir().unwrap();
    let root_dir = root.path();
    fs::set_permissions(root_dir, fs::Permissions::from_mode(0o700)).unwrap();
    let source = root_dir.join("source.git");
    fs::create_dir(&source).unwrap();
    git(
        &source,
        &["init", "--bare", "--template=", "--object-format=sha1"],
        b"",
    );
    git(&source, &["symbolic-ref", "HEAD", "refs/heads/main"], b"");
    let empty = put(&source, "tree", b"");
    let bytes = b"fork change\0\xff\n";
    let blob = put(&source, "blob", bytes);
    let tree = oid(git(
        &source,
        &["mktree"],
        format!("100644 blob {blob}\tpayload.bin\n").as_bytes(),
    ));
    let commit = |tree: &str, parents: &[&str], label: &str| {
        let parent: String = parents.iter().map(|p| format!("parent {p}\n")).collect();
        put(
            &source,
            "commit",
            format!(
                "tree {tree}\n{parent}author Fixture <fixture@example.invalid> 1234567890 +0000\n\
                 committer Fixture <fixture@example.invalid> 1234567890 +0000\n\n{label}\n"
            )
            .as_bytes(),
        )
    };
    let main_blob = put(&source, "blob", b"only on the current main\n");
    let main_tree = oid(git(
        &source,
        &["mktree"],
        format!("100644 blob {main_blob}\tmain-only.txt\n").as_bytes(),
    ));
    let base = commit(&empty, &[], "base");
    let left = commit(&main_tree, &[&base], "left");
    let right = commit(&empty, &[&base], "right");
    let main = commit(&main_tree, &[&left, &right], "new main");
    let fork = commit(&tree, &[&base], "fork proposal");
    let ambiguous = commit(&tree, &[&right, &left], "criss-cross");
    let unrelated = commit(&empty, &[], "unrelated proposal");
    let tagged = commit(&empty, &[], "reachable only through tag");
    let tag_bytes = format!(
        "object {tagged}\ntype commit\ntag v1\ntagger Fixture <fixture@example.invalid> 1234567890 +0000\n\nPreserve this object.\n"
    );
    let tag = put(&source, "tag", tag_bytes.as_bytes());
    for (name, object) in [
        ("refs/heads/main", &main),
        ("refs/pull/17/head", &fork),
        ("refs/pull/18/head", &ambiguous),
        ("refs/pull/19/head", &unrelated),
        ("refs/tags/v1", &tag),
        ("refs/tags/source-tree", &tree),
        ("refs/tags/source-blob", &blob),
    ] {
        git(&source, &["update-ref", name, object], b"");
    }
    let refs = [
        "for-each-ref",
        "--format=%(refname) %(objecttype) %(objectname)",
    ];
    let expected = git(&source, &refs, b"");
    let complete = root_dir.join("complete.bundle");
    git(
        &source,
        &["bundle", "create", complete.to_str().unwrap(), "--all"],
        b"",
    );

    let vector: Value =
        serde_json::from_str(include_str!("fixtures/github-archive-preservation-v1.json")).unwrap();
    let mut inventory = vector["inventory"].clone();
    inventory["archive"]["heads"][0]["target"]["oid"] = json!(main);
    inventory["tags"] = json!([
        {"id":"tag-blob","name":"source-blob","prefix":"refs/tags/","target":{"__typename":"Blob","oid":blob}},
        {"id":"tag-tree","name":"source-tree","prefix":"refs/tags/","target":{"__typename":"Tree","oid":tree}},
        {"id":"tag-v1","name":"v1","prefix":"refs/tags/","target":{"__typename":"Tag","oid":tag}},
    ]);
    let mut ready = inventory["open_pulls"][0].clone();
    ready["headRefOid"] = json!(fork);
    ready["baseRefOid"] = json!(main);
    let mut multi_base = ready.clone();
    multi_base["id"] = json!("pull-18");
    multi_base["number"] = json!(18);
    multi_base["headRefOid"] = json!(ambiguous);
    multi_base["isDraft"] = json!(true);
    let mut no_base = ready.clone();
    no_base["id"] = json!("pull-19");
    no_base["number"] = json!(19);
    no_base["headRefOid"] = json!(unrelated);
    no_base["baseRefName"] = json!("retired-topic");
    inventory["open_pulls"] = json!([ready, multi_base, no_base]);
    let state = root_dir.join("observed");
    capture_state(&state, vector["receipt"].clone(), inventory.clone());
    let destination = root_dir.join("saved");
    let output = preserve(&state, &complete, &destination);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(receipt["state"], "preserved");
    assert_eq!(receipt["branches"], 1);
    assert_eq!(receipt["tags"], 3);
    assert_eq!(receipt["open_pulls"], 3);
    let saved = destination.join("archive.bundle");
    assert_eq!(fs::read(&saved).unwrap(), fs::read(&complete).unwrap());
    assert_eq!(
        receipt["bundle"]["sha256"],
        ObjectId::from_bytes(Sha256::digest(fs::read(&saved).unwrap()).into()).hex()
    );
    assert_eq!(receipt["bundle"]["objects"], 14);
    let manifest = fs::read(destination.join("manifest.json")).unwrap();
    let recorded: Value = serde_json::from_slice(&manifest).unwrap();
    assert_eq!(recorded["inventory"], inventory);
    assert_eq!(recorded["bundle"], receipt["bundle"]);
    let mut hash = Sha256::new();
    hash.update(b"treeharbor-github-archive\0v1\0");
    hash.update(&manifest);
    assert_eq!(
        receipt["manifest_sha256"],
        ObjectId::from_bytes(hash.finalize().into()).to_string()
    );
    assert!(!preserve(&state, &complete, &destination).status.success());
    assert_eq!(
        fs::read(destination.join("manifest.json")).unwrap(),
        manifest
    );

    let rejected = root_dir.join("rejected");
    let incomplete = root_dir.join("no-pulls.bundle");
    git(
        &source,
        &[
            "bundle",
            "create",
            incomplete.to_str().unwrap(),
            "--branches",
            "--tags",
        ],
        b"",
    );
    assert!(!preserve(&state, &incomplete, &rejected).status.success());
    assert!(!rejected.exists());

    let extra = put(&source, "blob", b"outside the observed ref closures");
    let oids = git(
        &source,
        &[
            "cat-file",
            "--batch-check=%(objectname)",
            "--batch-all-objects",
        ],
        b"",
    );
    assert!(
        String::from_utf8_lossy(&oids)
            .lines()
            .any(|oid| oid == extra)
    );
    let pack = git(&source, &["pack-objects", "--stdout", "--quiet"], &oids);
    let list = git(
        &source,
        &["bundle", "list-heads", complete.to_str().unwrap()],
        b"",
    );
    let with_extra_object = root_dir.join("unreachable-object.bundle");
    let with_filter = root_dir.join("filtered.bundle");
    for (input, header) in [
        (with_extra_object, b"# v2 git bundle\n".as_slice()),
        (
            with_filter,
            b"# v3 git bundle\n@object-format=sha1\n@filter=blob:none\n".as_slice(),
        ),
    ] {
        fs::write(&input, [header, &list, b"\n", &pack].concat()).unwrap();
        let failed = preserve(&state, &input, &rejected);
        assert!(!failed.status.success(), "{input:?}");
        assert!(!rejected.exists());
    }
    let mut wrong_kind = inventory.clone();
    wrong_kind["tags"][2]["target"]["__typename"] = json!("Commit");
    let wrong_state = root_dir.join("wrong-tag-kind");
    capture_state(&wrong_state, vector["receipt"].clone(), wrong_kind);
    assert!(
        !preserve(&wrong_state, &complete, &rejected)
            .status
            .success()
    );
    assert!(!rejected.exists());
    let mut wrong_base = inventory;
    wrong_base["open_pulls"][0]["baseRefOid"] = json!("42".repeat(20));
    let wrong_state = root_dir.join("wrong-pull-base");
    capture_state(&wrong_state, vector["receipt"].clone(), wrong_base);
    assert!(
        !preserve(&wrong_state, &complete, &rejected)
            .status
            .success()
    );
    assert!(!rejected.exists());
    let data = fs::read(&complete).unwrap();
    let truncated = root_dir.join("truncated.bundle");
    fs::write(&truncated, &data[..data.len() - 20]).unwrap();
    assert!(!preserve(&state, &truncated, &rejected).status.success());
    assert!(!rejected.exists());

    // Read the retained file in a new, empty ordinary Git repository.
    fs::remove_dir_all(source).unwrap();
    fs::remove_file(complete).unwrap();
    fs::remove_dir_all(state).unwrap();
    let restored = root_dir.join("restored.git");
    fs::create_dir(&restored).unwrap();
    git(
        &restored,
        &["init", "--bare", "--template=", "--object-format=sha1"],
        b"",
    );
    git(&restored, &["symbolic-ref", "HEAD", "refs/heads/main"], b"");
    git(
        &restored,
        &["bundle", "verify", saved.to_str().unwrap()],
        b"",
    );
    git(
        &restored,
        &[
            "fetch",
            "--no-tags",
            saved.to_str().unwrap(),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
            "refs/pull/*:refs/pull/*",
        ],
        b"",
    );
    git(&restored, &["fsck", "--strict"], b"");
    assert_eq!(git(&restored, &refs, b""), expected);
    assert_eq!(
        git(&restored, &["cat-file", "tag", &tag], b""),
        tag_bytes.as_bytes()
    );
    assert_eq!(
        git(
            &restored,
            &["cat-file", "blob", "refs/pull/17/head:payload.bin"],
            b""
        ),
        bytes
    );
    assert_eq!(
        oid(git(&restored, &["merge-base", &main, &fork], b"")),
        base
    );
    assert_eq!(
        oid(git(&restored, &["rev-parse", "refs/tags/v1^{}"], b"")),
        tagged
    );
    check_prepared_proposals(
        &destination,
        receipt["manifest_sha256"].as_str().unwrap(),
        &restored,
    );
}
