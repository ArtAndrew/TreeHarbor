use std::{
    fs,
    sync::{Arc, Barrier},
};
use treeharbor_core::{Error, Object, ObjectStore};
use treeharbor_storage::{LocalStore, TenantId};

#[test]
fn object_survives_store_restart_and_tenants_do_not_share_access() {
    let temp = private_tempdir();
    let tenant = TenantId::new("one").unwrap();
    let other = TenantId::new("two").unwrap();
    let store = LocalStore::open(temp.path(), &tenant).unwrap();
    let object = Object::Blob(b"durable bytes".to_vec());
    let id = store.put(&object).unwrap();
    drop(store);
    assert_eq!(
        LocalStore::open(temp.path(), &tenant)
            .unwrap()
            .get(id)
            .unwrap(),
        object
    );
    assert!(matches!(
        LocalStore::open(temp.path(), &other).unwrap().get(id),
        Err(Error::MissingObject(_))
    ));
}

#[test]
fn concurrent_same_object_uploads_are_idempotent() {
    let temp = private_tempdir();
    let store = Arc::new(LocalStore::open(temp.path(), &TenantId::new("test").unwrap()).unwrap());
    let barrier = Arc::new(Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let store = store.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.put(&Object::Blob(vec![123; 8192])).unwrap()
            })
        })
        .collect();
    let ids: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert!(ids.iter().all(|id| id == &ids[0]));
    assert_eq!(store.get(ids[0]).unwrap(), Object::Blob(vec![123; 8192]));
    assert_eq!(fs::read_dir(store.staging_directory()).unwrap().count(), 0);
}

#[test]
fn corrupt_existing_objects_fail_closed_and_are_not_overwritten() {
    let temp = private_tempdir();
    let tenant = TenantId::new("test").unwrap();
    let store = LocalStore::open(temp.path(), &tenant).unwrap();
    let object = Object::Blob(b"truth".to_vec());
    let id = store.put(&object).unwrap();
    let hex = id.hex();
    let file = temp
        .path()
        .join("objects/test")
        .join(&hex[..2])
        .join(&hex[2..]);
    fs::write(&file, b"corrupt").unwrap();
    assert!(store.get(id).is_err());
    assert!(store.contains(id).is_err());
    assert!(store.put(&object).is_err());
    assert_eq!(fs::read(&file).unwrap(), b"corrupt");
}

#[test]
fn tenant_paths_cannot_escape_the_service_namespace() {
    for invalid in ["", "../other", "a/b", "a\\b", "UPPER", "a\0b", ".", ".."] {
        assert!(TenantId::new(invalid).is_err());
    }
    assert!(TenantId::new("a".repeat(65)).is_err());
}

#[cfg(unix)]
#[test]
fn storage_rejects_symlinks_and_public_directories() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let temp = private_tempdir();
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = temp.path().join("link");
    symlink(&target, &link).unwrap();
    let tenant = TenantId::new("test").unwrap();
    assert!(LocalStore::open(&link, &tenant).is_err());
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(LocalStore::open(&target, &tenant).is_err());
}

#[test]
fn abandoned_upload_does_not_become_a_readable_object() {
    let temp = private_tempdir();
    let tenant = TenantId::new("test").unwrap();
    let store = LocalStore::open(temp.path(), &tenant).unwrap();
    let object = Object::Blob(b"unpublished".to_vec());
    fs::write(
        store.staging_directory().join("interrupted-upload"),
        object.encode().unwrap(),
    )
    .unwrap();
    drop(store);
    let store = LocalStore::open(temp.path(), &tenant).unwrap();
    assert!(!store.contains(object.id().unwrap()).unwrap());
    assert_eq!(store.put(&object).unwrap(), object.id().unwrap());
}

fn private_tempdir() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    temp
}
