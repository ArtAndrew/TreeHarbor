//! Opt-in Linux test using a disposable bounded ext4 mount and a local gVisor
//! engine. The image must contain the runtime init, /bin/sh and /usr/bin/git.
use super::*;
use serde::Deserialize;
use std::{os::unix::fs::DirBuilderExt, path::Path};
use treeharbor_core::{
    ChangeOperation, Changes, Entry, EntryMode, EntryRef, Name, ObjectId, RepoPath,
};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Sandbox {
    engine_socket: PathBuf,
    mount: PathBuf,
    image: String,
}

pub(super) fn exercise_if_configured(
    f: &Fixture,
    root: ObjectId,
    revision: &str,
    node_root: &Path,
    credential: &Path,
    storage: &str,
    source_profile: &str,
) {
    let Some(config) = std::env::var_os("TH_TEST_GIT_WORKSPACE_SANDBOX") else {
        return;
    };
    let Sandbox {
        engine_socket,
        mount,
        image,
    } = serde_json::from_slice(&fs::read(config).unwrap()).unwrap();
    let workspace = Uuid::new_v4().to_string();
    let slot = Uuid::new_v4().to_string();
    let run = Uuid::new_v4().to_string();
    let seal = Uuid::new_v4().to_string();
    let node = |action: &str, args: &[&str]| {
        let output = Command::new(&f.binary)
            .env("PATH", "/treeharbor-test-no-executables")
            .args(["node", action, "--config"])
            .arg(&f.config_path)
            .arg("--credential-file")
            .arg(credential)
            .arg("--root")
            .arg(node_root)
            .args(["--storage", storage])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<Value>(&output.stdout).unwrap()
    };
    let placement = mount.join(&slot);
    fs::DirBuilder::new()
        .mode(0o700)
        .create(&placement)
        .unwrap();
    node(
        "volume-register",
        &[
            "--slot",
            &slot,
            "--mount",
            mount.to_str().unwrap(),
            "--placement",
            placement.to_str().unwrap(),
            "--limit-bytes",
            "134217728",
        ],
    );
    let runtime = f.dir.path().join("git-runtime-profile.json");
    files::write_private(
        &runtime,
        &serde_json::to_vec(&json!({
            "version": 1, "image": image, "uid": rustix::process::geteuid().as_raw(),
            "gid": rustix::process::getegid().as_raw(), "memory_bytes": 268435456,
            "nano_cpus": 1_000_000_000u64, "max_processes": 64, "timeout_ms": 10000,
            "workspace_bytes": 134217728,
        }))
        .unwrap(),
    )
    .unwrap();
    let profile = f.admin_ok(&[
        "runtime-profile-register",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &f.scope.repository().to_string(),
        "--file",
        runtime.to_str().unwrap(),
    ]);
    result(f.cli(&[
        "workspace-create",
        "--key",
        "git-sandbox-workspace",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--workspace",
        &workspace,
        "--base",
        &RevisionRef::Native(root).to_string(),
        "--profile-digest",
        source_profile,
        "--git-compatible",
    ]));
    let prepared = node(
        "prepare-once",
        &[
            "--git",
            "/usr/bin/git",
            "--workspace",
            &workspace,
            "--slot",
            &slot,
        ],
    );
    assert_eq!(prepared["state"], "files_ready");
    let working = Path::new(prepared["path"].as_str().unwrap());
    // Mutate HEAD and the index with real Git, then corrupt local config. The
    // resulting proposal must still use the imported base and selected bytes.
    let task = r#"
        test "$(/usr/bin/git rev-parse HEAD)" = "$1"
        test -z "$(/usr/bin/git status --porcelain)"
        /usr/bin/git reset --hard HEAD^
        test "$(/usr/bin/git rev-parse HEAD)" != "$1"
        printf 'fn main() { println!("agent"); }\n' > main.rs
        /usr/bin/git add main.rs
        printf '[invalid\n' > .git/config
    "#;
    result(f.cli(&[
        "run-request",
        "--key",
        "git-sandbox-run",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--run",
        &run,
        "--workspace",
        &workspace,
        "--generation",
        "1",
        "--profile-digest",
        profile["profile_digest"].as_str().unwrap(),
        "--agent",
        "git-fixture",
        "--task",
        "git-edit",
        "--",
        "/bin/sh",
        "-ec",
        task,
        "fixture",
        revision.strip_prefix("git:sha1:").unwrap(),
    ]));
    let engine = engine_socket.to_str().unwrap();
    let finished = node("run-once", &["--engine-socket", engine, "--run", &run]);
    assert_eq!(finished["state"], "succeeded");
    assert_eq!(finished["runtime_removed"], true);
    assert_eq!(
        fs::read(working.join(".git/config")).unwrap(),
        b"[invalid\n"
    );
    let modified = b"fn main() { println!(\"agent\"); }\n";
    assert_eq!(fs::read(working.join("main.rs")).unwrap(), modified);

    let request = [
        "seal-request",
        "--key",
        "git-sandbox-seal",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--seal",
        &seal,
        "--run",
        &run,
        "--generation",
        "1",
        "--replace",
    ];
    assert!(
        !f.cli(&[request.as_slice(), &[".git/config"]].concat())
            .status
            .success()
    );
    result(f.cli(&[request.as_slice(), &["main.rs"]].concat()));
    let captured = node("seal-once", &["--engine-socket", engine, "--seal", &seal]);
    assert_eq!(captured["state"], "ready");
    let entry = |bytes: &[u8]| {
        Some(EntryRef {
            mode: EntryMode::File,
            target: Object::Blob(bytes.to_vec()).id().unwrap(),
        })
    };
    let operations = Object::ChangeOperations(
        Changes::new(vec![ChangeOperation {
            path: RepoPath::new(b"main.rs".to_vec()).unwrap(),
            expected: entry(b"fn main() {}\n"),
            replacement: entry(modified),
        }])
        .unwrap(),
    )
    .id()
    .unwrap();
    assert_eq!(captured["operations"], operations.to_string());

    let repository = f.scope.repository().to_string();
    f.admin_ok(&[
        "access-set",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &repository,
        "--principal",
        "importer",
        "--permission",
        "read",
        "--permission",
        "workspace.write",
        "--permission",
        "repository.write",
        "--permission",
        "change.write",
    ]);
    let writer = f.dir.path().join("git-runtime-writer.json");
    f.admin_ok(&[
        "token-issue",
        "--tenant",
        f.scope.tenant(),
        "--repository",
        &repository,
        "--principal",
        "importer",
        "--permission",
        "change.write",
        "--output",
        writer.to_str().unwrap(),
    ]);
    let changeset = Uuid::new_v4().to_string();
    let accept = [
        "seal-accept",
        "--key",
        "git-sandbox-accept",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--seal",
        &seal,
        "--changeset",
        &changeset,
        "--previous-version",
        "0",
    ];
    let accepted = result(f.cli_with_credential(&writer, &accept));
    assert_eq!(accepted["state"], "accepted");
    assert_eq!(accepted["run"], run);
    assert_eq!(
        accepted["base_revision"],
        RevisionRef::Native(root).to_string()
    );
    let directory = Directory::new(vec![Entry {
        name: Name::new(b"main.rs".to_vec()).unwrap(),
        value: entry(modified).unwrap(),
    }])
    .unwrap();
    let snapshot = Object::Snapshot {
        root: Object::Directory(directory).id().unwrap(),
    };
    assert_eq!(
        accepted["result_snapshot"],
        snapshot.id().unwrap().to_string()
    );

    result(f.cli(&[
        "workspace-delete",
        "--key",
        "git-sandbox-drain",
        "--authority",
        "connected",
        "--epoch",
        "1",
        "--workspace",
        &workspace,
        "--generation",
        "1",
    ]));
    node(
        "cleanup-once",
        &["--engine-socket", engine, "--workspace", &workspace],
    );
    assert!(!working.exists());
    assert_eq!(result(f.cli_with_credential(&writer, &accept)), accepted);
    eprintln!("PASS real Git-compatible managed run, stopped-file seal, acceptance and cleanup");
}
