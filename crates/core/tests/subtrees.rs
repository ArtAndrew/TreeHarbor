use std::{cell::RefCell, collections::BTreeMap};
use treeharbor_core::*;

#[derive(Default)]
struct Store {
    objects: RefCell<BTreeMap<ObjectId, Object>>,
    reads: RefCell<Vec<ObjectId>>,
    writes: RefCell<Vec<ObjectId>>,
}
impl ObjectStore for Store {
    fn get(&self, id: ObjectId) -> Result<Object> {
        self.reads.borrow_mut().push(id);
        self.objects
            .borrow()
            .get(&id)
            .cloned()
            .ok_or(Error::MissingObject(id))
    }
    fn put(&self, object: &Object) -> Result<ObjectId> {
        let id = object.id()?;
        self.writes.borrow_mut().push(id);
        self.objects.borrow_mut().insert(id, object.clone());
        Ok(id)
    }
}
fn blob(s: &Store, bytes: &[u8], mode: EntryMode) -> EntryRef {
    EntryRef {
        mode,
        target: s.put(&Object::Blob(bytes.to_vec())).unwrap(),
    }
}
fn dir(s: &Store, entries: &[(&[u8], EntryRef)]) -> EntryRef {
    EntryRef {
        mode: EntryMode::Directory,
        target: s
            .put(&Object::Directory(
                Directory::new(
                    entries
                        .iter()
                        .map(|(name, value)| Entry {
                            name: Name::new(name.to_vec()).unwrap(),
                            value: *value,
                        })
                        .collect(),
                )
                .unwrap(),
            ))
            .unwrap(),
    }
}
fn snapshot(s: &Store, root: EntryRef) -> ObjectId {
    s.put(&Object::Snapshot { root: root.target }).unwrap()
}
fn op(path: &[u8], expected: Option<EntryRef>, replacement: Option<EntryRef>) -> ChangeOperation {
    ChangeOperation {
        path: RepoPath::new(path.to_vec()).unwrap(),
        expected,
        replacement,
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[test]
fn independent_golden_frames_and_mixed_version_typed_references() {
    let s = Store::default();
    let before = blob(&s, b"before\n", EntryMode::File);
    let after = blob(&s, b"after\n", EntryMode::Executable);
    let directory = dir(&s, &[(b"file", after)]);
    let changes = Changes::with_subtrees(vec![op(b"a", Some(before), Some(directory))]).unwrap();
    assert_eq!(changes.format_version(), 2);
    let object = Object::ChangeOperations(changes);
    let objects = [
        s.get(before.target).unwrap(),
        s.get(after.target).unwrap(),
        s.get(directory.target).unwrap(),
        object.clone(),
    ];
    for (object, vector) in objects
        .iter()
        .zip(include_str!("fixtures/subtree-v2.txt").lines())
    {
        let (id, frame) = vector.split_once(' ').unwrap();
        assert_eq!(object.id().unwrap().hex(), id);
        assert_eq!(hex(&object.encode().unwrap()), frame);
        assert_eq!(Object::decode(&object.encode().unwrap()).unwrap(), *object);
    }
    assert_eq!(
        object.references(),
        vec![(before.target, 1), (directory.target, 2)]
    );
    let id = s.put(&object).unwrap();
    assert_eq!(validate_graph(&s, id, 8).unwrap().len(), 4);
    let leaf = Object::ChangeOperations(
        Changes::with_subtrees(vec![op(b"f", None, Some(before))]).unwrap(),
    );
    assert_eq!(
        leaf,
        Object::ChangeOperations(Changes::new(vec![op(b"f", None, Some(before))]).unwrap())
    );
    assert_eq!(&leaf.encode().unwrap()[11..13], &[0, 1]);
}

#[test]
fn rejects_version_aliases_wrong_kinds_and_overlapping_paths() {
    let s = Store::default();
    let d = dir(&s, &[]);
    let b = blob(&s, b"x", EntryMode::File);
    assert!(Changes::new(vec![op(b"a", None, Some(d))]).is_err());
    let object =
        Object::ChangeOperations(Changes::with_subtrees(vec![op(b"a", None, Some(d))]).unwrap());
    let mut raw = object.encode().unwrap();
    raw[12] = 1;
    assert!(Object::decode(&raw).is_err());
    let leaf = Object::ChangeOperations(Changes::new(vec![op(b"a", None, Some(b))]).unwrap());
    let mut raw = leaf.encode().unwrap();
    raw[12] = 2;
    assert!(Object::decode(&raw).is_err());
    let mut raw = Object::Blob(vec![]).encode().unwrap();
    raw[12] = 2;
    assert!(matches!(
        Object::decode(&raw),
        Err(Error::UnsupportedVersion(2))
    ));
    for paths in [vec![b"a".as_slice(), b"a-1", b"a/b"], vec![b"a", b"a"]] {
        assert!(matches!(
            Changes::with_subtrees(paths.into_iter().map(|p| op(p, None, Some(d))).collect()),
            Err(Error::OverlappingPaths)
        ));
    }
    let wrong = EntryRef {
        mode: EntryMode::Directory,
        target: b.target,
    };
    let changes = Changes::with_subtrees(vec![op(b"bad", None, Some(wrong))]).unwrap();
    let root = dir(&s, &[]);
    s.writes.borrow_mut().clear();
    assert!(matches!(
        apply_changes(&s, root.target, &changes),
        Err(Error::WrongKind)
    ));
    assert!(s.writes.borrow().is_empty());
}

#[test]
fn exact_subtree_predicates_fail_before_any_write_and_both_type_transitions_replay() {
    let s = Store::default();
    let f = blob(&s, b"file", EntryMode::File);
    let g = blob(&s, b"new", EntryMode::Symlink);
    let d = dir(&s, &[(b"child", g)]);
    let before = dir(&s, &[(b"a", f), (b"z", g)]);
    let after = dir(&s, &[(b"a", d), (b"z", g)]);
    let change = Changes::with_subtrees(vec![op(b"a", Some(f), Some(d))]).unwrap();
    assert_eq!(
        apply_changes(&s, before.target, &change).unwrap(),
        after.target
    );
    let reverse = Changes::with_subtrees(vec![op(b"a", Some(d), Some(f))]).unwrap();
    assert_eq!(
        apply_changes(&s, after.target, &reverse).unwrap(),
        before.target
    );
    let stale = dir(&s, &[(b"a", dir(&s, &[(b"child", f)])), (b"z", g)]);
    s.writes.borrow_mut().clear();
    assert!(matches!(
        apply_changes(&s, stale.target, &reverse),
        Err(Error::PredicateMismatch)
    ));
    assert!(s.writes.borrow().is_empty());
    let two = Changes::with_subtrees(vec![op(b"a", Some(f), Some(d)), op(b"z", Some(f), Some(g))])
        .unwrap();
    assert!(matches!(
        apply_changes(&s, before.target, &two),
        Err(Error::PredicateMismatch)
    ));
    assert!(s.writes.borrow().is_empty());
}

#[test]
fn structural_diff_replays_all_pairs_with_empty_directories_and_reuses_equal_subtrees() {
    let s = Store::default();
    let f = blob(&s, b"a", EntryMode::File);
    let g = blob(&s, b"b", EntryMode::Executable);
    let empty = dir(&s, &[]);
    let nonempty = dir(&s, &[(b"f", f)]);
    let nested = dir(&s, &[(b"child", nonempty), (b"empty", empty)]);
    let roots = [
        empty,
        dir(&s, &[(b"a", f)]),
        dir(&s, &[(b"a", nonempty)]),
        dir(&s, &[(b"a", empty)]),
        dir(&s, &[(b"a", nested), (b"a-1", f)]),
        dir(&s, &[(b"a", g)]),
        dir(&s, &[(b"a", nonempty), (b"z", nested)]),
    ];
    let snapshots: Vec<_> = roots.iter().map(|r| snapshot(&s, *r)).collect();
    for (i, before) in snapshots.iter().enumerate() {
        for (j, after) in snapshots.iter().enumerate() {
            s.writes.borrow_mut().clear();
            s.reads.borrow_mut().clear();
            let changes = diff_snapshots(&s, *before, *after, TreeDiffLimits::default()).unwrap();
            assert!(s.writes.borrow().is_empty());
            assert!(
                !s.reads
                    .borrow()
                    .iter()
                    .any(|id| *id == f.target || *id == g.target)
            );
            assert_eq!(
                apply_changes(&s, roots[i].target, &changes).unwrap(),
                roots[j].target,
                "{i} -> {j}"
            );
        }
    }
    let before = snapshot(&s, dir(&s, &[(b"a", nested), (b"z", f)]));
    let after = snapshot(&s, dir(&s, &[(b"a", nested), (b"z", g)]));
    s.reads.borrow_mut().clear();
    let changes = diff_snapshots(&s, before, after, TreeDiffLimits::default()).unwrap();
    assert_eq!(changes.operations().len(), 1);
    assert_eq!(changes.format_version(), 1);
    assert!(!s.reads.borrow().contains(&nested.target));
}

#[test]
fn structural_diff_enforces_all_budgets_and_verified_source_identity_without_writes() {
    let s = Store::default();
    let b = snapshot(&s, dir(&s, &[]));
    let a = snapshot(&s, dir(&s, &[(b"a", blob(&s, b"file", EntryMode::File))]));
    s.writes.borrow_mut().clear();
    for limits in [
        TreeDiffLimits {
            directories: 1,
            ..Default::default()
        },
        TreeDiffLimits {
            entries: 0,
            ..Default::default()
        },
        TreeDiffLimits {
            path_bytes: 0,
            ..Default::default()
        },
        TreeDiffLimits {
            read_bytes: 1,
            ..Default::default()
        },
        TreeDiffLimits {
            operations: 0,
            ..Default::default()
        },
    ] {
        assert!(matches!(
            diff_snapshots(&s, b, a, limits),
            Err(Error::TooLarge)
        ));
        assert!(s.writes.borrow().is_empty());
    }
    s.objects.borrow_mut().insert(b, Object::Blob(vec![]));
    assert!(matches!(
        diff_snapshots(&s, b, a, TreeDiffLimits::default()),
        Err(Error::InvalidObject(_))
    ));
}
