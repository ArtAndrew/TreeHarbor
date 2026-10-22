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
    let ordered: Vec<_> = h
        .topological_log(left)
        .unwrap()
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    assert_eq!(ordered, vec![left, expected[0], expected[1], root]);
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
    assert!(History::new(&s, 1).topological_log(tip).is_err());
    assert!(
        History::new(&s, 100)
            .topological_log(ObjectId::from_bytes([255; 32]))
            .is_err()
    );
}

#[test]
fn topological_history_matches_independent_dag_matrix() {
    for seed in 1..=64u32 {
        let s = Store::default();
        let mut ids = Vec::new();
        let mut reach = [[false; 16]; 16];
        let mut state = seed;
        for i in 0..16 {
            reach[i][i] = true;
            let mut parents = Vec::new();
            for j in 0..i {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                if state.is_multiple_of(5) {
                    parents.push(ids[j]);
                    let inherited = reach[j];
                    for (slot, ancestor) in reach[i].iter_mut().zip(inherited) {
                        *slot |= ancestor;
                    }
                }
            }
            ids.push(commit(&s, parents, &format!("{seed}:{i}")));
        }
        let h = History::new(&s, 16);
        for i in 0..16 {
            let log = h.topological_log(ids[i]).unwrap();
            let actual: std::collections::BTreeSet<_> = log.iter().map(|(id, _)| *id).collect();
            let expected = ids
                .iter()
                .enumerate()
                .filter_map(|(j, id)| reach[i][j].then_some(*id))
                .collect();
            assert_eq!(actual, expected);
            assert_eq!(actual.len(), log.len());
            let positions: BTreeMap<_, _> = log
                .iter()
                .enumerate()
                .map(|(n, (id, _))| (*id, n))
                .collect();
            for (id, value) in &log {
                for parent in &value.parents {
                    assert!(positions[id] < positions[parent]);
                }
            }
            assert_eq!(log, h.topological_log(ids[i]).unwrap());
        }
    }
}
