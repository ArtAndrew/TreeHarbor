use treeharbor_core::{Directory, Entry, EntryMode, EntryRef, Name, Object};
use treeharbor_storage::{ManagedStore, PreparedObject, TenantId};

fn root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}
#[test]
fn old_generation_cleanup_cannot_remove_a_new_generation() {
    let root = root();
    let store = ManagedStore::open(root.path(), TenantId::new("one").unwrap(), "local-1").unwrap();
    let object = Object::Blob(b"same bytes".to_vec());
    let prepared = PreparedObject::new(&object).unwrap();
    let old = store.put(1, &prepared).unwrap();
    let new = store.put(2, &prepared).unwrap();
    assert_eq!(old.id(), new.id());
    store.remove_retired_generation(1, old.id()).unwrap();
    store.remove_retired_generation(1, old.id()).unwrap();
    assert!(store.get(1, old.id()).is_err());
    assert_eq!(store.get(2, new.id()).unwrap(), object);
}
#[test]
fn receipts_bind_durable_bytes_type_edges_tenant_and_backend() {
    let root = root();
    let store = ManagedStore::open(root.path(), TenantId::new("one").unwrap(), "local-1").unwrap();
    let blob = PreparedObject::new(&Object::Blob(b"data".to_vec())).unwrap();
    let child = store.put(1, &blob).unwrap();
    let dir = Object::Directory(
        Directory::new(vec![Entry {
            name: Name::new(b"file".to_vec()).unwrap(),
            value: EntryRef {
                mode: EntryMode::File,
                target: child.id(),
            },
        }])
        .unwrap(),
    );
    let prepared = PreparedObject::new(&dir).unwrap();
    let receipt = store.put(4, &prepared).unwrap();
    assert_eq!(receipt.tenant(), "one");
    assert_eq!(receipt.backend(), "local-1");
    assert_eq!(receipt.generation(), 4);
    assert_eq!(receipt.kind(), 2);
    assert_eq!(receipt.length(), dir.encode().unwrap().len() as i64);
    assert_eq!(receipt.references(), &[(child.id(), 1)]);
    let other = ManagedStore::open(root.path(), TenantId::new("two").unwrap(), "local-1").unwrap();
    assert!(other.get(4, receipt.id()).is_err());
    assert!(store.put(0, &prepared).is_err());
}
