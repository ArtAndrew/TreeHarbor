//! Git is an independent test oracle, never a dependency of the native engine.
//! Temporary repositories use no user configuration, hooks, attributes or filters.
mod common;
use common::*;
use std::{
    io::Write,
    process::{Command, Output, Stdio},
};
use treeharbor_core::EntryMode::{self, *};
use treeharbor_merge::{Limits, Outcome, merge_snapshots};

struct Git(tempfile::TempDir);
impl Git {
    fn new() -> Self {
        let git = Self(tempfile::tempdir().unwrap());
        git.ok(&["init", "--quiet", "--template="], b"");
        git
    }
    fn run(&self, args: &[&str], input: &[u8]) -> Output {
        let mut child = Command::new("git")
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.0.path())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_ATTR_NOSYSTEM", "1")
            .env("GIT_AUTHOR_NAME", "Test")
            .env("GIT_AUTHOR_EMAIL", "test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Test")
            .env("GIT_COMMITTER_EMAIL", "test@example.invalid")
            .env("GIT_AUTHOR_DATE", "2001-01-01T00:00:00+0000")
            .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00+0000")
            .current_dir(self.0.path())
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.attributesFile=/dev/null",
                "-c",
                "core.autocrlf=false",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("Git test oracle must be installed");
        child.stdin.take().unwrap().write_all(input).unwrap();
        child.wait_with_output().unwrap()
    }
    fn ok(&self, args: &[&str], input: &[u8]) -> Vec<u8> {
        let out = self.run(args, input);
        assert!(
            out.status.success(),
            "Git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out.stdout
    }
    fn id(&self, args: &[&str], input: &[u8]) -> String {
        String::from_utf8(self.ok(args, input))
            .unwrap()
            .trim()
            .into()
    }
    fn commit(&self, files: &Files, parent: Option<&str>, message: &str) -> String {
        self.ok(&["read-tree", "--empty"], b"");
        let mut index = Vec::new();
        for (path, (mode, content)) in files {
            let id = self.id(&["hash-object", "-w", "--stdin"], content);
            index.extend_from_slice(format!("{:o} {id}\t", *mode as u32).as_bytes());
            index.extend_from_slice(path);
            index.push(0);
        }
        self.ok(&["update-index", "-z", "--index-info"], &index);
        let tree = self.id(&["write-tree"], b"");
        let mut args = vec!["commit-tree", tree.as_str()];
        if let Some(parent) = parent {
            args.extend(["-p", parent]);
        }
        self.id(&args, message.as_bytes())
    }
    fn contents(&self, tree: &str) -> Files {
        let listing = self.ok(&["ls-tree", "-r", "-z", tree], b"");
        listing
            .split(|b| *b == 0)
            .filter(|r| !r.is_empty())
            .map(|record| {
                let tab = record.iter().position(|b| *b == b'\t').unwrap();
                let metadata = std::str::from_utf8(&record[..tab]).unwrap();
                let fields: Vec<_> = metadata.split(' ').collect();
                assert_eq!(fields[1], "blob");
                let mode = EntryMode::try_from(u32::from_str_radix(fields[0], 8).unwrap()).unwrap();
                let blob = self.ok(&["cat-file", "blob", fields[2]], b"");
                (record[tab + 1..].to_vec(), (mode, blob))
            })
            .collect()
    }
}

fn compare(label: &str, b: &Files, l: &Files, r: &Files) {
    let git = Git::new();
    let base = git.commit(b, None, "base");
    let left = git.commit(l, Some(&base), "left");
    let right = git.commit(r, Some(&base), "right");
    let oracle = git.run(
        &["merge-tree", "--write-tree", "--no-messages", &left, &right],
        b"",
    );
    let status = oracle.status.code().unwrap();
    assert!(
        [0, 1].contains(&status),
        "{label}: Git failed: {}",
        String::from_utf8_lossy(&oracle.stderr)
    );
    let s = Store::default();
    let [b, l, r] = [snapshot(&s, b), snapshot(&s, l), snapshot(&s, r)];
    match merge_snapshots(&s, b, l, r, Limits::default()).unwrap_or_else(|e| panic!("{label}: {e}"))
    {
        Outcome::Clean(result) => {
            assert_eq!(status, 0, "{label}: native silently resolved Git conflict");
            let tree =
                std::str::from_utf8(oracle.stdout.split(|b| *b == b'\n').next().unwrap()).unwrap();
            assert_eq!(
                contents(&s, result.result()),
                git.contents(tree),
                "{label}: different bytes, modes or paths"
            );
        }
        Outcome::Conflicted(_) => assert_eq!(status, 1, "{label}: unexpected native conflict"),
    }
}

#[test]
fn supported_tree_matrix_matches_real_git() {
    let cases = [
        (
            "independent paths",
            plain(&[("a", "old"), ("b", "old")]),
            plain(&[("a", "new"), ("b", "old")]),
            plain(&[("a", "old"), ("b", "new")]),
        ),
        (
            "delete and unrelated edit",
            plain(&[("a", "old"), ("b", "old")]),
            plain(&[("b", "old")]),
            plain(&[("a", "old"), ("b", "new")]),
        ),
        (
            "modify delete",
            plain(&[("f", "old")]),
            Files::new(),
            plain(&[("f", "new")]),
        ),
        (
            "both delete",
            plain(&[("f", "old")]),
            Files::new(),
            Files::new(),
        ),
        (
            "independent adds",
            Files::new(),
            plain(&[("left/a", "left")]),
            plain(&[("right/b", "right")]),
        ),
        (
            "same add",
            Files::new(),
            plain(&[("f", "same")]),
            plain(&[("f", "same")]),
        ),
        (
            "add add",
            Files::new(),
            plain(&[("f", "left")]),
            plain(&[("f", "right")]),
        ),
        (
            "rename modify",
            plain(&[("old", "before\n")]),
            plain(&[("new", "before\n")]),
            plain(&[("old", "after\n")]),
        ),
        (
            "same rename",
            plain(&[("old", "before\n")]),
            plain(&[("new", "before\n")]),
            plain(&[("new", "before\n")]),
        ),
        (
            "rename rename",
            plain(&[("old", "before\n")]),
            plain(&[("left", "before\n")]),
            plain(&[("right", "before\n")]),
        ),
        (
            "rename delete",
            plain(&[("old", "before\n")]),
            plain(&[("new", "before\n")]),
            Files::new(),
        ),
        (
            "rename add collision",
            plain(&[("old", "before\n")]),
            plain(&[("new", "before\n")]),
            plain(&[("old", "after\n"), ("new", "another\n")]),
        ),
        (
            "file directory",
            Files::new(),
            plain(&[("a", "file"), ("a-1", "between")]),
            plain(&[("a/b", "child")]),
        ),
        (
            "binary independent",
            files(&[(b"f", File, b"a\0b"), (b"g", File, b"before")]),
            files(&[(b"f", File, b"a\0c"), (b"g", File, b"before")]),
            files(&[(b"f", File, b"a\0b"), (b"g", File, b"after")]),
        ),
        (
            "binary conflict",
            files(&[(b"f", File, b"a\0b")]),
            files(&[(b"f", File, b"a\0c")]),
            files(&[(b"f", File, b"a\0d")]),
        ),
        (
            "symlink conflict",
            files(&[(b"f", Symlink, b"old")]),
            files(&[(b"f", Symlink, b"left")]),
            files(&[(b"f", Symlink, b"right")]),
        ),
        (
            "symlink type",
            files(&[(b"f", File, b"old")]),
            files(&[(b"f", Symlink, b"left")]),
            files(&[(b"f", File, b"right")]),
        ),
        (
            "mode with content",
            files(&[(b"f", File, b"before")]),
            files(&[(b"f", Executable, b"before")]),
            files(&[(b"f", File, b"after")]),
        ),
        (
            "add modes",
            Files::new(),
            files(&[(b"f", File, b"same")]),
            files(&[(b"f", Executable, b"same")]),
        ),
        (
            "byte paths",
            files(&[(b"f\xff", File, b"before"), (b"space name", File, b"old")]),
            files(&[
                (b"f\xff", Executable, b"before"),
                (b"space name", File, b"old"),
            ]),
            files(&[(b"f\xff", File, b"after"), (b"space name", File, b"new")]),
        ),
    ];
    for (label, b, l, r) in cases {
        compare(label, &b, &l, &r);
        compare(label, &b, &r, &l);
    }
}

#[test]
fn text_positions_line_endings_and_missing_final_newline_match_git() {
    for newline in [b"\n".as_slice(), b"\r\n".as_slice()] {
        for final_newline in [true, false] {
            for (left, right) in [(0, 7), (2, 5), (3, 3), (3, 4)] {
                let text = |change: Option<(usize, &str)>| {
                    let mut bytes = Vec::new();
                    for i in 0..8 {
                        let line = if let Some((at, value)) = change
                            && at == i
                        {
                            value.to_string()
                        } else {
                            format!("line-{i}")
                        };
                        bytes.extend_from_slice(line.as_bytes());
                        if i != 7 || final_newline {
                            bytes.extend_from_slice(newline);
                        }
                    }
                    files(&[(b"f", File, &bytes)])
                };
                compare(
                    "generated text",
                    &text(None),
                    &text(Some((left, "LEFT"))),
                    &text(Some((right, "RIGHT"))),
                );
            }
        }
    }
}
