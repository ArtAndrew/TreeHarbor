use std::{collections::BTreeMap, sync::Mutex};
use treeharbor_core::*;
use treeharbor_history::History;

#[derive(Default)]
struct Store(Mutex<BTreeMap<ObjectId, Object>>);
impl ObjectStore for Store {
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
fn commit(s: &Store, parents: Vec<ObjectId>, message: &str) -> ObjectId {
    let root = s
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap();
    let snapshot = s.put(&Object::Snapshot { root }).unwrap();
    let changes = s
        .put(&Object::ChangeOperations(Changes::new(vec![]).unwrap()))
        .unwrap();
    s.put(&Object::NativeCommit(Commit {
        git_origin: None,
        parents,
        snapshot,
        changes,
        author_name: "Test".into(),
        author_email: "test@example.invalid".into(),
        timestamp_seconds: 0,
        timezone_minutes: 0,
        message: message.into(),
    }))
    .unwrap()
}

#[test]
fn criss_cross_keeps_both_best_bases_and_reaches_both_parents() {
    let s = Store::default();
    let root = commit(&s, vec![], "root");
    let a = commit(&s, vec![root], "a");
    let b = commit(&s, vec![root], "b");
    let left = commit(&s, vec![a, b], "left");
    let right = commit(&s, vec![b, a], "right");
    let h = History::new(&s, 100);
    let mut expected = vec![a, b];
    expected.sort();
    assert_eq!(h.merge_bases(left, right).unwrap(), expected);
    assert!(h.is_ancestor(b, left).unwrap());
    assert_eq!(h.ancestors(left).unwrap().len(), 4);
    assert_eq!(
        h.first_parent_log(left, 100)
            .unwrap()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>(),
        vec![left, a, root]
    );
}

#[test]
fn disconnected_and_fast_forward_histories_are_distinct() {
    let s = Store::default();
    let root = commit(&s, vec![], "root");
    let other = commit(&s, vec![], "other");
    let tip = commit(&s, vec![root], "tip");
    let h = History::new(&s, 100);
    assert_eq!(h.merge_bases(root, tip).unwrap(), vec![root]);
    assert!(h.merge_bases(root, other).unwrap().is_empty());
    assert!(!h.is_ancestor(other, tip).unwrap());
    assert!(h.root(tip).is_ok());
}

#[test]
fn unknown_history_and_exhausted_budget_fail_instead_of_claiming_no_ancestor() {
    let s = Store::default();
    let root = commit(&s, vec![], "root");
    let tip = commit(&s, vec![root], "tip");
    assert!(matches!(
        History::new(&s, 1).ancestors(tip),
        Err(Error::TooLarge)
    ));
    assert!(
        History::new(&s, 100)
            .is_ancestor(ObjectId::from_bytes([255; 32]), tip)
            .is_err()
    );
    let leaf = s.put(&Object::Blob(vec![])).unwrap();
    assert!(History::new(&s, 100).commit(leaf).is_err());
}
