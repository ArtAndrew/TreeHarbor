use super::*;
use crate::{Selection, filesystem::Manifest as Baseline};
use std::{
    os::unix::{ffi::OsStrExt, fs::symlink},
    sync::Arc,
};
use treeharbor_core::{Directory, Entry, Name, SourceProfile, apply_changes, lookup};
use uuid::Uuid;
fn p(path: &str) -> RepoPath {
    RepoPath::new(path.as_bytes()).unwrap()
}
fn selection(replace: &[&str], delete: &[&str], exclude: &[&str]) -> CaptureSelection {
    CaptureSelection::new(
        replace.iter().map(|s| p(s)).collect(),
        delete.iter().map(|s| p(s)).collect(),
        exclude.iter().map(|s| p(s)).collect(),
    )
    .unwrap()
}
fn file(store: &LocalStore, data: &[u8]) -> EntryRef {
    EntryRef {
        mode: EntryMode::File,
        target: store.put(&Object::Blob(data.to_vec())).unwrap(),
    }
}
fn directory(store: &LocalStore, entries: &[(&str, EntryRef)]) -> EntryRef {
    EntryRef {
        mode: EntryMode::Directory,
        target: store
            .put(&Object::Directory(
                Directory::new(
                    entries
                        .iter()
                        .map(|(name, value)| Entry {
                            name: Name::new(name.as_bytes()).unwrap(),
                            value: *value,
                        })
                        .collect(),
                )
                .unwrap(),
            ))
            .unwrap(),
    }
}
struct Fixture {
    _temp: tempfile::TempDir,
    tree: MaterializedTree,
    source: LocalStore,
    root: ObjectId,
    scope: Scope,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::Builder::new()
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir()
            .unwrap();
        let source =
            LocalStore::open(temp.path().join("source"), &TenantId::new("test").unwrap()).unwrap();
        let before = file(&source, b"before\n");
        let secret = file(&source, b"old secret\n");
        let cache = file(&source, b"old cache\n");
        let outside = file(&source, b"outside\n");
        let obsolete = file(&source, b"obsolete\n");
        let build = directory(&source, &[("cache", cache)]);
        let empty = directory(&source, &[]);
        let src = directory(
            &source,
            &[
                ("file", before),
                (".env", secret),
                ("build", build),
                ("empty", empty),
            ],
        );
        let root = directory(
            &source,
            &[("src", src), ("outside", outside), ("obsolete", obsolete)],
        )
        .target;
        let snapshot = source.put(&Object::Snapshot { root }).unwrap();
        let workspace = temp.path().join("workspace");
        fs::create_dir(&workspace).unwrap();
        fs::set_permissions(&workspace, fs::Permissions::from_mode(0o700)).unwrap();
        let files = workspace.join("files");
        fs::create_dir(&files).unwrap();
        fs::create_dir(files.join("src")).unwrap();
        fs::create_dir(files.join("src/build")).unwrap();
        fs::create_dir(files.join("src/empty")).unwrap();
        let mut entries = BTreeMap::new();
        for (path, value) in [
            ("src", src),
            ("src/file", before),
            ("src/.env", secret),
            ("src/build", build),
            ("src/build/cache", cache),
            ("src/empty", empty),
            ("outside", outside),
            ("obsolete", obsolete),
        ] {
            entries.insert(p(path), value);
            if value.mode == EntryMode::File {
                let Object::Blob(data) = source.get(value.target).unwrap() else {
                    panic!()
                };
                fs::write(files.join(path), data).unwrap();
            }
        }
        let scope = Scope::new(
            "test",
            Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap(),
        )
        .unwrap();
        let tree = MaterializedTree {
            root: workspace,
            manifest: Arc::new(Baseline {
                tenant: scope.tenant().into(),
                repository: scope.repository().to_string(),
                snapshot,
                selection: Selection::full(),
                entries,
                preserved_local_state: false,
            }),
            stats: None,
        };
        Self {
            _temp: temp,
            tree,
            source,
            root,
            scope,
        }
    }
    fn publication(&self, name: &str) -> Publication {
        Publication::new(self._temp.path().join(name), [9; 32], Arc::new(()))
    }
    fn capture(&self, spec: &CaptureSelection) -> CaptureArtifact {
        self.tree
            .capture_selected(spec, &self.publication("capture"), Limits::default())
            .unwrap()
    }
    fn apply(&self, artifact: &CaptureArtifact) -> ObjectId {
        for id in artifact.objects() {
            self.source.put(&artifact.object(*id).unwrap()).unwrap();
        }
        apply_changes(&self.source, self.root, artifact.operations()).unwrap()
    }
    fn bytes(&self, root: ObjectId, path: &str) -> Vec<u8> {
        let value = lookup(&self.source, root, &p(path)).unwrap().unwrap();
        let Object::Blob(data) = self.source.get(value.target).unwrap() else {
            panic!()
        };
        data
    }
    fn expected<'a>(&'a self, spec: &'a CaptureSelection) -> CaptureExpectation<'a> {
        CaptureExpectation {
            scope: &self.scope,
            snapshot: self.tree.snapshot(),
            selection: spec,
            binding: [9; 32],
        }
    }
}
#[test]
fn capture_projects_selected_subtrees_preserves_exclusions_and_omitted_source() {
    let f = Fixture::new();
    let files = f.tree.path();
    fs::write(files.join("src/file"), b"after\n").unwrap();
    fs::write(files.join("src/.env"), b"new secret\n").unwrap();
    fs::write(files.join("src/build/cache"), b"new output\n").unwrap();
    fs::write(files.join("outside"), b"not selected\n").unwrap();
    fs::write(files.join("src/script"), b"new script\n").unwrap();
    fs::set_permissions(files.join("src/script"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(files.join("src/new-empty")).unwrap();
    symlink("file", files.join("src/link")).unwrap();
    fs::create_dir(files.join("src/.git")).unwrap();
    fs::write(files.join("src/.git/config"), b"never capture metadata").unwrap();
    fs::remove_file(files.join("obsolete")).unwrap();
    let artifact = f.capture(&selection(
        &["src"],
        &["obsolete"],
        &["src/.env", "src/build"],
    ));
    let root = f.apply(&artifact);
    assert_eq!(f.bytes(root, "src/file"), b"after\n");
    assert_eq!(f.bytes(root, "outside"), b"outside\n");
    assert_eq!(f.bytes(root, "src/.env"), b"old secret\n");
    assert_eq!(f.bytes(root, "src/build/cache"), b"old cache\n");
    assert_eq!(f.bytes(root, "src/link"), b"file");
    assert_eq!(
        lookup(&f.source, root, &p("src/link"))
            .unwrap()
            .unwrap()
            .mode,
        EntryMode::Symlink
    );
    assert_eq!(
        lookup(&f.source, root, &p("src/script"))
            .unwrap()
            .unwrap()
            .mode,
        EntryMode::Executable
    );
    assert_eq!(
        lookup(&f.source, root, &p("src/new-empty"))
            .unwrap()
            .unwrap()
            .mode,
        EntryMode::Directory
    );
    assert!(lookup(&f.source, root, &p("obsolete")).unwrap().is_none());
    let src = lookup(&f.source, root, &p("src")).unwrap().unwrap();
    let Object::Directory(dir) = f.source.get(src.target).unwrap() else {
        panic!()
    };
    assert!(dir.entries().iter().all(|e| e.name.as_bytes() != b".git"));
    assert!(!artifact.baseline_dependencies().is_empty());
}
#[test]
fn completed_capture_reopens_without_source_and_never_replaces_published_bytes() {
    let f = Fixture::new();
    let spec = selection(&["src/file"], &[], &[]);
    let artifact = f.capture(&spec);
    let digest = artifact.manifest_digest();
    let path = artifact.path().to_owned();
    fs::write(f.tree.path().join("src/file"), b"later mutation\n").unwrap();
    assert!(
        f.tree
            .capture_selected(&spec, &f.publication("capture"), Limits::default())
            .is_err()
    );
    fs::remove_dir_all(f.tree.path()).unwrap();
    drop(artifact);
    let reopened = CaptureArtifact::open(&path, f.expected(&spec), Limits::default()).unwrap();
    assert_eq!(reopened.manifest_digest(), digest);
    assert_eq!(f.bytes(f.apply(&reopened), "src/file"), b"before\n");
    for field in 0..4 {
        let other = Scope::new("other", f.scope.repository()).unwrap();
        let mut expected = f.expected(&spec);
        let different = selection(&["outside"], &[], &[]);
        match field {
            0 => expected.binding = [1; 32],
            1 => expected.scope = &other,
            2 => expected.snapshot = ObjectId::from_bytes([1; 32]),
            3 => expected.selection = &different,
            _ => unreachable!(),
        }
        assert!(CaptureArtifact::open(&path, expected, Limits::default()).is_err());
    }
    let id = reopened.objects()[0];
    let raw = id.hex();
    let object = path
        .join("objects/capture-pack")
        .join(&raw[..2])
        .join(&raw[2..]);
    fs::write(object, b"corrupt frame").unwrap();
    assert!(CaptureArtifact::open(&path, f.expected(&spec), Limits::default()).is_err());
}
#[test]
fn sparse_capture_never_turns_undelivered_files_into_deletions() {
    let mut f = Fixture::new();
    let mut manifest = Baseline {
        tenant: f.scope.tenant().into(),
        repository: f.scope.repository().to_string(),
        snapshot: f.tree.snapshot(),
        selection: Selection::from_profile(&SourceProfile::sparse(vec![p("src/file")]).unwrap()),
        entries: f.tree.manifest.entries.clone(),
        preserved_local_state: false,
    };
    manifest
        .entries
        .retain(|path, _| path == &p("src") || path == &p("src/file"));
    f.tree.manifest = Arc::new(manifest);
    fs::remove_file(f.tree.path().join("outside")).unwrap();
    fs::remove_file(f.tree.path().join("src/.env")).unwrap();
    for spec in [
        selection(&["src"], &[], &[]),
        selection(&[], &["outside"], &[]),
        selection(&["src/new"], &[], &[]),
    ] {
        assert!(
            f.tree
                .capture_selected(&spec, &f.publication("bad"), Limits::default())
                .is_err()
        );
        assert!(!f._temp.path().join("bad").exists());
    }
    fs::write(f.tree.path().join("src/file"), b"changed\n").unwrap();
    let artifact = f.capture(&selection(&["src/file"], &[], &[]));
    let root = f.apply(&artifact);
    assert_eq!(f.bytes(root, "src/file"), b"changed\n");
    assert_eq!(f.bytes(root, "outside"), b"outside\n");
    assert_eq!(f.bytes(root, "src/.env"), b"old secret\n");
}
#[test]
fn capture_rejects_unsafe_files_unconfirmed_deletes_shadowed_exclusions_and_limits() {
    for attack in 0..9 {
        let f = Fixture::new();
        let path = f.tree.path().join("src/file");
        let mut limits = Limits::default();
        let mut spec = selection(&["src/file"], &[], &[]);
        match attack {
            0 => fs::hard_link(&path, f.tree.path().join("alias")).unwrap(),
            1 => {
                fs::remove_file(&path).unwrap();
                symlink("/etc/passwd", &path).unwrap();
            }
            2 => {
                fs::rename(f.tree.path().join("src"), f.tree.path().join("elsewhere")).unwrap();
                symlink("elsewhere", f.tree.path().join("src")).unwrap();
            }
            3 => {
                // macOS sandbox strips set-id bits; sticky remains observable.
                // Linux CI exercises an actual setuid file at this boundary.
                #[cfg(target_os = "linux")]
                let mode = 0o4700;
                #[cfg(not(target_os = "linux"))]
                let mode = 0o1700;
                fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
                assert_ne!(
                    fs::metadata(&path).unwrap().permissions().mode() & 0o7000,
                    0
                );
            }
            4 => limits.max_file_bytes = 1,
            5 => limits.max_source_bytes = 1,
            6 => spec = selection(&[], &["src/file"], &[]),
            7 => {
                fs::remove_file(&path).unwrap();
                // rustix does not expose mkfifoat on macOS. The external
                // command constructs only a hostile fixture, never captures it.
                assert!(
                    std::process::Command::new("/usr/bin/mkfifo")
                        .arg(&path)
                        .status()
                        .unwrap()
                        .success()
                );
            }
            8 => {
                fs::remove_dir_all(f.tree.path().join("src/build")).unwrap();
                spec = selection(&["src"], &[], &["src/build/cache"]);
            }
            _ => unreachable!(),
        }
        assert!(
            f.tree
                .capture_selected(&spec, &f.publication("bad"), limits)
                .is_err(),
            "{attack}"
        );
        assert!(!f._temp.path().join("bad").exists(), "{attack}");
    }
}
#[test]
fn captured_blob_can_satisfy_an_earlier_excluded_dependency_and_keep_child_first_order() {
    let f = Fixture::new();
    fs::write(f.tree.path().join("zcopy"), b"old secret\n").unwrap();
    let artifact = f.capture(&selection(&["src", "zcopy"], &[], &["src/.env"]));
    let root = f.apply(&artifact);
    assert_eq!(f.bytes(root, "zcopy"), b"old secret\n");
    let mut seen = artifact.baseline_dependencies().clone();
    for id in artifact.objects() {
        let object = artifact.object(*id).unwrap();
        for (child, kind) in object.references() {
            assert_eq!(seen.get(&child), Some(&kind));
        }
        seen.insert(*id, object.kind());
    }
}
#[test]
fn raw_paths_and_directory_type_changes_are_explicit_operations() {
    let f = Fixture::new();
    #[cfg(target_os = "linux")]
    let name = b"new-\xff\n".as_slice();
    #[cfg(not(target_os = "linux"))]
    let name = "new-ю\n".as_bytes();
    let mut path = b"src/".to_vec();
    path.extend_from_slice(name);
    fs::write(
        f.tree.path().join(std::ffi::OsStr::from_bytes(&path)),
        b"raw\0binary",
    )
    .unwrap();
    fs::remove_file(f.tree.path().join("obsolete")).unwrap();
    fs::create_dir(f.tree.path().join("obsolete")).unwrap();
    let artifact = f.capture(
        &CaptureSelection::new(
            vec![RepoPath::new(path.clone()).unwrap(), p("obsolete")],
            vec![],
            vec![],
        )
        .unwrap(),
    );
    let root = f.apply(&artifact);
    assert_eq!(
        lookup(&f.source, root, &p("obsolete"))
            .unwrap()
            .unwrap()
            .mode,
        EntryMode::Directory
    );
    let value = lookup(&f.source, root, &RepoPath::new(path).unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(
        f.source.get(value.target).unwrap(),
        Object::Blob(b"raw\0binary".to_vec())
    );
}

#[test]
fn captured_manifest_matches_independent_binary_and_graph_reference() {
    let f = Fixture::new();
    fs::write(f.tree.path().join("src/file"), b"after\n").unwrap();
    let artifact = f.capture(&selection(&["src/file"], &[], &[]));
    assert_eq!(
        fs::read(artifact.path().join("manifest.json")).unwrap(),
        include_bytes!("../../tests/fixtures/capture-manifest-v1.json")
    );
    assert_eq!(
        ObjectId::from_bytes(artifact.manifest_digest()).to_string(),
        include_str!("../../tests/fixtures/capture-manifest-v1.sha256").trim()
    );
}
