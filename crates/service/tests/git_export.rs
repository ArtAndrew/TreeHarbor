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
