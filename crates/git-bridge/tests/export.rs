use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use treeharbor_core::*;
use treeharbor_git_bridge::{Export, HashAlgorithm, Limits, Manifest, export};
use treeharbor_storage::{LocalStore, TenantId};

fn store(path: &Path) -> LocalStore {
    LocalStore::open(path, &TenantId::new("test").unwrap()).unwrap()
}
fn tree(s: &impl ObjectStore, entries: &[(&[u8], EntryMode, &[u8])]) -> ObjectId {
    let entries = entries
        .iter()
        .map(|(name, mode, bytes)| Entry {
            name: Name::new(name.to_vec()).unwrap(),
            value: EntryRef {
                mode: *mode,
                target: s.put(&Object::Blob(bytes.to_vec())).unwrap(),
            },
        })
        .collect();
    let root = s
        .put(&Object::Directory(Directory::new(entries).unwrap()))
        .unwrap();
    s.put(&Object::Snapshot { root }).unwrap()
}
fn commit(
    s: &impl ObjectStore,
    snapshot: ObjectId,
    parents: Vec<ObjectId>,
    message: &str,
) -> ObjectId {
    let changes = s
        .put(&Object::ChangeOperations(Changes::new(vec![]).unwrap()))
        .unwrap();
    s.put(&Object::NativeCommit(Commit {
        snapshot,
        changes,
        parents,
        author_name: "Andrey".into(),
        author_email: "a@example.invalid".into(),
        timestamp_seconds: 1234567890,
        timezone_minutes: -330,
        message: message.into(),
    }))
    .unwrap()
}
fn run(dir: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
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
fn git_path() -> &'static Path {
    Path::new("/usr/bin/git")
}
fn request<'a>(output: &'a Path, head: ObjectId, algorithm: HashAlgorithm) -> Export<'a> {
    Export {
        git: git_path(),
        output,
        refs: BTreeMap::from([("refs/heads/main".into(), head)]),
        algorithm,
        limits: Limits::default(),
    }
}
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap().trim().into()
}

#[test]
fn complete_dag_restores_with_bytes_modes_parent_order_and_both_hash_algorithms() {
    for algorithm in [HashAlgorithm::Sha1, HashAlgorithm::Sha256] {
        let temp = tempfile::tempdir().unwrap();
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let s = store(&temp.path().join("store"));
        let binary = b"\0\ncommit refs/heads/evil\n\xff";
        let snap = tree(
            &s,
            &[
                (b"f\xff\n\" name", EntryMode::File, binary),
                (b"executable", EntryMode::Executable, b"#!/bin/sh\nexit 1\n"),
                (b"link", EntryMode::Symlink, b"../../never-open-this"),
                (b".gitattributes", EntryMode::File, b"* filter=evil\n"),
            ],
        );
        let root = commit(&s, snap, vec![], "root\n");
        let empty = tree(&s, &[]);
        let unrelated = commit(&s, empty, vec![], "unrelated root");
        let left = commit(&s, empty, vec![root], "left");
        let right = commit(&s, snap, vec![root], "right");
        let merge = commit(
            &s,
            snap,
            vec![right, left, unrelated],
            "merge\n\nno final newline",
        );
        let output = temp.path().join("export");
        let mut r = request(&output, merge, algorithm);
        r.refs.insert("refs/heads/other".into(), left);
        let manifest = export(&s, r).unwrap();
        assert_eq!(manifest.commits.len(), 5);
        assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
        let disk: Manifest =
            serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest, disk);
        let restored = temp.path().join("restored.git");
        run(
            temp.path(),
            &[
                "clone",
                "--bare",
                output.join("history.bundle").to_str().unwrap(),
                restored.to_str().unwrap(),
            ],
            b"",
        );
        run(&restored, &["fsck", "--strict", "--no-reflogs"], b"");
        let oid = |id: ObjectId| manifest.commits[&id.to_string()].as_str();
        assert_eq!(
            text(run(&restored, &["rev-list", "--count", "--all"], b"")),
            "5"
        );
        assert_eq!(
            text(run(
                &restored,
                &["show", "-s", "--format=%P", oid(merge)],
                b""
            )),
            format!("{} {} {}", oid(right), oid(left), oid(unrelated))
        );
        assert_eq!(
            text(run(
                &restored,
                &["show", "-s", "--format=%P", oid(unrelated)],
                b""
            )),
            ""
        );
        assert_eq!(
            text(run(
                &restored,
                &["show", "-s", "--format=%P", oid(root)],
                b""
            )),
            ""
        );
        let raw = run(&restored, &["cat-file", "commit", oid(merge)], b"");
        assert!(raw.ends_with(b"\n\nmerge\n\nno final newline"));
        assert!(
            raw.windows(b"author Andrey <a@example.invalid> 1234567890 -0530".len())
                .any(|w| w == b"author Andrey <a@example.invalid> 1234567890 -0530")
        );
        let listing = run(&restored, &["ls-tree", "-rz", oid(merge)], b"");
        let mut found = BTreeMap::new();
        for entry in listing.split(|b| *b == 0).filter(|b| !b.is_empty()) {
            let (header, path) = entry.split_at(entry.iter().position(|b| *b == b'\t').unwrap());
            let header = std::str::from_utf8(header).unwrap();
            let fields: Vec<_> = header.split(' ').collect();
            found.insert(
                path[1..].to_vec(),
                (
                    fields[0].to_string(),
                    run(&restored, &["cat-file", "blob", fields[2]], b""),
                ),
            );
        }
        assert_eq!(
            found[b"f\xff\n\" name".as_slice()],
            ("100644".into(), binary.to_vec())
        );
        assert_eq!(found[b"executable".as_slice()].0, "100755");
        assert_eq!(
            found[b"link".as_slice()],
            ("120000".into(), b"../../never-open-this".to_vec())
        );
        // Independent Git plumbing reconstructs the tree and commit from literal
        // inputs, rather than reusing the exporter stream as its own oracle.
        let entries: Vec<_> = listing
            .split(|b| *b == 0)
            .filter(|b| !b.is_empty())
            .collect();
        let mut input = Vec::new();
        for entry in entries {
            input.extend_from_slice(entry);
            input.push(0);
        }
        let tree_oid = text(run(&restored, &["mktree", "-z"], &input));
        let expected = format!(
            "tree {tree_oid}\nparent {}\nparent {}\nparent {}\nauthor Andrey <a@example.invalid> 1234567890 -0530\ncommitter Andrey <a@example.invalid> 1234567890 -0530\n\nmerge\n\nno final newline",
            oid(right),
            oid(left),
            oid(unrelated)
        );
        assert_eq!(
            text(run(
                &restored,
                &["hash-object", "-t", "commit", "--stdin"],
                expected.as_bytes()
            )),
            oid(merge)
        );
        let second = temp.path().join("second");
        let m2 = export(&s, request(&second, merge, algorithm)).unwrap();
        assert_eq!(m2.commits, manifest.commits);
    }
}

#[test]
fn git_reference_golden_empty_root_commit() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let head = commit(&s, tree(&s, &[]), vec![], "golden");
    let output = temp.path().join("export");
    let m = export(&s, request(&output, head, HashAlgorithm::Sha1)).unwrap();
    // SHA-1 of a Git commit frame, independently generated by Python hashlib.
    assert_eq!(
        m.commits[&head.to_string()],
        "9ddc57252779b944f82af2992c8e9c9458922c1a"
    );
}

#[test]
fn rejects_partial_graph_limits_invalid_refs_and_output_replacement() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let snap = tree(&s, &[(b"file", EntryMode::File, b"bytes")]);
    let head = commit(&s, snap, vec![], "root");
    let output = temp.path().join("export");
    for limits in [
        Limits {
            objects: 1,
            ..Limits::default()
        },
        Limits {
            source_bytes: 1,
            ..Limits::default()
        },
        Limits {
            stream_bytes: 1,
            ..Limits::default()
        },
        Limits {
            path_visits: 1,
            ..Limits::default()
        },
        Limits {
            edges: 1,
            ..Limits::default()
        },
    ] {
        let mut r = request(&output, head, HashAlgorithm::Sha1);
        r.limits = limits;
        assert!(export(&s, r).is_err());
        assert!(!output.exists());
    }
    for name in [
        "--all",
        "refs/heads/a\nfeature force",
        "refs/heads/a..b",
        "refs/heads/.bad",
        "refs/heads/a.lock",
        "refs/heads/x@{y",
    ] {
        let mut r = request(&output, head, HashAlgorithm::Sha1);
        r.refs = BTreeMap::from([(name.into(), head)]);
        assert!(export(&s, r).is_err());
        assert!(!output.exists());
    }
    let broken = commit(
        &s,
        snap,
        vec![ObjectId::from_bytes([7; 32])],
        "missing parent",
    );
    assert!(export(&s, request(&output, broken, HashAlgorithm::Sha1)).is_err());
    assert!(!output.exists());
    export(&s, request(&output, head, HashAlgorithm::Sha1)).unwrap();
    let before = fs::read(output.join("history.bundle")).unwrap();
    assert!(export(&s, request(&output, head, HashAlgorithm::Sha1)).is_err());
    assert_eq!(before, fs::read(output.join("history.bundle")).unwrap());
    let link = temp.path().join("link");
    std::os::unix::fs::symlink(&output, &link).unwrap();
    assert!(export(&s, request(&link, head, HashAlgorithm::Sha1)).is_err());
}

#[test]
fn rejects_nested_empty_directories_and_unrepresentable_metadata() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let empty = s
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap();
    let root = s
        .put(&Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"empty".to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Directory,
                    target: empty,
                },
            }])
            .unwrap(),
        ))
        .unwrap();
    let snap = s.put(&Object::Snapshot { root }).unwrap();
    let head = commit(&s, snap, vec![], "root");
    let output = temp.path().join("export");
    assert!(export(&s, request(&output, head, HashAlgorithm::Sha1)).is_err());
    let valid = commit(&s, tree(&s, &[]), vec![], "valid");
    let Object::NativeCommit(c) = s.get(valid).unwrap() else {
        panic!()
    };
    for bad in [
        Commit {
            message: "x\0y".into(),
            ..c.clone()
        },
        Commit {
            timestamp_seconds: -1,
            ..c.clone()
        },
        Commit {
            author_name: " leading".into(),
            ..c.clone()
        },
        Commit {
            author_email: "x y".into(),
            ..c
        },
    ] {
        let id = s.put(&Object::NativeCommit(bad)).unwrap();
        assert!(export(&s, request(&output, id, HashAlgorithm::Sha1)).is_err());
        assert!(!output.exists());
    }
}

#[test]
fn deadline_kills_child_group_and_never_publishes_partial_output() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let head = commit(&s, tree(&s, &[]), vec![], "root");
    let fake = temp.path().join("fake-git");
    let sentinel = temp.path().join("escaped");
    // The trusted-executable test double has a descendant: killing only its
    // parent would let this write happen after export returned.
    fs::write(
        &fake,
        format!(
            "#!/bin/sh\n(sleep 1; touch '{}') &\nwait\n",
            sentinel.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o700)).unwrap();
    let output = temp.path().join("export");
    let mut r = request(&output, head, HashAlgorithm::Sha1);
    r.git = &fake;
    r.limits.timeout = Duration::from_millis(150);
    let start = Instant::now();
    assert!(matches!(
        export(&s, r),
        Err(treeharbor_git_bridge::Error::Limit)
    ));
    assert!(start.elapsed() < Duration::from_secs(3));
    assert!(!output.exists());
    std::thread::sleep(Duration::from_millis(1200));
    assert!(!sentinel.exists());
}

#[test]
fn concurrent_exports_publish_one_complete_result_without_overwriting() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let head = commit(&s, tree(&s, &[]), vec![], "root");
    let output = temp.path().join("export");
    let barrier = std::sync::Barrier::new(2);
    let results = std::thread::scope(|scope| {
        let task = || {
            barrier.wait();
            export(&s, request(&output, head, HashAlgorithm::Sha1))
        };
        let a = scope.spawn(task);
        let b = scope.spawn(task);
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let manifest: Manifest =
        serde_json::from_slice(&fs::read(output.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest, results.into_iter().find_map(|r| r.ok()).unwrap());
    assert_eq!(fs::read_dir(&output).unwrap().count(), 2);
}

#[test]
fn shared_subtrees_remain_exact_and_expansion_budget_counts_each_path() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let blob = s.put(&Object::Blob(b"nested bytes\0".to_vec())).unwrap();
    let child = s
        .put(&Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"file".to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Executable,
                    target: blob,
                },
            }])
            .unwrap(),
        ))
        .unwrap();
    let root = s
        .put(&Object::Directory(
            Directory::new(vec![
                Entry {
                    name: Name::new(b"a".to_vec()).unwrap(),
                    value: EntryRef {
                        mode: EntryMode::Directory,
                        target: child,
                    },
                },
                Entry {
                    name: Name::new(b"a-1".to_vec()).unwrap(),
                    value: EntryRef {
                        mode: EntryMode::File,
                        target: blob,
                    },
                },
                Entry {
                    name: Name::new(b"b".to_vec()).unwrap(),
                    value: EntryRef {
                        mode: EntryMode::Directory,
                        target: child,
                    },
                },
            ])
            .unwrap(),
        ))
        .unwrap();
    let snapshot = s.put(&Object::Snapshot { root }).unwrap();
    let head = commit(&s, snapshot, vec![], "shared");
    let output = temp.path().join("export");
    let manifest = export(&s, request(&output, head, HashAlgorithm::Sha1)).unwrap();
    let restored = temp.path().join("restored");
    run(
        temp.path(),
        &[
            "clone",
            "--bare",
            output.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(
        run(&restored, &["show", "main:a/file"], b""),
        b"nested bytes\0"
    );
    assert_eq!(
        run(&restored, &["show", "main:b/file"], b""),
        b"nested bytes\0"
    );
    assert_eq!(
        text(run(&restored, &["rev-parse", "main:a"], b"")),
        text(run(&restored, &["rev-parse", "main:b"], b""))
    );
    let projected_blob = text(run(
        &restored,
        &["hash-object", "--stdin"],
        b"nested bytes\0",
    ));
    let projected_child = text(run(
        &restored,
        &["mktree"],
        format!("100755 blob {projected_blob}\tfile\n").as_bytes(),
    ));
    let projected_root = text(run(&restored, &["mktree"], format!("040000 tree {projected_child}\ta\n100644 blob {projected_blob}\ta-1\n040000 tree {projected_child}\tb\n").as_bytes()));
    assert_eq!(
        text(run(
            &restored,
            &[
                "rev-parse",
                &format!("{}^{{tree}}", manifest.commits[&head.to_string()])
            ],
            b""
        )),
        projected_root
    );
    let next = temp.path().join("limited");
    let mut request = request(&next, head, HashAlgorithm::Sha1);
    request.limits.path_visits = 6;
    assert!(matches!(
        export(&s, request),
        Err(treeharbor_git_bridge::Error::Limit)
    ));
    assert!(!next.exists());
}

#[test]
fn mixed_native_frame_versions_export_file_directory_history_with_original_trees() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let s = store(&temp.path().join("store"));
    let before = tree(&s, &[(b"a", EntryMode::File, b"original")]);
    let parent = commit(&s, before, vec![], "before");
    let child_snapshot = tree(&s, &[(b"file", EntryMode::Executable, b"child")]);
    let Object::Snapshot { root: child } = s.get(child_snapshot).unwrap() else {
        panic!()
    };
    let root = s
        .put(&Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"a".to_vec()).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Directory,
                    target: child,
                },
            }])
            .unwrap(),
        ))
        .unwrap();
    let after = s.put(&Object::Snapshot { root }).unwrap();
    let changes = diff_snapshots(&s, before, after, TreeDiffLimits::default()).unwrap();
    assert_eq!(changes.format_version(), 2);
    let operations = s.put(&Object::ChangeOperations(changes)).unwrap();
    let template = commit(&s, after, vec![parent], "after");
    let Object::NativeCommit(mut native) = s.get(template).unwrap() else {
        panic!()
    };
    native.changes = operations;
    let head = s.put(&Object::NativeCommit(native)).unwrap();
    let output = temp.path().join("export");
    let manifest = export(&s, request(&output, head, HashAlgorithm::Sha1)).unwrap();
    let restored = temp.path().join("restored");
    run(
        temp.path(),
        &[
            "clone",
            "--bare",
            output.join("history.bundle").to_str().unwrap(),
            restored.to_str().unwrap(),
        ],
        b"",
    );
    assert_eq!(manifest.commits.len(), 2);
    assert_eq!(run(&restored, &["show", "main:a/file"], b""), b"child");
    assert_eq!(run(&restored, &["show", "main^:a"], b""), b"original");
    run(&restored, &["fsck", "--strict"], b"");
}
