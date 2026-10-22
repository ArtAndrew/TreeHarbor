use super::*;
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::{ffi::OsStrExt, fs::symlink};
fn ok(o: Output) -> Value {
    assert!(
        o.status.success(),
        "{} {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice::<Value>(&o.stdout).unwrap()["result"].clone()
}
fn hex(p: &[u8]) -> String {
    p.iter().map(|b| format!("{b:02x}")).collect()
}
fn git_commit(path: &std::path::Path) -> String {
    git_reference::git(path, &["add", "--all"], b"");
    git_reference::git(
        path,
        &[
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "-qm",
            "fixture",
        ],
        b"",
    );
    String::from_utf8(git_reference::git(path, &["rev-parse", "HEAD"], b""))
        .unwrap()
        .trim()
        .into()
}
fn capture(f: &Fixture, source: &std::path::Path, name: &str) -> PathBuf {
    let path = f.dir.path().join(name);
    let output = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .env("PATH", "/no-executables")
        .args(["source-pack", "--directory"])
        .arg(source)
        .arg("--output")
        .arg(&path)
        .args([
            "--path",
            "files",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    path
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual read-only review CLI, Git oracle, binary/empty-directory/mode/symlink cases"]
async fn review_cli_matches_git_paths_and_patches_and_retains_exact_pages_after_restart() {
    let mut f = Fixture::new().await;
    f.importer();
    let source = f.dir.path().join("review-source");
    fs::create_dir(&source).unwrap();
    git_reference::git(&source, &["init", "--template=", "-q"], b"");
    let files = source.join("files");
    fs::create_dir(&files).unwrap();
    fs::create_dir(files.join("dir")).unwrap();
    fs::create_dir(files.join("empty-old")).unwrap();
    fs::create_dir(files.join("becomes-empty")).unwrap();
    fs::write(files.join("becomes-empty/child"), b"removed\n").unwrap();
    let original = b"one\ntwo\nthree\n";
    fs::write(files.join("file.txt"), original).unwrap();
    fs::write(files.join("gone"), b"deleted\n").unwrap();
    fs::write(files.join("binary"), b"a\0b").unwrap();
    fs::write(files.join("mode"), b"same\n").unwrap();
    fs::write(files.join("dir/child"), b"child\n").unwrap();
    fs::write(files.join("keep"), vec![42; 1024 * 1024]).unwrap();
    symlink("old", files.join("link")).unwrap();
    // Linux accepts non-UTF8 names; APFS does not. Both exercise lossless
    // path encoding, including a newline that must never enter patch headers.
    let raw_name: &[u8] = if cfg!(target_os = "linux") {
        b"raw-\xff\nname"
    } else {
        "raw-π\nname".as_bytes()
    };
    let raw = files.join(std::ffi::OsStr::from_bytes(raw_name));
    fs::write(&raw, b"old\n").unwrap();
    let before_git = git_commit(&source);
    let before_pack = capture(&f, &source, "review-before");
    fs::write(files.join("file.txt"), b"one\nTWO\nthree\n").unwrap();
    fs::remove_file(files.join("gone")).unwrap();
    fs::write(files.join("binary"), b"c\0d").unwrap();
    fs::set_permissions(files.join("mode"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_dir_all(files.join("dir")).unwrap();
    fs::write(files.join("dir"), b"now file\n").unwrap();
    fs::create_dir(files.join("added")).unwrap();
    fs::write(files.join("added/sub"), b"new\n").unwrap();
    fs::write(files.join("big"), vec![b'x'; 65537]).unwrap();
    fs::write(&raw, b"new\n").unwrap();
    fs::remove_file(files.join("link")).unwrap();
    symlink("new", files.join("link")).unwrap();
    fs::remove_dir(files.join("empty-old")).unwrap();
    fs::create_dir(files.join("empty-new")).unwrap();
    fs::remove_file(files.join("becomes-empty/child")).unwrap();
    let after_git = git_commit(&source);
    let after_pack = capture(&f, &source, "review-after");
    let oracle: BTreeSet<_> = git_reference::git(
        &source,
        &[
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--name-only",
            "-z",
            &before_git,
            &after_git,
        ],
        b"",
    )
    .split(|b| *b == 0)
    .filter(|p| !p.is_empty())
    .map(hex)
    .collect();
    let mut server = f.start().await;
    let mut revisions = Vec::new();
    for (pack, name) in [(&before_pack, "before"), (&after_pack, "after")] {
        revisions.push(
            ok(f.cli(&[
                "source-import",
                "--pack",
                pack.to_str().unwrap(),
                "--state",
                f.dir
                    .path()
                    .join(format!("{name}-import.json"))
                    .to_str()
                    .unwrap(),
                "--authority",
                "standalone",
                "--epoch",
                "1",
            ]))["revision"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    // Use a new read-only credential, not the repository importer.
    let path = f.dir.path().join("read-only.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--principal",
        "importer",
        "--permission",
        "read",
        "--output",
        path.to_str().unwrap(),
    ]);
    f.credential_path = path;
    let query = vec![
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--limit",
        "100",
        "revisions",
        "--before",
        &revisions[0],
        "--after",
        &revisions[1],
    ];
    let diff = ok(f.cli(&query));
    assert_eq!(diff["next_after"], Value::Null);
    let rows: BTreeMap<_, _> = diff["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["path_hex"].as_str().unwrap().to_owned(), r.clone()))
        .collect();
    let leaves: BTreeSet<_> = rows
        .iter()
        .filter(|(_, r)| {
            ["before", "after"]
                .into_iter()
                .any(|side| r[side]["mode"].as_u64().is_some_and(|mode| mode != 16384))
        })
        .map(|(p, _)| p.clone())
        .collect();
    assert_eq!(leaves, oracle);
    assert_eq!(rows[&hex(b"files/binary")]["text"]["reason"], "binary");
    assert_eq!(rows[&hex(b"files/big")]["text"]["reason"], "size_limit");
    assert_eq!(rows[&hex(b"files/mode")]["kind"], "mode");
    assert_eq!(rows[&hex(b"files/mode")]["text"]["status"], "unchanged");
    assert_eq!(
        rows[&hex(b"files/empty-new")]["text"]["reason"],
        "directory"
    );
    assert_eq!(rows[&hex(b"files/dir")]["kind"], "type");
    assert!(rows.contains_key(&hex(b"files/dir/child")));
    assert!(rows.contains_key(&hex(b"files/becomes-empty/child")));
    let patch = rows[&hex(b"files/file.txt")]["text"]["patch"]
        .as_str()
        .unwrap();
    assert_eq!(
        diffy::apply(
            std::str::from_utf8(original).unwrap(),
            &diffy::Patch::from_str(patch).unwrap()
        )
        .unwrap(),
        "one\nTWO\nthree\n"
    );
    // Restart, remove original inputs, then reconstruct every exact page.
    server.0.kill().unwrap();
    server.0.wait().unwrap();
    drop(server);
    fs::remove_dir_all(&source).unwrap();
    fs::remove_dir_all(&before_pack).unwrap();
    fs::remove_dir_all(&after_pack).unwrap();
    server = f.start().await;
    let mut after = None;
    let mut pages = Vec::new();
    loop {
        let mut args = vec![
            "diff",
            "--authority",
            "standalone",
            "--epoch",
            "1",
            "--limit",
            "2",
        ];
        if let Some(cursor) = after.as_deref() {
            args.extend(["--after", cursor]);
        }
        args.extend([
            "revisions",
            "--before",
            &revisions[0],
            "--after",
            &revisions[1],
        ]);
        let page = ok(f.cli(&args));
        assert_eq!(page["before_snapshot"], diff["before_snapshot"]);
        assert_eq!(page["after_snapshot"], diff["after_snapshot"]);
        pages.extend(page["changes"].as_array().unwrap().iter().cloned());
        after = page["next_after"].as_str().map(str::to_owned);
        if after.is_none() {
            break;
        }
    }
    assert_eq!(pages, *diff["changes"].as_array().unwrap());
    let mut bad = query.clone();
    bad[4] = "2";
    assert!(!f.cli(&bad).status.success());
    let metadata = ok(f.cli(&[
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "--metadata-only",
        "--limit",
        "100",
        "revisions",
        "--before",
        &revisions[0],
        "--after",
        &revisions[1],
    ]));
    assert!(
        metadata["changes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["text"]["reason"] == "not_requested")
    );
    let same = ok(f.cli(&[
        "diff",
        "--authority",
        "standalone",
        "--epoch",
        "1",
        "revisions",
        "--before",
        &revisions[0],
        "--after",
        &revisions[0],
    ]));
    assert_eq!(same["total_changes"], 0);
    drop(server);
    f.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires PostgreSQL; actual oversized escaped response and complete paginated fallback"]
async fn review_response_limit_never_returns_silently_incomplete_results() {
    let mut f = Fixture::new().await;
    f.importer();
    let source = f.dir.path().join("large-review");
    let files = source.join("files");
    fs::create_dir_all(&files).unwrap();
    for i in 0..3 {
        fs::write(files.join(i.to_string()), vec![3; 60_000]).unwrap();
    }
    let before = capture(&f, &source, "large-before");
    for i in 0..3 {
        fs::write(files.join(i.to_string()), vec![4; 60_000]).unwrap();
    }
    let after = capture(&f, &source, "large-after");
    let server = f.start().await;
    let mut revisions = Vec::new();
    for (pack, name) in [(&before, "before"), (&after, "after")] {
        revisions.push(
            ok(f.cli(&[
                "source-import",
                "--pack",
                pack.to_str().unwrap(),
                "--state",
                f.dir.path().join(format!("{name}.json")).to_str().unwrap(),
                "--authority",
                "standalone",
                "--epoch",
                "1",
            ]))["revision"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    let base = ["diff", "--authority", "standalone", "--epoch", "1"];
    let initial_holds: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    let target = [
        "revisions",
        "--before",
        revisions[0].as_str(),
        "--after",
        revisions[1].as_str(),
    ];
    let mut full = base.to_vec();
    full.extend(target);
    let rejected = f.cli(&full);
    assert!(!rejected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&rejected.stdout).unwrap()["error"]["code"],
        "review_limit"
    );
    // All paths remain available without patch text, and smaller pages retain
    // complete text. Escaped JSON size, not raw blob size, controls the limit.
    let mut metadata = base.to_vec();
    metadata.push("--metadata-only");
    metadata.extend(target);
    let all = ok(f.cli(&metadata));
    assert_eq!(all["total_changes"], 3);
    assert_eq!(all["changes"].as_array().unwrap().len(), 3);
    let mut cursor = None;
    for row in all["changes"].as_array().unwrap() {
        let mut args = base.to_vec();
        args.extend(["--limit", "1"]);
        if let Some(after) = cursor.as_deref() {
            args.extend(["--after", after]);
        }
        args.extend(target);
        let page = ok(f.cli(&args));
        assert_eq!(page["total_changes"], 3);
        assert_eq!(page["changes"][0]["path_hex"], row["path_hex"]);
        let patch =
            diffy::Patch::from_str(page["changes"][0]["text"]["patch"].as_str().unwrap()).unwrap();
        assert_eq!(
            diffy::apply(&"\u{3}".repeat(60_000), &patch).unwrap(),
            "\u{4}".repeat(60_000)
        );
        cursor = page["next_after"].as_str().map(str::to_owned);
    }
    assert!(cursor.is_none());
    let mut invalid = base.to_vec();
    invalid.extend(["--after", "6e6f6e6d656d626572"]);
    invalid.extend(target);
    let rejected = f.cli(&invalid);
    assert!(!rejected.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&rejected.stdout).unwrap()["error"]["code"],
        "review_cursor"
    );
    let holds: i64 = f
        .sql
        .query_one("SELECT count(*) FROM th_object_holds", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(holds, initial_holds);
    drop(server);
    f.cleanup().await;
}
