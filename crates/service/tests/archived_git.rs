#![cfg(unix)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output, Stdio},
};
use treeharbor_core::ObjectId;
use treeharbor_github::ArchivedRepository;
use treeharbor_metadata::github::ArchiveInventoryDigest;
use treeharbor_service::files;

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
    let commit = |tree: &str, parent: Option<&str>, label: &str| {
        let parent = parent.map_or_else(String::new, |p| format!("parent {p}\n"));
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
    let base = commit(&empty, None, "base");
    let main = commit(&empty, Some(&base), "new main");
    let fork = commit(&tree, Some(&base), "fork proposal");
    let tagged = commit(&empty, None, "reachable only through tag");
    let tag_bytes = format!(
        "object {tagged}\ntype commit\ntag v1\ntagger Fixture <fixture@example.invalid> 1234567890 +0000\n\nPreserve this object.\n"
    );
    let tag = put(&source, "tag", tag_bytes.as_bytes());
    for (name, object) in [
        ("refs/heads/main", &main),
        ("refs/pull/17/head", &fork),
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
    inventory["open_pulls"][0]["headRefOid"] = json!(fork);
    inventory["open_pulls"][0]["baseRefOid"] = json!(base);
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
    assert_eq!(receipt["open_pulls"], 1);
    let saved = destination.join("archive.bundle");
    assert_eq!(fs::read(&saved).unwrap(), fs::read(&complete).unwrap());
    assert_eq!(
        receipt["bundle"]["sha256"],
        ObjectId::from_bytes(Sha256::digest(fs::read(&saved).unwrap()).into()).hex()
    );
    assert_eq!(receipt["bundle"]["objects"], 8);
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
}
