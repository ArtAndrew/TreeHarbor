use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt, symlink},
    path::Path,
    process::Command,
};
use treeharbor_core::{ObjectId, RepoPath, SourceProfile};
use treeharbor_materializer::{BaselineExpectation, Limits, MaterializedTree};
use treeharbor_metadata::Scope;
use uuid::Uuid;

// Independent v1 bytes, shared by specification, writer golden and this reader.
const GOLDEN: &str = concat!(
    "{\"header\":{\"materializer\":\"copy-unix-v1\",\"preserved_local_state\":false,\"repository\":\"00000000-0000-0000-0000-000000000001\",\"selection\":null,\"snapshot\":\"",
    "0303030303030303030303030303030303030303030303030303030303030303",
    "\",\"tenant\":\"tenant\",\"version\":1},\"entries\":[[\"61\",33188,\"",
    "0101010101010101010101010101010101010101010101010101010101010101",
    "\"]]}\n"
);
const GOLDEN_SHA: &str = "sha256:cbe9a4f9c1b174ccf3ba76c0f2024f0939bc50cc564b1b23a64c6f5e6b57e4eb";

fn scope() -> Scope {
    Scope::new("tenant", Uuid::from_u128(1)).unwrap()
}
fn fixture(raw: &str) -> tempfile::TempDir {
    let root = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    fs::DirBuilder::new()
        .mode(0o700)
        .create(root.path().join("files"))
        .unwrap();
    let manifest = root.path().join("manifest.json");
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&manifest)
        .unwrap();
    fs::write(manifest, raw).unwrap();
    root
}
fn load(
    root: &Path,
    raw: &str,
    limits: Limits,
    profile: &SourceProfile,
) -> treeharbor_materializer::Result<MaterializedTree> {
    MaterializedTree::load_baseline(
        root,
        BaselineExpectation {
            scope: &scope(),
            snapshot: ObjectId::from_bytes([3; 32]),
            profile,
            manifest_digest: Sha256::digest(raw.as_bytes()).into(),
        },
        limits,
    )
}
fn reject(raw: &str) {
    let root = fixture(raw);
    assert!(
        load(root.path(), raw, Limits::default(), &SourceProfile::full()).is_err(),
        "accepted: {raw}"
    );
}

#[test]
fn known_v1_digest_loads_without_restoring_or_sealing_agent_files() {
    let root = fixture(GOLDEN);
    fs::write(root.path().join("files/a"), b"edited\0\xff").unwrap();
    fs::write(root.path().join("files/new"), b"untracked").unwrap();
    let digest: ObjectId = GOLDEN_SHA.parse().unwrap();
    let recovered = MaterializedTree::load_baseline(
        root.path(),
        BaselineExpectation {
            scope: &scope(),
            snapshot: ObjectId::from_bytes([3; 32]),
            profile: &SourceProfile::full(),
            manifest_digest: *digest.as_bytes(),
        },
        Limits::default(),
    )
    .unwrap();
    assert!(recovered.statistics().is_none());
    assert!(!recovered.preserved_local_state());
    assert!(recovered.includes(&RepoPath::new(b"a".to_vec()).unwrap()));
    assert_eq!(
        fs::read(recovered.path().join("a")).unwrap(),
        b"edited\0\xff"
    );
    assert_eq!(
        fs::read(recovered.path().join("new")).unwrap(),
        b"untracked"
    );
    fs::remove_file(recovered.path().join("a")).unwrap();
    drop(recovered);
    let recovered = load(
        root.path(),
        GOLDEN,
        Limits::default(),
        &SourceProfile::full(),
    )
    .unwrap();
    assert!(!recovered.path().join("a").exists());
    // Even an outward untracked link is not traversed or judged as a seal.
    symlink("/this-must-not-be-read", recovered.path().join("outside")).unwrap();
    load(
        root.path(),
        GOLDEN,
        Limits::default(),
        &SourceProfile::full(),
    )
    .unwrap();
    assert_eq!(
        fs::read_to_string(root.path().join("manifest.json")).unwrap(),
        GOLDEN
    );
}

#[test]
fn exact_observation_scope_snapshot_and_profile_are_all_required() {
    let root = fixture(GOLDEN);
    let profile = SourceProfile::sparse(vec![RepoPath::new(b"a".to_vec()).unwrap()]).unwrap();
    assert!(load(root.path(), GOLDEN, Limits::default(), &profile).is_err());
    for (bound_scope, snapshot, digest) in [
        (
            Scope::new("other", Uuid::from_u128(1)).unwrap(),
            ObjectId::from_bytes([3; 32]),
            Sha256::digest(GOLDEN.as_bytes()).into(),
        ),
        (
            Scope::new("tenant", Uuid::from_u128(2)).unwrap(),
            ObjectId::from_bytes([3; 32]),
            Sha256::digest(GOLDEN.as_bytes()).into(),
        ),
        (
            scope(),
            ObjectId::from_bytes([4; 32]),
            Sha256::digest(GOLDEN.as_bytes()).into(),
        ),
        (scope(), ObjectId::from_bytes([3; 32]), [0; 32]),
    ] {
        assert!(
            MaterializedTree::load_baseline(
                root.path(),
                BaselineExpectation {
                    scope: &bound_scope,
                    snapshot,
                    profile: &SourceProfile::full(),
                    manifest_digest: digest,
                },
                Limits::default()
            )
            .is_err()
        );
    }
    let reencoded =
        serde_json::to_string_pretty(&serde_json::from_str::<serde_json::Value>(GOLDEN).unwrap())
            .unwrap();
    fs::write(root.path().join("manifest.json"), &reencoded).unwrap();
    assert!(
        load(
            root.path(),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
    load(
        root.path(),
        &reencoded,
        Limits::default(),
        &SourceProfile::full(),
    )
    .unwrap();
}

#[test]
fn duplicate_unknown_missing_and_future_fields_never_gain_default_meanings() {
    for raw in [
        GOLDEN.replace("\"version\":1", "\"version\":2"),
        GOLDEN.replace("copy-unix-v1", "copy-unix-v2"),
        GOLDEN.replace("\"version\":1", "\"version\":1,\"version\":1"),
        GOLDEN.replace("\"version\":1", "\"version\":1,\"future\":false"),
        GOLDEN.replace("\"entries\":", "\"entries\":[],\"entries\":"),
        GOLDEN.replace("{\"header\":", "{\"header\":{},\"header\":"),
        GOLDEN.replace("\"selection\":null,", ""),
        GOLDEN.replace("\"preserved_local_state\":false,", ""),
        GOLDEN.replace(
            "\"preserved_local_state\":false",
            "\"preserved_local_state\":0",
        ),
        GOLDEN.replace("{\"header\":", "{\"unknown\":null,\"header\":"),
        GOLDEN.replace("\"version\":1", "\"version\":1.0"),
        format!("{GOLDEN}{{}}"),
        GOLDEN[..GOLDEN.len() - 3].into(),
    ] {
        reject(&raw);
    }
    let expanded = GOLDEN.replace(
        "\"preserved_local_state\":false",
        "\"preserved_local_state\":true",
    );
    let root = fixture(&expanded);
    assert!(
        load(
            root.path(),
            &expanded,
            Limits::default(),
            &SourceProfile::full()
        )
        .unwrap()
        .preserved_local_state()
    );
}

fn with_entries(entries: serde_json::Value) -> String {
    let mut value: serde_json::Value = serde_json::from_str(GOLDEN).unwrap();
    value["entries"] = entries;
    serde_json::to_string(&value).unwrap()
}

#[test]
fn entry_order_ancestors_modes_and_byte_names_are_checked() {
    let id = "01".repeat(32);
    let entry = |path: &str, mode: u32| serde_json::json!([path, mode, id]);
    for entries in [
        serde_json::json!([entry("62", 33188), entry("61", 33188)]),
        serde_json::json!([entry("61", 33188), entry("61", 33188)]),
        serde_json::json!([entry("612f62", 33188)]),
        serde_json::json!([entry("61", 33188), entry("612f62", 33188)]),
        serde_json::json!([entry("61", 0)]),
        serde_json::json!([entry("61", 57344)]),
        serde_json::json!([entry("2e2e2f61", 33188)]),
        serde_json::json!([entry("2e676974", 33188)]),
        serde_json::json!([entry("2F61", 33188)]),
        serde_json::json!([entry("0", 33188)]),
        serde_json::json!([["61", 33188, "01"]]),
        serde_json::json!([["61", 33188, id, "extra"]]),
    ] {
        reject(&with_entries(entries));
    }
    let valid = with_entries(serde_json::json!([
        entry("61", 16384),
        entry("612fff", 33261),
        entry("62", 40960)
    ]));
    let root = fixture(&valid);
    let baseline = load(
        root.path(),
        &valid,
        Limits::default(),
        &SourceProfile::full(),
    )
    .unwrap();
    assert!(baseline.includes(&RepoPath::new(b"a/\xff".to_vec()).unwrap()));
}

#[test]
fn selection_is_canonical_bounded_complete_and_restricts_entries() {
    let sparse = GOLDEN.replace("\"selection\":null", "\"selection\":[\"61\"]");
    let root = fixture(&sparse);
    let profile = SourceProfile::sparse(vec![RepoPath::new(b"a".to_vec()).unwrap()]).unwrap();
    load(root.path(), &sparse, Limits::default(), &profile).unwrap();
    for selection in [
        "[]",
        "[\"61\",\"61\"]",
        "[\"61\",\"612f62\"]",
        "[\"62\",\"61\"]",
        "[\"6A\"]",
    ] {
        let raw = GOLDEN.replace("\"selection\":null", &format!("\"selection\":{selection}"));
        let root = fixture(&raw);
        assert!(load(root.path(), &raw, Limits::default(), &profile).is_err());
    }
    let empty = sparse.replace(&format!("[[\"61\",33188,\"{}\"]]", "01".repeat(32)), "[]");
    let root = fixture(&empty);
    assert!(load(root.path(), &empty, Limits::default(), &profile).is_err());
    let outside = sparse.replace(
        &format!("[[\"61\",33188,\"{}\"]]", "01".repeat(32)),
        &format!(
            "[[\"61\",33188,\"{0}\"],[\"62\",33188,\"{0}\"]]",
            "01".repeat(32)
        ),
    );
    let root = fixture(&outside);
    assert!(load(root.path(), &outside, Limits::default(), &profile).is_err());
}

#[test]
fn parsing_respects_entry_path_and_token_budgets() {
    let raw = with_entries(serde_json::json!([
        ["61", 33188, "01".repeat(32)],
        ["62", 33188, "01".repeat(32)]
    ]));
    let root = fixture(&raw);
    for limits in [
        Limits {
            max_entries: 1,
            ..Limits::default()
        },
        Limits {
            max_path_bytes: 1,
            ..Limits::default()
        },
    ] {
        assert!(load(root.path(), &raw, limits, &SourceProfile::full()).is_err());
    }
    // A single string must be rejected before it can consume the whole manifest budget.
    reject(&GOLDEN.replace("copy-unix-v1", &"x".repeat(70_000)));
    reject(&GOLDEN.replace("\"version\":1", &format!("\"version\":{}", "1".repeat(200))));
    let oversized = root.path().join("manifest.json");
    fs::OpenOptions::new()
        .write(true)
        .open(oversized)
        .unwrap()
        .set_len(256 * 1024 * 1024 + 1)
        .unwrap();
    assert!(load(root.path(), &raw, Limits::default(), &SourceProfile::full()).is_err());
}

#[test]
fn private_manifest_checks_reject_links_permissions_and_mount_root_substitution() {
    let root = fixture(GOLDEN);
    let path = root.path().join("manifest.json");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        load(
            root.path(),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::hard_link(&path, root.path().join("alias")).unwrap();
    assert!(
        load(
            root.path(),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
    fs::remove_file(&path).unwrap();
    symlink("alias", &path).unwrap();
    assert!(
        load(
            root.path(),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
    fs::remove_file(&path).unwrap();
    fs::rename(root.path().join("alias"), &path).unwrap();
    fs::rename(root.path().join("files"), root.path().join("elsewhere")).unwrap();
    symlink("elsewhere", root.path().join("files")).unwrap();
    assert!(
        load(
            root.path(),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
    fs::remove_file(root.path().join("files")).unwrap();
    fs::rename(root.path().join("elsewhere"), root.path().join("files")).unwrap();
    symlink(root.path(), root.path().join("root-link")).unwrap();
    assert!(
        load(
            &root.path().join("root-link"),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        load(
            root.path(),
            GOLDEN,
            Limits::default(),
            &SourceProfile::full()
        )
        .is_err()
    );
}

#[test]
fn new_process_loads_baseline_and_preserves_current_files() {
    if let Some(root) = std::env::var_os("TH_BASELINE_TEST_CHILD") {
        let root = Path::new(&root);
        let digest: ObjectId = GOLDEN_SHA.parse().unwrap();
        let tree = MaterializedTree::load_baseline(
            root,
            BaselineExpectation {
                scope: &scope(),
                snapshot: ObjectId::from_bytes([3; 32]),
                profile: &SourceProfile::full(),
                manifest_digest: *digest.as_bytes(),
            },
            Limits::default(),
        )
        .unwrap();
        assert_eq!(
            fs::read(tree.path().join("a")).unwrap(),
            b"saved agent edit"
        );
        assert!(tree.statistics().is_none());
        return;
    }
    let root = fixture(GOLDEN);
    fs::write(root.path().join("files/a"), b"saved agent edit").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "new_process_loads_baseline_and_preserves_current_files",
            "--nocapture",
        ])
        .env("TH_BASELINE_TEST_CHILD", root.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(root.path().join("files/a")).unwrap(),
        b"saved agent edit"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("manifest.json")).unwrap(),
        GOLDEN
    );
}
