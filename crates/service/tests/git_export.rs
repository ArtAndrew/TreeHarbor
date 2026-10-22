#![cfg(unix)]
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[test]
fn actual_cli_exports_pack_without_inherited_git_configuration_or_checkout() {
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("test.txt"), b"kept\0exactly\r\n").unwrap();
    fs::write(source.join(".gitattributes"), b"* filter=evil\n").unwrap();
    let pack = temp.path().join("pack");
    let captured = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["source-pack", "--directory"])
        .arg(&source)
        .arg("--output")
        .arg(&pack)
        .args([
            "--path",
            "test.txt",
            "--path",
            ".gitattributes",
            "--author-name",
            "Test",
            "--author-email",
            "test@example.invalid",
        ])
        .output()
        .unwrap();
    assert!(
        captured.status.success(),
        "{}",
        String::from_utf8_lossy(&captured.stderr)
    );
    fs::remove_dir_all(source).unwrap();
    // A hostile surrounding repository and inherited config must not be used.
    let initialized = Command::new("git")
        .current_dir(temp.path())
        .args(["init", "--quiet"])
        .output()
        .unwrap();
    assert!(initialized.status.success());
    let hooks = temp.path().join("hooks");
    fs::create_dir(&hooks).unwrap();
    let sentinel = temp.path().join("hook-ran");
    let script = format!("#!/bin/sh\ntouch '{}'\nexit 1\n", sentinel.display());
    for name in ["reference-transaction", "post-checkout", "pre-commit"] {
        fs::write(hooks.join(name), &script).unwrap();
        fs::set_permissions(hooks.join(name), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let config = temp.path().join("hostile.config");
    fs::write(&config, format!("[core]\n hooksPath = {}\n[filter \"evil\"]\n required = true\n smudge = false\n clean = false\n[init]\n defaultObjectFormat = sha256\n", hooks.display())).unwrap();
    fs::copy(&config, temp.path().join(".git/config")).unwrap();
    let output = temp.path().join("export");
    let result = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .current_dir(temp.path())
        .env("PATH", "/treeharbor-test-no-executables")
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_SYSTEM", &config)
        .env("GIT_DIR", temp.path().join(".git"))
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.hooksPath")
        .env("GIT_CONFIG_VALUE_0", &hooks)
        .args(["git-export", "--pack"])
        .arg(&pack)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let manifest: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(manifest["git_object_format"], "sha1");
    assert_eq!(manifest["projection"], "native-v1");
    assert!(!sentinel.exists());
    assert!(!output.join("test.txt").exists());
    let restored = temp.path().join("restored");
    let clone = Command::new("git")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["clone", "--bare"])
        .arg(output.join("history.bundle"))
        .arg(&restored)
        .output()
        .unwrap();
    assert!(
        clone.status.success(),
        "{}",
        String::from_utf8_lossy(&clone.stderr)
    );
    let contents = Command::new("git")
        .current_dir(&restored)
        .args(["show", "main:test.txt"])
        .output()
        .unwrap();
    assert!(contents.status.success());
    assert_eq!(contents.stdout, b"kept\0exactly\r\n");
    assert!(!sentinel.exists());
    let second = Command::new(env!("CARGO_BIN_EXE_treeharbor"))
        .args(["git-export", "--pack"])
        .arg(&pack)
        .arg("--output")
        .arg(&output)
        .output()
        .unwrap();
    assert!(!second.status.success());
}

#[test]
fn actual_cli_exports_a_complete_origin_graph_and_rejects_tampered_association() {
    use std::io::Write;
    use std::process::Stdio;
    use treeharbor_core::*;
    use treeharbor_storage::{LocalStore, TenantId};
    let temp = tempfile::tempdir().unwrap();
    fs::set_permissions(temp.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let source = temp.path().join("source");
    fs::create_dir(&source).unwrap();
    fs::write(source.join("file"), b"exact").unwrap();
    let pack = temp.path().join("pack");
    let cli = env!("CARGO_BIN_EXE_treeharbor");
    assert!(
        Command::new(cli)
            .args(["source-pack", "--directory"])
            .arg(&source)
            .arg("--output")
            .arg(&pack)
            .args([
                "--path",
                "file",
                "--author-name",
                "Test",
                "--author-email",
                "test@example.invalid"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let first = temp.path().join("native-export");
    assert!(
        Command::new(cli)
            .args(["git-export", "--pack"])
            .arg(&pack)
            .arg("--output")
            .arg(&first)
            .output()
            .unwrap()
            .status
            .success()
    );
    let git = temp.path().join("original.git");
    assert!(
        Command::new("git")
            .args(["clone", "--bare"])
            .arg(first.join("history.bundle"))
            .arg(&git)
            .output()
            .unwrap()
            .status
            .success()
    );
    let original = Command::new("git")
        .current_dir(&git)
        .args(["cat-file", "commit", "main"])
        .output()
        .unwrap();
    assert!(original.status.success());
    let original = String::from_utf8(original.stdout).unwrap().replace(
        "committer Test <test@example.invalid> 0 +0000",
        "committer Another <other@example.invalid> 1 +0030\nx-preserved header",
    );
    let mut hash = Command::new("git")
        .current_dir(&git)
        .args(["hash-object", "-w", "-t", "commit", "--stdin"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    hash.stdin
        .take()
        .unwrap()
        .write_all(original.as_bytes())
        .unwrap();
    let hash = hash.wait_with_output().unwrap();
    assert!(hash.status.success());
    let git_oid = String::from_utf8(hash.stdout).unwrap().trim().to_owned();
    let RevisionRef::Git(oid) = format!("git:sha1:{git_oid}").parse().unwrap() else {
        panic!()
    };
    // Construct a complete local native graph, not a claim that source-pack is a
    // Git importer. This exercises the actual CLI/pack decoder/export boundary.
    let store = LocalStore::open(&pack, &TenantId::new("native-pack").unwrap()).unwrap();
    let manifest_path = pack.join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    let old_id: ObjectId = manifest["commit"].as_str().unwrap().parse().unwrap();
    let Object::NativeCommit(mut c) = store.get(old_id).unwrap() else {
        panic!()
    };
    let raw = store
        .put(&Object::Blob(original.as_bytes().to_vec()))
        .unwrap();
    c.git_origin = Some(Box::new(GitOrigin { oid, raw }));
    let head = store.put(&Object::NativeCommit(c.clone())).unwrap();
    let mut ids: Vec<ObjectId> = manifest["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().parse().unwrap())
        .collect();
    assert_eq!(ids.pop(), Some(old_id));
    ids.extend([raw, head]);
    let save = |manifest: &mut serde_json::Value, ids: &[ObjectId]| {
        manifest["commit"] = ids.last().unwrap().to_string().into();
        manifest["objects"] =
            serde_json::json!(ids.iter().map(ToString::to_string).collect::<Vec<_>>());
        manifest["encoded_bytes"] = serde_json::json!(
            ids.iter()
                .map(|id| store.get(*id).unwrap().encode().unwrap().len() as u64)
                .sum::<u64>()
        );
        fs::write(&manifest_path, serde_json::to_vec(manifest).unwrap()).unwrap();
    };
    save(&mut manifest, &ids);
    let second = temp.path().join("origin-export");
    let result = Command::new(cli)
        .args(["git-export", "--pack"])
        .arg(&pack)
        .arg("--output")
        .arg(&second)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(result["projection"], "native-with-git-origin-v1");
    assert_eq!(result["refs"]["refs/heads/main"]["git"], git_oid);
    c.message = "tampered".into();
    *ids.last_mut().unwrap() = store.put(&Object::NativeCommit(c)).unwrap();
    save(&mut manifest, &ids);
    let bad = temp.path().join("bad-export");
    let result = Command::new(cli)
        .args(["git-export", "--pack"])
        .arg(&pack)
        .arg("--output")
        .arg(&bad)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(!bad.exists());
}
