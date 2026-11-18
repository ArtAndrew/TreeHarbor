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

#[test]
fn packed_connected_capture_preserves_scoped_git_workspace_data() {
    use treeharbor_git_bridge::{WorkspacePack, WorkspacePackIdentity, WorkspacePackScope};

    let f = Fixture::new();
    let path = f.dir.path().join("packed");
    let scope = WorkspacePackScope {
        tenant: "pilot".into(),
        repository: uuid::Uuid::new_v4(),
        epoch: 1,
    };
    let pack = pack::capture_git_packed(
        GitCapture {
            git: Path::new("/usr/bin/git"),
            bundle: &f.bundle,
            commit: f.commit,
            output: &path,
            limits: Limits::default(),
        },
        Some(&scope),
    )
    .unwrap();
    assert!(path.join("frames.bin").is_file());
    let plain = f.capture("loose", Limits::default()).unwrap();
    assert_eq!(
        (pack.commit(), pack.snapshot()),
        (plain.commit(), plain.snapshot())
    );
    let load = |scope: &WorkspacePackScope| {
        WorkspacePack::load(
            &path,
            f.dir.path(),
            WorkspacePackIdentity {
                scope,
                native: plain.commit(),
                snapshot: plain.snapshot(),
                origin: f.commit,
            },
            Limits::default(),
        )
    };
    assert!(load(&scope).is_ok());
    assert!(load(&WorkspacePackScope { epoch: 2, ..scope }).is_err());
    fs::remove_dir_all(path.join("workspace-git-v1")).unwrap();
    assert_eq!(Pack::open(&path).unwrap().commit(), plain.commit());
}

#[test]
fn packed_cli_matches_reference_frames_and_detects_incomplete_or_changed_data() {
    use std::process::Command;
    use treeharbor_core::ObjectId;

    let f = Fixture::new();
    let output = f.dir.path().join("packed");
    let cli = env!("CARGO_BIN_EXE_treeharbor");
    let revision = RevisionRef::Git(f.commit).to_string();
    let import = Command::new(cli)
        .args(["git-import", "--packed", "--bundle"])
        .arg(&f.bundle)
        .args(["--commit", &revision, "--output"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        import.status.success(),
        "{}",
        String::from_utf8_lossy(&import.stderr)
    );
    let manifest_bytes = fs::read(output.join("manifest.json")).unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&manifest_bytes).unwrap();
    let vector: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/source-pack-v4-vector.json")).unwrap();
    let frame_path = output.join("frames.bin");
    let bytes = fs::read(&frame_path).unwrap();
    assert_eq!(manifest["version"], 4);
    assert_eq!(
        ObjectId::hash_frame(&manifest_bytes).hex(),
        vector["manifest_sha256"]
    );
    let mut fingerprint = b"treeharbor-source-pack\0v4\0".to_vec();
    fingerprint.extend_from_slice(&manifest_bytes);
    assert_eq!(
        ObjectId::hash_frame(&fingerprint).hex(),
        vector["fingerprint"]
    );
    assert_eq!(bytes.len() as u64, vector["file_bytes"].as_u64().unwrap());
    assert_eq!(ObjectId::hash_frame(&bytes).hex(), vector["frames_sha256"]);
    let pack = Pack::open(&output).unwrap();
    let loose = f.capture("loose", Limits::default()).unwrap();
    assert_eq!(pack.commit(), loose.commit());
    assert_eq!(pack.snapshot(), loose.snapshot());
    let restored = f.dir.path().join("restored");
    let export = Command::new(cli)
        .args(["git-export", "--pack"])
        .arg(&output)
        .arg("--output")
        .arg(&restored)
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let exported: serde_json::Value = serde_json::from_slice(&export.stdout).unwrap();
    assert_eq!(
        format!(
            "git:sha1:{}",
            exported["refs"]["refs/heads/main"]["git"].as_str().unwrap()
        ),
        revision
    );

    let first = manifest["objects"][0].as_str().unwrap().parse().unwrap();
    let mut damaged = bytes.clone();
    damaged[4] ^= 1;
    fs::write(&frame_path, &damaged).unwrap();
    assert!(pack.frame(first).is_err());
    assert!(Pack::open(&output).is_err());
    let mut oversized = bytes.clone();
    oversized[..4].copy_from_slice(&u32::MAX.to_be_bytes());
    let mut trailing = bytes.clone();
    trailing.push(0);
    for invalid in [&oversized[..], &bytes[..bytes.len() - 1], &trailing] {
        fs::write(&frame_path, invalid).unwrap();
        assert!(Pack::open(&output).is_err());
    }
    fs::write(&frame_path, &bytes).unwrap();
    let original = output.join("original");
    fs::rename(&frame_path, &original).unwrap();
    symlink(&original, &frame_path).unwrap();
    assert!(Pack::open(&output).is_err());
}
