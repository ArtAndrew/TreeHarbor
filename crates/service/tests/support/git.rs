//! Independent source fixtures produced by system Git, not the native encoder.
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

pub fn git(repo: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    let mut child = Command::new("/usr/bin/git")
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    result.stdout
}
pub fn hash(repo: &Path, kind: &str, raw: &[u8]) -> String {
    String::from_utf8(git(
        repo,
        &["hash-object", "-w", "-t", kind, "--stdin"],
        raw,
    ))
    .unwrap()
    .trim()
    .into()
}
pub fn bundle(repo: &Path, output: &Path, head: &str) {
    git(repo, &["update-ref", "refs/heads/main", head], b"");
    git(
        repo,
        &[
            "bundle",
            "create",
            output.to_str().unwrap(),
            "refs/heads/main",
        ],
        b"",
    );
}
