#![allow(dead_code)]
use std::{cell::RefCell, collections::BTreeMap};
use treeharbor_core::*;

#[derive(Default)]
pub struct Store {
    pub objects: RefCell<BTreeMap<ObjectId, Object>>,
    pub reads: RefCell<Vec<ObjectId>>,
    pub writes: RefCell<Vec<ObjectId>>,
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

pub type Files = BTreeMap<Vec<u8>, (EntryMode, Vec<u8>)>;
pub fn files(items: &[(&[u8], EntryMode, &[u8])]) -> Files {
    items
        .iter()
        .map(|(p, m, b)| (p.to_vec(), (*m, b.to_vec())))
        .collect()
}
pub fn plain(items: &[(&str, &str)]) -> Files {
    items
        .iter()
        .map(|(p, b)| {
            (
                p.as_bytes().to_vec(),
                (EntryMode::File, b.as_bytes().to_vec()),
            )
        })
        .collect()
}

pub fn snapshot(store: &impl ObjectStore, files: &Files) -> ObjectId {
    fn dir(store: &impl ObjectStore, files: &Files) -> ObjectId {
        let mut entries = Vec::new();
        let mut children: BTreeMap<Vec<u8>, Files> = BTreeMap::new();
        for (path, (mode, bytes)) in files {
            if let Some(slash) = path.iter().position(|b| *b == b'/') {
                children
                    .entry(path[..slash].to_vec())
                    .or_default()
                    .insert(path[slash + 1..].to_vec(), (*mode, bytes.clone()));
            } else {
                let target = store.put(&Object::Blob(bytes.clone())).unwrap();
                entries.push(Entry {
                    name: Name::new(path.clone()).unwrap(),
                    value: EntryRef {
                        mode: *mode,
                        target,
                    },
                });
            }
        }
        for (name, child) in children {
            entries.push(Entry {
                name: Name::new(name).unwrap(),
                value: EntryRef {
                    mode: EntryMode::Directory,
                    target: dir(store, &child),
                },
            });
        }
        store
            .put(&Object::Directory(Directory::new(entries).unwrap()))
            .unwrap()
    }
    store
        .put(&Object::Snapshot {
            root: dir(store, files),
        })
        .unwrap()
}

pub fn contents(store: &impl ObjectStore, snapshot: ObjectId) -> Files {
    let Object::Snapshot { root } = store.get(snapshot).unwrap() else {
        panic!()
    };
    let mut out = Files::new();
    let mut pending = vec![(Vec::new(), root)];
    while let Some((prefix, id)) = pending.pop() {
        let Object::Directory(dir) = store.get(id).unwrap() else {
            panic!()
        };
        for entry in dir.entries() {
            let mut path = prefix.clone();
            path.extend_from_slice(entry.name.as_bytes());
            if entry.value.mode == EntryMode::Directory {
                path.push(b'/');
                pending.push((path, entry.value.target));
            } else {
                let Object::Blob(bytes) = store.get(entry.value.target).unwrap() else {
                    panic!()
                };
                assert!(out.insert(path, (entry.value.mode, bytes)).is_none());
            }
        }
    }
    out
}

pub fn commit(
    store: &impl ObjectStore,
    snapshot: ObjectId,
    parents: Vec<ObjectId>,
    message: &str,
) -> ObjectId {
    let changes = store
        .put(&Object::ChangeOperations(Changes::new(vec![]).unwrap()))
        .unwrap();
    store
        .put(&Object::NativeCommit(Commit {
            parents,
            snapshot,
            changes,
            author_name: "Merge fixture".into(),
            author_email: "fixture@example.invalid".into(),
            timestamp_seconds: 1,
            timezone_minutes: 0,
            message: message.into(),
        }))
        .unwrap()
}
