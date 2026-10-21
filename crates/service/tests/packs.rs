#![cfg(unix)]
use std::{
    ffi::OsStr,
    fs,
    os::unix::{
        ffi::OsStrExt,
        fs::{PermissionsExt, symlink},
    },
    path::Path,
};
use treeharbor_core::{EntryMode, Object, ObjectId, ObjectStore, RepoPath};
use treeharbor_service::pack::{self, Capture, Limits, Pack};
use treeharbor_storage::{LocalStore, TenantId};

struct Fixture {
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        fs::create_dir_all(dir.path().join("source/src")).unwrap();
        fs::write(dir.path().join("source/src/main.rs"), b"fn main() {}\n").unwrap();
        Self { dir }
    }
    fn capture(&self, name: &str, paths: &[&[u8]], limits: Limits) -> pack::Result<Pack> {
        pack::capture(Capture {
            directory: &self.dir.path().join("source"),
            output: &self.dir.path().join(name),
            paths: paths
                .iter()
                .map(|p| RepoPath::new(p.to_vec()).unwrap())
                .collect(),
            author_name: "Test".into(),
            author_email: "test@example.invalid".into(),
            message: "capture".into(),
            timestamp_seconds: 0,
            limits,
        })
    }
    fn good(&self, name: &str) -> Pack {
        self.capture(name, &[b"src"], Limits::default()).unwrap()
    }
}
fn object(pack: &Pack, id: ObjectId) -> Object {
    Object::decode_verified(id, &pack.frame(id).unwrap()).unwrap()
}
fn root(pack: &Pack) -> treeharbor_core::Directory {
    let Object::Snapshot { root } = object(pack, pack.snapshot()) else {
        panic!()
    };
    let Object::Directory(dir) = object(pack, root) else {
        panic!()
    };
    dir
}
fn rewrite(path: &Path, value: &serde_json::Value) {
    fs::write(path, serde_json::to_vec(value).unwrap()).unwrap();
}

#[test]
fn selected_source_matches_independent_golden_and_survives_source_removal() {
    let f = Fixture::new();
    // Unselected secrets and repository metadata must not enter the graph.
    fs::write(f.dir.path().join("source/secret"), b"never transmit").unwrap();
    fs::create_dir(f.dir.path().join("source/.git")).unwrap();
    let pack = f.good("pack");
    assert_eq!(
        pack.commit().to_string(),
        "sha256:edd78b7b781955d45661c7d81c46ac4939ec0cb6fa9a80dc1fd28b0e53b5bf3f"
    );
    assert_eq!(
        pack.snapshot().to_string(),
        "sha256:8e64bc29cab70eaae9816b3f7d26b3e08efccf2376da28118ef474feb4078bd3"
    );
    assert_eq!(pack.object_count(), 6);
    assert_eq!(pack.encoded_bytes(), 400);
    assert_eq!(pack.filesystem_entries(), 2);
    // This vector was calculated separately using Python struct/hashlib from
    // the canonical frame specification, not emitted by the Rust encoder.
    let raw = fs::read(f.dir.path().join("pack/manifest.json")).unwrap();
    assert_eq!(
        raw,
        include_bytes!("fixtures/source-pack-v1.json")
            .strip_suffix(b"\n")
            .unwrap()
    );
    let mut fingerprint = b"treeharbor-source-pack\0v1\0".to_vec();
    fingerprint.extend_from_slice(&raw);
    assert_eq!(
        ObjectId::hash_frame(&fingerprint).to_string(),
        "sha256:cb4a7d0f8f09dab7215cf4c3624c59cda21a61308b103fa275f73e9f0008f1d2"
    );
    let again = f
        .capture("same", &[b"src/main.rs", b"src"], Limits::default())
        .unwrap();
    assert_eq!(again.commit(), pack.commit());
    fs::remove_dir_all(f.dir.path().join("source")).unwrap();
    let recovered = Pack::open(&f.dir.path().join("pack")).unwrap();
    assert_eq!(recovered.commit(), pack.commit());
    assert_eq!(root(&recovered).entries().len(), 1);
    assert_eq!(root(&recovered).entries()[0].name.as_bytes(), b"src");
}

#[test]
fn raw_names_executable_files_and_links_are_preserved_without_dereferencing() {
    let f = Fixture::new();
    let source = f.dir.path().join("source/src");
    // APFS rejects non-UTF-8 names; Linux exercises the binary-path case.
    let raw_name: &[u8] = if cfg!(target_os = "macos") {
        b"run-me"
    } else {
        b"raw-\xff"
    };
    let raw = source.join(OsStr::from_bytes(raw_name));
    fs::write(&raw, b"#!/bin/sh\n").unwrap();
    fs::set_permissions(&raw, fs::Permissions::from_mode(0o755)).unwrap();
    symlink(OsStr::from_bytes(raw_name), source.join("link")).unwrap();
    symlink("missing", source.join("dangling")).unwrap();
    let pack = f.good("pack");
    let Object::Directory(src) = object(&pack, root(&pack).entries()[0].value.target) else {
        panic!()
    };
    assert_eq!(src.entries().len(), 4);
    let entry = src
        .entries()
        .iter()
        .find(|e| e.name.as_bytes() == raw_name)
        .unwrap();
    assert_eq!(entry.value.mode, EntryMode::Executable);
    assert_eq!(
        object(&pack, entry.value.target),
        Object::Blob(b"#!/bin/sh\n".to_vec())
    );
    let entry = src
        .entries()
        .iter()
        .find(|e| e.name.as_bytes() == b"link")
        .unwrap();
    assert_eq!(entry.value.mode, EntryMode::Symlink);
    assert_eq!(
        object(&pack, entry.value.target),
        Object::Blob(raw_name.to_vec())
    );
}

#[test]
fn unsafe_inputs_missing_selection_and_limits_never_publish_a_manifest() {
    let f = Fixture::new();
    symlink(f.dir.path(), f.dir.path().join("source/alias")).unwrap();
    assert!(
        f.capture("traversal", &[b"alias/source/src"], Limits::default())
            .is_err()
    );
    assert!(f.capture("link", &[b"alias"], Limits::default()).is_err());
    symlink("../../secret", f.dir.path().join("source/escape")).unwrap();
    assert!(
        f.capture("escape", &[b"escape"], Limits::default())
            .is_err()
    );
    assert!(
        f.capture("missing", &[b"absent"], Limits::default())
            .is_err()
    );
    assert!(
        f.capture(
            "objects",
            &[b"src"],
            Limits {
                objects: 2,
                ..Limits::default()
            }
        )
        .is_err()
    );
    assert!(
        f.capture(
            "bytes",
            &[b"src"],
            Limits {
                bytes: 30,
                ..Limits::default()
            }
        )
        .is_err()
    );
    let huge = fs::File::create(f.dir.path().join("source/huge")).unwrap();
    huge.set_len(treeharbor_core::MAX_PAYLOAD as u64 + 1)
        .unwrap();
    assert!(f.capture("huge", &[b"huge"], Limits::default()).is_err());
    // rustix has no mkfifoat on macOS. This only constructs a test input;
    // source capture itself never starts a subprocess.
    assert!(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(f.dir.path().join("source/fifo"))
            .status()
            .unwrap()
            .success()
    );
    assert!(f.capture("fifo", &[b"fifo"], Limits::default()).is_err());
    for (index, ending) in ["\n", "\r\n"].iter().enumerate() {
        fs::write(
            f.dir.path().join("source/lfs"),
            format!("version https://git-lfs.github.com/spec/v1{ending}oid sha256:abc{ending}"),
        )
        .unwrap();
        assert!(
            f.capture(&format!("lfs-{index}"), &[b"lfs"], Limits::default())
                .is_err()
        );
    }
    fs::create_dir(f.dir.path().join("source/src/.git")).unwrap();
    assert!(
        f.capture("nested-git", &[b"src"], Limits::default())
            .is_err()
    );
    for entry in fs::read_dir(f.dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.file_name().unwrap() != "source" {
            assert!(!path.join("manifest.json").exists(), "{}", path.display());
            assert!(Pack::open(&path).is_err());
        }
    }
}

#[test]
fn output_is_new_private_and_outside_source() {
    let f = Fixture::new();
    fs::set_permissions(
        f.dir.path().join("source"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    assert!(matches!(
        f.capture("source/pack", &[b"src"], Limits::default()),
        Err(pack::Error::Output)
    ));
    let first = f.good("pack");
    assert!(f.capture("pack", &[b"src"], Limits::default()).is_err());
    assert_eq!(
        Pack::open(&f.dir.path().join("pack")).unwrap().commit(),
        first.commit()
    );
    fs::set_permissions(f.dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        f.capture("exposed", &[b"src"], Limits::default()),
        Err(pack::Error::Output)
    ));
    assert!(!f.dir.path().join("exposed").exists());
}

#[test]
fn altered_manifests_cannot_add_reorder_or_omit_objects() {
    let f = Fixture::new();
    let pack = f.good("pack");
    let path = f.dir.path().join("pack/manifest.json");
    let original: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut variants = Vec::new();
    let mut value = original.clone();
    value["objects"].as_array_mut().unwrap().swap(0, 1);
    variants.push(value);
    let mut value = original.clone();
    value["objects"].as_array_mut().unwrap().remove(0);
    variants.push(value);
    let mut value = original.clone();
    value["selection_hex"] = serde_json::json!(["737263", "737263"]);
    variants.push(value);
    let mut value = original.clone();
    value["encoded_bytes"] = serde_json::json!(401);
    variants.push(value);
    let mut value = original.clone();
    value["version"] = serde_json::json!(2);
    variants.push(value);
    let store = LocalStore::open(
        f.dir.path().join("pack"),
        &TenantId::new("native-pack").unwrap(),
    )
    .unwrap();
    let extra = Object::Blob(b"unrelated secret".to_vec());
    let id = store.put(&extra).unwrap();
    let mut value = original.clone();
    value["objects"]
        .as_array_mut()
        .unwrap()
        .insert(0, serde_json::json!(id.to_string()));
    value["encoded_bytes"] =
        serde_json::json!(pack.encoded_bytes() + extra.encode().unwrap().len() as u64);
    variants.push(value);
    for variant in variants {
        rewrite(&path, &variant);
        assert!(Pack::open(&f.dir.path().join("pack")).is_err());
    }
    rewrite(&path, &original);
    assert_eq!(
        Pack::open(&f.dir.path().join("pack")).unwrap().commit(),
        pack.commit()
    );
    // A manifest is insufficient: verify actual content and missing disk frames.
    let hex = pack.commit().hex();
    let frame = f
        .dir
        .path()
        .join("pack/objects/native-pack")
        .join(&hex[..2])
        .join(&hex[2..]);
    fs::write(&frame, b"corrupt").unwrap();
    assert!(Pack::open(&f.dir.path().join("pack")).is_err());
    fs::remove_file(&frame).unwrap();
    assert!(Pack::open(&f.dir.path().join("pack")).is_err());
}
