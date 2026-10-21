use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use treeharbor_node::Node;
use uuid::Uuid;

fn parent() -> tempfile::TempDir {
    tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap()
}
#[test]
fn initialize_open_and_process_exclusion_never_repair_or_replace_storage_identity() {
    let parent = parent();
    let root = parent.path().join("node");
    let node = Node::create(&root, "tenant", "node-1", 2).unwrap();
    let storage = node.identity().storage();
    assert!(Node::create(&root, "tenant", "node-1", 2).is_err());
    assert!(Node::open(&root, "tenant", "node-1", storage, 2).is_err());
    drop(node);
    assert!(Node::open(&root, "tenant", "node-1", Uuid::new_v4(), 2).is_err());
    assert!(Node::open(&root, "other", "node-1", storage, 2).is_err());
    assert!(Node::open(&root, "tenant", "other", storage, 2).is_err());
    drop(Node::open(&root, "tenant", "node-1", storage, 2).unwrap());
    fs::remove_file(root.join("identity.json")).unwrap();
    assert!(Node::open(&root, "tenant", "node-1", storage, 2).is_err());
    assert!(!root.join("identity.json").exists());
}
#[test]
fn copied_or_linked_roots_and_exposed_identity_files_are_rejected() {
    let parent = parent();
    let root = parent.path().join("node");
    let node = Node::create(&root, "tenant", "node-1", 1).unwrap();
    let storage = node.identity().storage();
    drop(node);
    symlink(&root, parent.path().join("linked")).unwrap();
    assert!(
        Node::open(
            &parent.path().join("linked"),
            "tenant",
            "node-1",
            storage,
            1
        )
        .is_err()
    );
    let other = parent.path().join("other");
    drop(Node::create(&other, "tenant", "node-1", 1).unwrap());
    fs::copy(root.join("identity.json"), other.join("identity.json")).unwrap();
    assert!(Node::open(&other, "tenant", "node-1", storage, 1).is_err());
    fs::set_permissions(
        root.join("identity.json"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    assert!(Node::open(&root, "tenant", "node-1", storage, 1).is_err());
    fs::set_permissions(
        root.join("identity.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::hard_link(root.join("node.lock"), root.join("alias")).unwrap();
    assert!(Node::open(&root, "tenant", "node-1", storage, 1).is_err());
}
