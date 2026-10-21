mod common;
use common::*;
use treeharbor_core::{EntryMode::*, Object, ObjectId, ObjectStore};
use treeharbor_merge::{
    ConflictKind, Error, Limits, Outcome, Unsupported, merge_revisions, merge_snapshots,
};

fn merged(
    store: &impl ObjectStore,
    b: &Files,
    l: &Files,
    r: &Files,
) -> treeharbor_merge::Result<Outcome> {
    let [b, l, r] = [snapshot(store, b), snapshot(store, l), snapshot(store, r)];
    merge_snapshots(store, b, l, r, Limits::default())
}

#[test]
fn nonoverlapping_content_mode_and_binary_paths_keep_exact_inputs() {
    let s = Store::default();
    let b = files(&[(b"a\xff", File, b"one\ntwo\nthree\nfour\n")]);
    let l = files(&[(b"a\xff", File, b"ONE\ntwo\nthree\nfour\n")]);
    let r = files(&[(b"a\xff", Executable, b"one\ntwo\nthree\nFOUR\n")]);
    let [bi, li, ri] = [snapshot(&s, &b), snapshot(&s, &l), snapshot(&s, &r)];
    let Outcome::Clean(result) = merge_snapshots(&s, bi, li, ri, Limits::default()).unwrap() else {
        panic!()
    };
    assert_eq!(
        (result.base(), result.current(), result.proposed()),
        (bi, li, ri)
    );
    assert_eq!(result.profile(), treeharbor_merge::PROFILE);
    assert_eq!(
        contents(&s, result.result()),
        files(&[(b"a\xff", Executable, b"ONE\ntwo\nthree\nFOUR\n")])
    );
}

#[test]
fn conflicts_never_write_a_partial_tree_or_conflict_markers() {
    let cases = [
        (
            File,
            b"base\n".as_slice(),
            File,
            b"left\n".as_slice(),
            File,
            b"right\n".as_slice(),
            ConflictKind::Content,
        ),
        (
            File,
            b"a\0b",
            File,
            b"a\0L",
            File,
            b"a\0R",
            ConflictKind::Binary,
        ),
        (
            Symlink,
            b"base",
            Symlink,
            b"left",
            Symlink,
            b"right",
            ConflictKind::Symlink,
        ),
        (
            File,
            b"base",
            Symlink,
            b"left",
            File,
            b"right",
            ConflictKind::Type,
        ),
    ];
    for (bm, b, lm, l, rm, r, expected) in cases {
        let s = Store::default();
        let [bi, li, ri] = [
            snapshot(&s, &files(&[(b"f", bm, b)])),
            snapshot(&s, &files(&[(b"f", lm, l)])),
            snapshot(&s, &files(&[(b"f", rm, r)])),
        ];
        s.writes.borrow_mut().clear();
        let Outcome::Conflicted(conflicts) =
            merge_snapshots(&s, bi, li, ri, Limits::default()).unwrap()
        else {
            panic!()
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, expected);
        assert!(s.writes.borrow().is_empty());
    }
}

#[test]
fn exact_sibling_rename_follows_opposite_edit_and_preserves_mode() {
    let s = Store::default();
    let b = plain(&[("src/old", "original\n")]);
    let l = files(&[(b"src/new", Executable, b"original\n")]);
    let r = plain(&[("src/old", "edited\n")]);
    let Outcome::Clean(result) = merged(&s, &b, &l, &r).unwrap() else {
        panic!()
    };
    assert_eq!(
        contents(&s, result.result()),
        files(&[(b"src/new", Executable, b"edited\n")])
    );
    let Outcome::Clean(reverse) = merged(&s, &b, &r, &l).unwrap() else {
        panic!()
    };
    assert_eq!(result.result(), reverse.result());
}

#[test]
fn rename_delete_rename_rename_and_destination_collisions_remain_proposals() {
    let s = Store::default();
    let b = plain(&[("old", "original\n")]);
    let l = plain(&[("left", "original\n")]);
    for (r, expected) in [
        (Files::new(), ConflictKind::RenameDelete),
        (
            plain(&[("right", "original\n")]),
            ConflictKind::RenameRename,
        ),
        (
            plain(&[("old", "edited\n"), ("left", "new file\n")]),
            ConflictKind::DestinationCollision,
        ),
    ] {
        let Outcome::Conflicted(conflicts) = merged(&s, &b, &l, &r).unwrap() else {
            panic!()
        };
        assert!(conflicts.iter().any(|c| c.kind == expected));
    }
}

#[test]
fn uncertain_moves_and_attributes_are_explicitly_unsupported() {
    let s = Store::default();
    let b = plain(&[("old", "original\n")]);
    let r = plain(&[("old", "other edit\n")]);
    for (l, expected) in [
        (
            plain(&[("new", "edited rename\n")]),
            Unsupported::PossibleEditedRename,
        ),
        (
            plain(&[("dir/old", "original\n")]),
            Unsupported::CrossDirectoryRename,
        ),
        (
            plain(&[("new", "original\n"), ("other", "original\n")]),
            Unsupported::AmbiguousRename,
        ),
        (
            plain(&[("old", "original\n"), (".gitattributes", "* merge=ours\n")]),
            Unsupported::Attributes,
        ),
    ] {
        assert!(
            matches!(merged(&s, &b, &l, &r), Err(Error::Unsupported(actual)) if actual == expected)
        );
    }
}

#[test]
fn file_directory_collision_is_not_hidden_by_byte_order_neighbor() {
    let s = Store::default();
    let b = Files::new();
    let l = plain(&[("a", "file"), ("a-1", "separator")]);
    let r = plain(&[("a/b", "nested")]);
    let Outcome::Conflicted(conflicts) = merged(&s, &b, &l, &r).unwrap() else {
        panic!()
    };
    assert_eq!(conflicts[0].kind, ConflictKind::FileDirectory);
    assert_eq!(conflicts[0].path.as_bytes(), b"a");
}

#[test]
fn limits_fail_before_writing_and_unchanged_blobs_are_not_read() {
    let s = Store::default();
    let mut b = plain(&[("f", "a\nb\nc\nd\n")]);
    b.insert(b"large".to_vec(), (File, vec![b'x'; 1024 * 1024]));
    let mut l = b.clone();
    l.get_mut(b"f".as_slice()).unwrap().1 = b"A\nb\nc\nd\n".to_vec();
    let mut r = b.clone();
    r.get_mut(b"f".as_slice()).unwrap().1 = b"a\nb\nc\nD\n".to_vec();
    let [bi, li, ri] = [snapshot(&s, &b), snapshot(&s, &l), snapshot(&s, &r)];
    for limits in [
        Limits {
            max_directories: 0,
            ..Limits::default()
        },
        Limits {
            max_entries: 1,
            ..Limits::default()
        },
        Limits {
            max_path_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_read_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_write_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_text_bytes: 1,
            ..Limits::default()
        },
        Limits {
            max_text_lines: 1,
            ..Limits::default()
        },
        Limits {
            max_diff_work: 1,
            ..Limits::default()
        },
    ] {
        s.writes.borrow_mut().clear();
        assert!(matches!(
            merge_snapshots(&s, bi, li, ri, limits),
            Err(Error::Limit(_))
        ));
        assert!(s.writes.borrow().is_empty());
    }
    s.reads.borrow_mut().clear();
    let large = Object::Blob(vec![b'x'; 1024 * 1024]).id().unwrap();
    assert!(matches!(
        merge_snapshots(
            &s,
            bi,
            li,
            ri,
            Limits {
                max_read_bytes: 10_000,
                ..Limits::default()
            }
        )
        .unwrap(),
        Outcome::Clean(_)
    ));
    assert!(!s.reads.borrow().contains(&large));
}

#[test]
fn merge_bases_are_complete_and_never_chosen_arbitrarily() {
    let s = Store::default();
    let tree = snapshot(&s, &plain(&[("f", "base")]));
    let root = commit(&s, tree, vec![], "root");
    let a = commit(&s, tree, vec![root], "a");
    let b = commit(&s, tree, vec![root], "b");
    let left = commit(&s, tree, vec![a, b], "left");
    let right = commit(&s, tree, vec![b, a], "right");
    let mut bases = vec![a, b];
    bases.sort();
    assert!(
        matches!(merge_revisions(&s, left, right, Limits::default()), Err(Error::MultipleBases(actual)) if actual == bases)
    );
    let other = commit(&s, tree, vec![], "unrelated");
    assert!(matches!(
        merge_revisions(&s, root, other, Limits::default()),
        Err(Error::UnrelatedHistories)
    ));
    assert!(matches!(
        merge_revisions(&s, root, a, Limits::default()).unwrap(),
        Outcome::Clean(_)
    ));
    assert!(
        merge_revisions(
            &s,
            root,
            a,
            Limits {
                max_commits: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
}

#[test]
fn corrupt_or_missing_objects_are_errors_not_merge_conflicts() {
    let s = Store::default();
    let b = snapshot(&s, &plain(&[("f", "base")]));
    let l = snapshot(&s, &plain(&[("f", "left")]));
    let r = snapshot(&s, &plain(&[("f", "right")]));
    let blob = Object::Blob(b"left".to_vec()).id().unwrap();
    s.objects.borrow_mut().remove(&blob);
    assert!(matches!(
        merge_snapshots(&s, b, l, r, Limits::default()),
        Err(Error::Core(treeharbor_core::Error::MissingObject(_)))
    ));
    s.objects
        .borrow_mut()
        .insert(blob, Object::Blob(b"wrong bytes".to_vec()));
    assert!(matches!(
        merge_snapshots(&s, b, l, r, Limits::default()),
        Err(Error::Core(treeharbor_core::Error::InvalidObject(_)))
    ));
    assert!(merge_snapshots(&s, ObjectId::from_bytes([0; 32]), l, r, Limits::default()).is_err());
}

#[test]
fn native_path_runs_without_git() {
    const CHILD: &str = "TREEHARBOR_MERGE_NO_GIT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "native_path_runs_without_git", "--nocapture"])
            .env(CHILD, "1")
            .env("PATH", "/treeharbor-no-executables")
            .env_remove("GITHUB_TOKEN")
            .env_remove("GH_TOKEN")
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let tenant = treeharbor_storage::TenantId::new("merge-test").unwrap();
    let store = treeharbor_storage::LocalStore::open(root.path().join("store"), &tenant).unwrap();
    let Outcome::Clean(result) = merged(
        &store,
        &plain(&[("f", "a\nb\nc\nd\n")]),
        &plain(&[("f", "A\nb\nc\nd\n")]),
        &plain(&[("f", "a\nb\nc\nD\n")]),
    )
    .unwrap() else {
        panic!()
    };
    drop(store);
    let reopened =
        treeharbor_storage::LocalStore::open(root.path().join("store"), &tenant).unwrap();
    assert_eq!(
        contents(&reopened, result.result()),
        plain(&[("f", "A\nb\nc\nD\n")])
    );
}

#[test]
fn a_later_conflict_discards_earlier_staged_merges() {
    let s = Store::default();
    let b = snapshot(&s, &plain(&[("a", "1\n2\n3\n4\n"), ("z", "base")]));
    let l = snapshot(&s, &plain(&[("a", "ONE\n2\n3\n4\n"), ("z", "left")]));
    let r = snapshot(&s, &plain(&[("a", "1\n2\n3\nFOUR\n"), ("z", "right")]));
    s.writes.borrow_mut().clear();
    assert!(matches!(
        merge_snapshots(&s, b, l, r, Limits::default()).unwrap(),
        Outcome::Conflicted(_)
    ));
    assert!(s.writes.borrow().is_empty());
}

#[test]
fn incorrect_write_identity_or_io_failure_never_returns_clean() {
    struct Faulty<'a> {
        store: &'a Store,
        wrong_id: bool,
    }
    impl ObjectStore for Faulty<'_> {
        fn get(&self, id: ObjectId) -> treeharbor_core::Result<Object> {
            self.store.get(id)
        }
        fn put(&self, object: &Object) -> treeharbor_core::Result<ObjectId> {
            if self.wrong_id {
                self.store.put(object)?;
                Ok(ObjectId::from_bytes([0; 32]))
            } else {
                Err(treeharbor_core::Error::Storage(
                    "injected IO failure".into(),
                ))
            }
        }
    }
    let s = Store::default();
    let b = snapshot(&s, &plain(&[("f", "1\n2\n3\n4\n")]));
    let l = snapshot(&s, &plain(&[("f", "ONE\n2\n3\n4\n")]));
    let r = snapshot(&s, &plain(&[("f", "1\n2\n3\nFOUR\n")]));
    for wrong_id in [false, true] {
        assert!(matches!(
            merge_snapshots(
                &Faulty {
                    store: &s,
                    wrong_id
                },
                b,
                l,
                r,
                Limits::default()
            ),
            Err(Error::Core(_))
        ));
    }
}

#[test]
fn native_empty_directory_is_not_silently_erased() {
    use treeharbor_core::{Directory, Entry, EntryRef, Name};
    let s = Store::default();
    let empty = s
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap();
    let root = s
        .put(&Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"empty".to_vec()).unwrap(),
                value: EntryRef {
                    mode: Directory,
                    target: empty,
                },
            }])
            .unwrap(),
        ))
        .unwrap();
    let b = snapshot(&s, &plain(&[("f", "base")]));
    let l = s.put(&Object::Snapshot { root }).unwrap();
    let r = snapshot(&s, &plain(&[("f", "right")]));
    assert!(matches!(
        merge_snapshots(&s, b, l, r, Limits::default()),
        Err(Error::Unsupported(Unsupported::EmptyDirectory(_)))
    ));
}

#[test]
fn history_byte_budget_and_identity_are_checked_before_tree_reuse() {
    let s = Store::default();
    let tree = snapshot(&s, &Files::new());
    let root = commit(&s, tree, vec![], "root");
    let tip = commit(&s, tree, vec![root], "tip");
    assert!(
        merge_revisions(
            &s,
            root,
            tip,
            Limits {
                max_read_bytes: 1,
                ..Limits::default()
            }
        )
        .is_err()
    );
    let Object::NativeCommit(mut corrupt) = s.objects.borrow()[&root].clone() else {
        panic!()
    };
    corrupt.message = "different".into();
    s.objects
        .borrow_mut()
        .insert(root, Object::NativeCommit(corrupt));
    assert!(matches!(
        merge_revisions(&s, root, tip, Limits::default()),
        Err(Error::Core(treeharbor_core::Error::InvalidObject(_)))
    ));
}
