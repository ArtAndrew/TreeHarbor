use std::{collections::BTreeMap, sync::Mutex};
use treeharbor_core::*;

#[derive(Default)]
struct Memory(Mutex<BTreeMap<ObjectId, Object>>);
impl ObjectStore for Memory {
    fn get(&self, id: ObjectId) -> Result<Object> {
        self.0
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(Error::MissingObject(id))
    }
    fn put(&self, object: &Object) -> Result<ObjectId> {
        let id = object.id()?;
        self.0.lock().unwrap().insert(id, object.clone());
        Ok(id)
    }
}
fn path(s: impl AsRef<[u8]>) -> RepoPath {
    RepoPath::new(s.as_ref().to_vec()).unwrap()
}
fn entry(store: &Memory, bytes: &[u8]) -> EntryRef {
    EntryRef {
        mode: EntryMode::File,
        target: store.put(&Object::Blob(bytes.to_vec())).unwrap(),
    }
}
fn empty(store: &Memory) -> ObjectId {
    store
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap()
}
fn put(path: RepoPath, expected: Option<EntryRef>, replacement: EntryRef) -> ChangeOperation {
    ChangeOperation {
        path,
        expected,
        replacement: Some(replacement),
    }
}
fn commit(store: &Memory, parents: Vec<ObjectId>, root: ObjectId, message: &str) -> ObjectId {
    let snapshot = store.put(&Object::Snapshot { root }).unwrap();
    let changes = store
        .put(&Object::ChangeOperations(Changes::new(vec![]).unwrap()))
        .unwrap();
    store
        .put(&Object::NativeCommit(Commit {
            parents,
            snapshot,
            changes,
            author_name: "Test Author".into(),
            author_email: "test@example.invalid".into(),
            timestamp_seconds: 0,
            timezone_minutes: 0,
            message: message.into(),
        }))
        .unwrap()
}

#[test]
fn golden_vectors_from_independent_python_hashlib() {
    let vectors = [
        (
            Object::Blob(vec![]),
            "42f731cb22c7a0f9e42001839becfea9e69acd3d3ef4e763dc74030b06b80b9d",
        ),
        (
            Object::Blob(b"hello\n".to_vec()),
            "54eae9cb4e6dbdb3f8c95599a2616e81b46b83aeb3edf1bd66f22f983dc19e5e",
        ),
        (
            Object::Directory(Directory::new(vec![]).unwrap()),
            "695c01de00de6c2c7c34bd57c3bbda9c4555af03e9bb1f6b5ed42b489246194b",
        ),
    ];
    for (object, hex) in vectors {
        assert_eq!(object.id().unwrap().hex(), hex);
        assert_eq!(
            Object::decode_verified(object.id().unwrap(), &object.encode().unwrap()).unwrap(),
            object
        );
    }
    let blob = Object::Blob(vec![]).encode().unwrap();
    assert_eq!(&blob[..11], b"treeharbor\0");
    assert_eq!(&blob[11..], &[0, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn paths_preserve_non_utf8_but_reject_escapes_and_reserved_names() {
    let raw = b"src/\xff.rs";
    assert_eq!(path(raw).as_bytes(), raw);
    for invalid in [
        &b""[..],
        b"/a",
        b"a//b",
        b"a/../b",
        b"a/./b",
        b"a/",
        b"a\0b",
        b"a\\b",
        b".GIT/config",
        b"x/.treeharbor/state",
    ] {
        assert!(RepoPath::new(invalid.to_vec()).is_err(), "{invalid:?}");
    }
    assert!(Name::new(vec![b'x'; 256]).is_err());
    assert!(RepoPath::new(vec![b'x'; 4097]).is_err());
    assert!(
        RepoPath::new(
            std::iter::repeat_n("a", 129)
                .collect::<Vec<_>>()
                .join("/")
                .into_bytes()
        )
        .is_err()
    );
}

#[test]
fn namespace_and_commit_identity_are_not_interchangeable() {
    let id = Object::Blob(vec![]).id().unwrap();
    let native = RevisionRef::Native(id);
    let git = RevisionRef::Git(GitOid::Sha256(*id.as_bytes()));
    assert_ne!(native, git);
    assert_eq!(native.to_string().parse::<RevisionRef>().unwrap(), native);
    assert_eq!(git.to_string().parse::<RevisionRef>().unwrap(), git);
    assert!(id.hex().parse::<RevisionRef>().is_err());
    assert!(
        format!("sha256:{}", id.hex().to_uppercase())
            .parse::<ObjectId>()
            .is_err()
    );
    let store = Memory::default();
    let root = empty(&store);
    assert_ne!(
        commit(&store, vec![], root, "first"),
        commit(&store, vec![], root, "second")
    );
}

#[test]
fn duplicate_and_non_adjacent_ancestor_operations_are_rejected() {
    let store = Memory::default();
    let value = entry(&store, b"a");
    assert!(
        Changes::new(vec![
            put(path("a"), None, value),
            put(path("a"), None, value)
        ])
        .is_err()
    );
    assert!(
        Changes::new(vec![
            put(path("a"), None, value),
            put(path("a-1"), None, value),
            put(path("a/b"), None, value)
        ])
        .is_err()
    );
}

#[test]
fn one_file_edit_reuses_unaffected_subtree_and_does_not_mutate_base() {
    let store = Memory::default();
    let initial = empty(&store);
    let old = entry(&store, b"old");
    let new = entry(&store, b"new");
    let root = apply_changes(
        &store,
        initial,
        &Changes::new(vec![
            put(path("one/file"), None, old),
            put(path("two/file"), None, old),
        ])
        .unwrap(),
    )
    .unwrap();
    let retained = lookup(&store, root, &path("two")).unwrap();
    let changed = apply_changes(
        &store,
        root,
        &Changes::new(vec![put(path("one/file"), Some(old), new)]).unwrap(),
    )
    .unwrap();
    assert_eq!(lookup(&store, changed, &path("two")).unwrap(), retained);
    assert_eq!(lookup(&store, root, &path("one/file")).unwrap(), Some(old));
    assert_eq!(
        lookup(&store, changed, &path("one/file")).unwrap(),
        Some(new)
    );
    assert_eq!(lookup(&store, initial, &path("one/file")).unwrap(), None);
}

#[test]
fn preconditions_include_absence_and_mode_and_are_checked_before_any_write() {
    let store = Memory::default();
    let initial = empty(&store);
    let value = entry(&store, b"");
    let root = apply_changes(
        &store,
        initial,
        &Changes::new(vec![put(path("exists"), None, value)]).unwrap(),
    )
    .unwrap();
    let before = store.0.lock().unwrap().len();
    let changes = Changes::new(vec![
        put(path("new"), None, value),
        put(path("exists"), None, value),
    ])
    .unwrap();
    assert!(matches!(
        apply_changes(&store, root, &changes),
        Err(Error::PredicateMismatch)
    ));
    assert_eq!(before, store.0.lock().unwrap().len());
    let executable = EntryRef {
        mode: EntryMode::Executable,
        ..value
    };
    assert!(matches!(
        apply_changes(
            &store,
            root,
            &Changes::new(vec![put(path("exists"), Some(executable), value)]).unwrap()
        ),
        Err(Error::PredicateMismatch)
    ));
}

#[test]
fn explicit_delete_prunes_empty_directories_but_sparse_absence_does_not() {
    let store = Memory::default();
    let initial = empty(&store);
    let value = entry(&store, b"content");
    let root = apply_changes(
        &store,
        initial,
        &Changes::new(vec![
            put(path("a/b/c"), None, value),
            put(path("outside"), None, value),
        ])
        .unwrap(),
    )
    .unwrap();
    let changed = apply_changes(
        &store,
        root,
        &Changes::new(vec![ChangeOperation {
            path: path("a/b/c"),
            expected: Some(value),
            replacement: None,
        }])
        .unwrap(),
    )
    .unwrap();
    assert_eq!(lookup(&store, changed, &path("a")).unwrap(), None);
    assert_eq!(
        lookup(&store, changed, &path("outside")).unwrap(),
        Some(value)
    );
}

#[test]
fn graph_verifier_preserves_second_parent_and_rejects_wrong_edge_type() {
    let store = Memory::default();
    let root = empty(&store);
    let first = commit(&store, vec![], root, "first");
    let second = commit(&store, vec![], root, "second");
    let merge = commit(&store, vec![first, second], root, "merge");
    let graph = validate_graph(&store, merge, 100).unwrap();
    assert!(graph.contains(&first) && graph.contains(&second));
    assert!(validate_graph(&store, merge, 1).is_err());
    let wrong = store
        .put(&Object::Snapshot {
            root: entry(&store, b"not a directory").target,
        })
        .unwrap();
    assert!(matches!(
        validate_graph(&store, wrong, 100),
        Err(Error::WrongKind)
    ));
}

#[test]
fn malformed_frames_are_rejected_without_panics() {
    let object = Object::Blob(b"payload".to_vec());
    let bytes = object.encode().unwrap();
    for end in 0..bytes.len() {
        assert!(Object::decode(&bytes[..end]).is_err());
    }
    let mut trailing = bytes.clone();
    trailing.push(0);
    assert!(Object::decode(&trailing).is_err());
    let mut bad = bytes.clone();
    bad[11] = 1;
    assert!(matches!(
        Object::decode(&bad),
        Err(Error::UnsupportedVersion(_))
    ));
    let mut bad = bytes.clone();
    bad[13] = 255;
    assert!(Object::decode(&bad).is_err());
    let mut bad = bytes.clone();
    bad[14..22].copy_from_slice(&u64::MAX.to_be_bytes());
    assert!(Object::decode(&bad).is_err());
    let mut bad = bytes.clone();
    bad[22] ^= 1;
    assert!(Object::decode_verified(object.id().unwrap(), &bad).is_err());
    for seed in 0..512u64 {
        let mut state = seed + 1;
        let mut random = vec![0; (seed as usize) % 257];
        for byte in &mut random {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            *byte = (state >> 32) as u8;
        }
        let _ = Object::decode(&random);
    }
}

#[test]
fn canonical_directories_cannot_have_duplicate_or_unsorted_wire_entries() {
    let store = Memory::default();
    let value = entry(&store, b"blob");
    let a = Entry {
        name: Name::new(b"a".to_vec()).unwrap(),
        value,
    };
    let b = Entry {
        name: Name::new(b"b".to_vec()).unwrap(),
        value,
    };
    assert!(Directory::new(vec![a.clone(), a.clone()]).is_err());
    let d = Object::Directory(Directory::new(vec![b, a]).unwrap());
    let mut wire = d.encode().unwrap();
    // Each single-byte-name entry occupies 4+1+4+32 bytes; swap raw records.
    let start = FRAME_HEADER + 4;
    let mid = start + 41;
    let a = wire[start..mid].to_vec();
    let b = wire[mid..mid + 41].to_vec();
    wire[start..mid].copy_from_slice(&b);
    wire[mid..mid + 41].copy_from_slice(&a);
    assert!(Object::decode(&wire).is_err());
}

#[test]
fn every_object_kind_round_trips_with_binary_names_and_content() {
    let store = Memory::default();
    let root = empty(&store);
    let value = entry(&store, b"\0\xff");
    let changes =
        Object::ChangeOperations(Changes::new(vec![put(path(b"raw/\xff"), None, value)]).unwrap());
    let commit_id = commit(&store, vec![], root, "message\nsecond line");
    for object in [
        changes,
        store.get(commit_id).unwrap(),
        Object::Snapshot { root },
        Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(vec![255]).unwrap(),
                value,
            }])
            .unwrap(),
        ),
    ] {
        assert_eq!(Object::decode(&object.encode().unwrap()).unwrap(), object);
    }
}

#[test]
fn prepared_change_binds_base_operations_and_computed_result() {
    let store = Memory::default();
    let base = store
        .put(&Object::Snapshot {
            root: empty(&store),
        })
        .unwrap();
    let value = entry(&store, b"new file");
    let changes = Changes::new(vec![put(path("src/file"), None, value)]).unwrap();
    let applied = prepare_change(&store, base, &changes).unwrap();
    assert_eq!(applied.base_snapshot(), base);
    assert_eq!(
        store.get(applied.operations()).unwrap(),
        Object::ChangeOperations(changes.clone())
    );
    let Object::Snapshot { root } = store.get(applied.result_snapshot()).unwrap() else {
        panic!()
    };
    assert_eq!(
        lookup(&store, root, &path("src/file")).unwrap(),
        Some(value)
    );
    assert!(matches!(
        prepare_change(&store, applied.result_snapshot(), &changes),
        Err(Error::PredicateMismatch)
    ));
    assert!(matches!(
        prepare_change(&store, value.target, &changes),
        Err(Error::WrongKind)
    ));
}
