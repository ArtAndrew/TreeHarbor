#![cfg(unix)]
#[path = "support/git.rs"]
mod reference;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
};
use treeharbor_core::{GitOid, Object, RevisionRef};
use treeharbor_git_bridge::Limits;
use treeharbor_service::pack::{self, GitCapture, Pack};

struct Fixture {
    dir: tempfile::TempDir,
    source: PathBuf,
    bundle: PathBuf,
    commit: GitOid,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let source = dir.path().join("source.git");
        reference::git(
            dir.path(),
            &[
                "init",
                "--bare",
                "--object-format=sha1",
                "--template=",
                source.to_str().unwrap(),
            ],
            b"",
        );
        reference::hash(&source, "tree", b"");
        let vector = include_str!("../../core/tests/fixtures/git-origin-v2.txt")
            .lines()
            .nth(4)
            .unwrap();
        let (id, raw) = vector.split_once(' ').unwrap();
        let bytes: Vec<u8> = (0..raw.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).unwrap())
            .collect();
        let Object::Blob(raw) =
            Object::decode_verified(format!("sha256:{id}").parse().unwrap(), &bytes).unwrap()
        else {
            panic!()
        };
        let oid = reference::hash(&source, "commit", &raw);
        let bundle = dir.path().join("source.bundle");
        reference::bundle(&source, &bundle, &oid);
        let RevisionRef::Git(commit) = format!("git:sha1:{oid}").parse().unwrap() else {
            panic!()
        };
        Self {
            dir,
            source,
            bundle,
            commit,
        }
    }
    fn capture(&self, name: &str, limits: Limits) -> pack::Result<Pack> {
        pack::capture_git(GitCapture {
            git: Path::new("/usr/bin/git"),
            bundle: &self.bundle,
            commit: self.commit,
            output: &self.dir.path().join(name),
            limits,
        })
    }
}

#[test]
fn full_history_pack_matches_independent_vector_and_rejects_altered_manifests() {
    let f = Fixture::new();
    let pack = f.capture("pack", Limits::default()).unwrap();
    let path = f.dir.path().join("pack/manifest.json");
    let raw = fs::read(&path).unwrap();
    let mut fingerprint = b"treeharbor-source-pack\0v2\0".to_vec();
    fingerprint.extend_from_slice(&raw);
    assert_eq!(
        treeharbor_core::ObjectId::hash_frame(&fingerprint).to_string(),
        "sha256:f4b6ed668e538f38a2e1b854eba7223495fbe0884f8e201670d77236b0c7cb14"
    );
    assert_eq!(
        raw,
        include_bytes!("fixtures/source-pack-v2.json")
            .strip_suffix(b"\n")
            .unwrap()
    );
    assert_eq!(pack.object_count(), 5);
    assert_eq!(pack.filesystem_entries(), 0);
    fs::remove_dir_all(&f.source).unwrap();
    fs::remove_file(&f.bundle).unwrap();
    assert_eq!(
        Pack::open(path.parent().unwrap()).unwrap().commit(),
        pack.commit()
    );
    let original: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    let mut variants = Vec::new();
    for (field, value) in [
        ("selection", serde_json::json!("sparse")),
        ("selection_hex", serde_json::json!([])),
        ("version", serde_json::json!(1)),
        (
            "source_revision",
            serde_json::json!(format!("git:sha1:{}", "0".repeat(40))),
        ),
        ("encoded_bytes", serde_json::json!(0)),
    ] {
        let mut changed = original.clone();
        changed[field] = value;
        variants.push(changed);
    }
    let mut changed = original.clone();
    changed["objects"].as_array_mut().unwrap().remove(0);
    variants.push(changed);
    let mut changed = original.clone();
    changed["objects"].as_array_mut().unwrap().swap(0, 4);
    variants.push(changed);
    for changed in variants {
        fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(Pack::open(path.parent().unwrap()).is_err());
    }
    fs::write(&path, &raw).unwrap();
    assert_eq!(
        Pack::open(path.parent().unwrap()).unwrap().commit(),
        pack.commit()
    );
}

#[test]
fn failed_capture_and_concurrent_publish_never_replace_an_existing_pack() {
    let f = Fixture::new();
    let limits = Limits {
        objects: 1,
        ..Limits::default()
    };
    assert!(f.capture("failed", limits).is_err());
    assert!(!f.dir.path().join("failed").exists());
    assert!(fs::read_dir(f.dir.path()).unwrap().all(|e| {
        !e.unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".treeharbor-")
    }));
    let results = std::thread::scope(|scope| {
        let a = scope.spawn(|| f.capture("same", Limits::default()));
        let b = scope.spawn(|| f.capture("same", Limits::default()));
        [a.join().unwrap(), b.join().unwrap()]
    });
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    let first = Pack::open(&f.dir.path().join("same")).unwrap().commit();
    assert!(f.capture("same", Limits::default()).is_err());
    assert_eq!(
        Pack::open(&f.dir.path().join("same")).unwrap().commit(),
        first
    );
    symlink(f.dir.path().join("same"), f.dir.path().join("alias")).unwrap();
    assert!(f.capture("alias", Limits::default()).is_err());
    fs::set_permissions(f.dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(f.capture("exposed", Limits::default()).is_err());
    assert!(!f.dir.path().join("exposed").exists());
}
