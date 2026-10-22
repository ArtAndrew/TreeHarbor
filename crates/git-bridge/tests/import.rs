#![cfg(unix)]
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
use treeharbor_core::*;
use treeharbor_git_bridge::{Export, HashAlgorithm, Import, Limits, export, import};
use treeharbor_storage::{LocalStore, TenantId};
fn git(repo: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut p = Command::new("/usr/bin/git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    p.stdin.take().unwrap().write_all(input).unwrap();
    let r = p.wait_with_output().unwrap();
    assert!(
        r.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&r.stderr)
    );
    r.stdout
}
fn text(v: Vec<u8>) -> String {
    String::from_utf8(v).unwrap().trim().into()
}
fn oid(s: &str, algorithm: HashAlgorithm) -> GitOid {
    let RevisionRef::Git(oid) = format!("git:{}:{s}", algorithm.name()).parse().unwrap() else {
        panic!()
    };
    oid
}
struct Fixture {
    dir: tempfile::TempDir,
    repo: PathBuf,
    algorithm: HashAlgorithm,
}
impl Fixture {
    fn new(algorithm: HashAlgorithm) -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let repo = dir.path().join("source.git");
        git(
            dir.path(),
            &[
                "init",
                "--bare",
                &format!("--object-format={}", algorithm.name()),
                repo.to_str().unwrap(),
            ],
            b"",
        );
        Self {
            dir,
            repo,
            algorithm,
        }
    }
    fn tree(&self, entries: &[(u32, &str, &[u8])]) -> String {
        let mut raw = Vec::new();
        for (mode, oid, name) in entries {
            raw.extend_from_slice(
                format!(
                    "{mode:o} {} {oid}\t",
                    if *mode == 0o40000 {
                        "tree"
                    } else if *mode == 0o160000 {
                        "commit"
                    } else {
                        "blob"
                    }
                )
                .as_bytes(),
            );
            raw.extend_from_slice(name);
            raw.push(0);
        }
        text(git(&self.repo, &["mktree", "-z"], &raw))
    }
    fn blob(&self, body: &[u8]) -> String {
        text(git(&self.repo, &["hash-object", "-w", "--stdin"], body))
    }
    fn commit(&self, tree: &str, parents: &[&str], message: &str) -> String {
        let mut raw = format!("tree {tree}\n");
        for parent in parents {
            raw.push_str(&format!("parent {parent}\n"));
        }
        raw.push_str(&format!("author Author <author@example.invalid> 1234567890 -0530\ncommitter Different <committer@example.invalid> 1234567999 +0230\nencoding UTF-8\nx-preserve exact\n multiline\ngpgsig fixture-not-a-signature\n\n{message}"));
        text(git(
            &self.repo,
            &["hash-object", "-w", "-t", "commit", "--stdin"],
            raw.as_bytes(),
        ))
    }
    fn bundle(&self, head: &str, name: &str) -> PathBuf {
        git(&self.repo, &["update-ref", "refs/heads/main", head], b"");
        let path = self.dir.path().join(name);
        git(
            &self.repo,
            &[
                "bundle",
                "create",
                path.to_str().unwrap(),
                "refs/heads/main",
            ],
            b"",
        );
        path
    }
    fn store(&self, name: &str) -> LocalStore {
        LocalStore::open(
            self.dir.path().join(name),
            &TenantId::new("import-test").unwrap(),
        )
        .unwrap()
    }
    fn request<'a>(&'a self, bundle: &'a Path, head: &str) -> Import<'a> {
        Import {
            git: Path::new("/usr/bin/git"),
            bundle,
            scratch: self.dir.path(),
            commit: oid(head, self.algorithm),
            limits: Limits::default(),
        }
    }
}
#[test]
fn an_unrelated_bundled_branch_does_not_enter_the_selected_history() {
    let f = Fixture::new(HashAlgorithm::Sha1);
    let empty = f.tree(&[]);
    let selected = f.commit(&empty, &[], "selected\n");
    let secret = f.blob(b"other branch content");
    let other_tree = f.tree(&[(0o100644, &secret, b"unselected")]);
    let other = f.commit(&other_tree, &[], "other\n");
    git(&f.repo, &["update-ref", "refs/heads/main", &selected], b"");
    git(&f.repo, &["update-ref", "refs/heads/other", &other], b"");
    let bundle = f.dir.path().join("all.bundle");
    git(
        &f.repo,
        &["bundle", "create", bundle.to_str().unwrap(), "--all"],
        b"",
    );
    let store = f.store("native");
    let imported = import(&store, f.request(&bundle, &selected)).unwrap();
    assert_eq!(imported.objects.len(), 5);
    assert_eq!(imported.filesystem_entries, 0);
    let Object::NativeCommit(c) = store.get(imported.commit).unwrap() else {
        panic!()
    };
    assert!(c.parents.is_empty());
    assert_eq!(
        c.git_origin.unwrap().oid,
        oid(&selected, HashAlgorithm::Sha1)
    );
    assert!(
        !imported
            .objects
            .contains(&Object::Blob(b"other branch content".to_vec()).id().unwrap())
    );
}
#[test]
fn actual_git_history_import_replays_every_delta_and_exports_all_original_ids() {
    for algorithm in [HashAlgorithm::Sha1, HashAlgorithm::Sha256] {
        let f = Fixture::new(algorithm);
        let binary = f.blob(b"binary\0\xff\n");
        let text_blob = f.blob(b"edited");
        let link = f.blob(b"a/file");
        let empty = f.tree(&[]);
        let flat = f.tree(&[(0o100644, &binary, b"a")]);
        let child = f.tree(&[(0o100755, &text_blob, b"file")]);
        let nested = f.tree(&[
            (0o40000, &child, b"a"),
            (0o100644, &binary, b"a.c"),
            (0o100644, &binary, b"raw-\xff\n"),
            (0o120000, &link, b"link"),
        ]);
        let root = f.commit(&empty, &[], "empty root");
        let file = f.commit(&flat, &[&root], "file");
        let dir = f.commit(&nested, &[&file], "file to directory");
        let back = f.commit(&flat, &[&dir], "directory to file");
        let unrelated = f.commit(&empty, &[], "independent root");
        let merge = f.commit(
            &nested,
            &[&back, &unrelated, &file],
            "merge without final newline",
        );
        let expected = [&root, &file, &dir, &back, &unrelated, &merge]
            .into_iter()
            .map(|id| (id.clone(), git(&f.repo, &["cat-file", "commit", id], b"")))
            .collect::<BTreeMap<_, _>>();
        let bundle = f.bundle(&merge, "complete.bundle");
        let store = f.store("native");
        let imported = import(&store, f.request(&bundle, &merge)).unwrap();
        assert_eq!(imported.filesystem_entries, 5);
        let empty_root = store
            .put(&Object::Directory(Directory::new(vec![]).unwrap()))
            .unwrap();
        let empty_base = store.put(&Object::Snapshot { root: empty_root }).unwrap();
        let mut commits = 0;
        for id in &imported.objects {
            if let Object::NativeCommit(c) = store.get(*id).unwrap() {
                commits += 1;
                let base = if let Some(parent) = c.parents.first() {
                    let Object::NativeCommit(p) = store.get(*parent).unwrap() else {
                        panic!()
                    };
                    p.snapshot
                } else {
                    empty_base
                };
                let Object::ChangeOperations(delta) = store.get(c.changes).unwrap() else {
                    panic!()
                };
                assert_eq!(
                    prepare_change(&store, base, &delta)
                        .unwrap()
                        .result_snapshot(),
                    c.snapshot
                );
                let Object::Blob(raw) = store.get(c.git_origin.as_ref().unwrap().raw).unwrap()
                else {
                    panic!()
                };
                assert!(expected.values().any(|value| *value == raw));
            }
        }
        assert_eq!(commits, 6);
        let repeated = import(&f.store("again"), f.request(&bundle, &merge)).unwrap();
        assert_eq!(imported.commit, repeated.commit);
        assert_eq!(imported.objects, repeated.objects);
        fs::remove_dir_all(&f.repo).unwrap();
        fs::remove_file(&bundle).unwrap();
        let output = f.dir.path().join("export");
        let exported = export(
            &store,
            Export {
                git: Path::new("/usr/bin/git"),
                output: &output,
                refs: BTreeMap::from([("refs/heads/main".into(), imported.commit)]),
                algorithm,
                limits: Limits::default(),
            },
        )
        .unwrap();
        assert_eq!(exported.refs["refs/heads/main"].git, merge);
        assert_eq!(
            exported
                .commits
                .values()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>(),
            expected.keys().cloned().collect()
        );
        let restored = f.dir.path().join("restored.git");
        git(
            f.dir.path(),
            &[
                "clone",
                "--bare",
                output.join("history.bundle").to_str().unwrap(),
                restored.to_str().unwrap(),
            ],
            b"",
        );
        for (oid, raw) in expected {
            assert_eq!(git(&restored, &["cat-file", "commit", &oid], b""), raw);
        }
        assert_eq!(git(&restored, &["show", "main:a/file"], b""), b"edited");
    }
}
#[test]
fn incomplete_corrupt_wrong_tip_symlink_and_budget_inputs_are_rejected() {
    let f = Fixture::new(HashAlgorithm::Sha1);
    let empty = f.tree(&[]);
    let root = f.commit(&empty, &[], "root");
    let head = f.commit(&empty, &[&root], "next");
    let bundle = f.bundle(&head, "full.bundle");
    let incremental = f.dir.path().join("incremental.bundle");
    git(
        &f.repo,
        &[
            "bundle",
            "create",
            incremental.to_str().unwrap(),
            "refs/heads/main",
            &format!("^{root}"),
        ],
        b"",
    );
    assert!(import(&f.store("partial"), f.request(&incremental, &head)).is_err());
    let corrupt = f.dir.path().join("corrupt.bundle");
    let mut bytes = fs::read(&bundle).unwrap();
    *bytes.last_mut().unwrap() ^= 1;
    fs::write(&corrupt, bytes).unwrap();
    assert!(import(&f.store("corrupt"), f.request(&corrupt, &head)).is_err());
    assert!(import(&f.store("wrong"), f.request(&bundle, &"0".repeat(40))).is_err());
    let link = f.dir.path().join("linked.bundle");
    symlink(&bundle, &link).unwrap();
    assert!(import(&f.store("link"), f.request(&link, &head)).is_err());
    for (i, limits) in [
        Limits {
            objects: 1,
            ..Default::default()
        },
        Limits {
            source_bytes: 1,
            ..Default::default()
        },
        Limits {
            edges: 1,
            ..Default::default()
        },
        Limits {
            path_visits: 1,
            ..Default::default()
        },
        Limits {
            stream_bytes: 1,
            ..Default::default()
        },
    ]
    .into_iter()
    .enumerate()
    {
        let mut req = f.request(&bundle, &head);
        req.limits = limits;
        assert!(import(&f.store(&format!("bounded{i}")), req).is_err());
    }
    assert_eq!(text(git(&f.repo, &["rev-parse", "main"], b"")), head);
}
#[test]
fn unsupported_lfs_submodule_and_non_utf8_metadata_fail_explicitly() {
    let f = Fixture::new(HashAlgorithm::Sha1);
    let empty = f.tree(&[]);
    let root = f.commit(&empty, &[], "root");
    let lfs = f.blob(b"version https://git-lfs.github.com/spec/v1\noid sha256:missing\nsize 10\n");
    for (i, tree) in [
        f.tree(&[(0o100644, &lfs, b"large")]),
        f.tree(&[(0o160000, &root, b"module")]),
        f.tree(&[(0o40000, &empty, b"nested-empty")]),
    ]
    .into_iter()
    .enumerate()
    {
        let head = f.commit(&tree, &[], "unsupported");
        let bundle = f.bundle(&head, &format!("bad{i}.bundle"));
        assert!(import(&f.store(&format!("bad{i}")), f.request(&bundle, &head)).is_err());
    }
    let raw=format!("tree {empty}\nauthor Test <test@example.invalid> 0 +0000\ncommitter Test <test@example.invalid> 0 +0000\n\n").into_bytes();
    let mut raw = raw;
    raw.push(255);
    let head = text(git(
        &f.repo,
        &["hash-object", "-w", "-t", "commit", "--stdin"],
        &raw,
    ));
    let bundle = f.bundle(&head, "encoding.bundle");
    assert!(import(&f.store("encoding"), f.request(&bundle, &head)).is_err());
}
