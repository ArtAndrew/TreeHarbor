#![cfg(target_os = "linux")]
#[path = "support/git.rs"]
mod git_reference;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use treeharbor_core::{GitOid, RevisionRef};
use treeharbor_git_bridge::{Limits, download};
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn private() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    fs::set_permissions(d.path(), fs::Permissions::from_mode(0o700)).unwrap();
    d
}
fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn history(root: &Path) -> (PathBuf, String) {
    let repo = root.join("demo.git");
    git_reference::git(
        root,
        &["init", "--bare", "--template=", repo.to_str().unwrap()],
        b"",
    );
    let mut head = String::new();
    for body in [b"first\n".as_slice(), b"binary\0\xff"] {
        let blob = git_reference::hash(&repo, "blob", body);
        let tree = String::from_utf8(git_reference::git(
            &repo,
            &["mktree"],
            format!("100644 blob {blob}\tfile\n").as_bytes(),
        ))
        .unwrap()
        .trim()
        .to_owned();
        let parent = if head.is_empty() {
            String::new()
        } else {
            format!("parent {head}\n")
        };
        head=git_reference::hash(&repo,"commit",format!("tree {tree}\n{parent}author Test <test@example.invalid> 1 +0000\ncommitter Test <test@example.invalid> 1 +0000\n\nfixture\n").as_bytes());
    }
    git_reference::bundle(&repo, &root.join("oracle.bundle"), &head);
    (repo, head)
}
fn oid(sha: &str) -> GitOid {
    let RevisionRef::Git(id) = format!("git:sha1:{sha}").parse().unwrap() else {
        panic!()
    };
    id
}
fn request<'a>(
    worker: &'a Path,
    git: &'a Path,
    output: &'a Path,
    commit: GitOid,
    timeout: Duration,
) -> download::Request<'a> {
    download::Request {
        worker,
        git,
        output,
        owner: "example-org",
        name: "demo",
        token: "synthetic-only-not-a-credential",
        commit,
        limits: Limits {
            timeout,
            source_bytes: 1024 * 1024,
            stream_bytes: 2 * 1024 * 1024,
            ..Default::default()
        },
    }
}
#[test]
#[ignore = "Linux real Git smart HTTP fixture and constrained child; no GitHub credentials"]
fn exact_history_download_uses_private_token_and_resource_limited_git_worker() {
    let d = private();
    let (repo, head) = history(d.path());
    let port_file = d.path().join("port");
    let audit = d.path().join("audit.jsonl");
    let mut server = Process(
        Command::new("/usr/bin/python3")
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/support/git-http.py"
            ))
            .arg(d.path())
            .arg(&port_file)
            .arg(&audit)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !port_file.exists() {
        assert!(server.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    let url = format!(
        "http://127.0.0.1:{}/demo.git",
        fs::read_to_string(&port_file).unwrap()
    );
    let wrapper = d.path().join("git-wrapper");
    // The operator-selected fixture executable checks production arguments,
    // then rewrites only the endpoint to the local Git HTTP backend. Product
    // configuration has no arbitrary endpoint or TLS-disable option.
    script(
        &wrapper,
        &format!(
            r#"#!/usr/bin/python3
import os, sys, resource
args=sys.argv[1:]
if 'fetch' in args:
    original='https://github.com/example-org/demo.git'
    assert original in args
    assert os.environ['GIT_CONFIG_KEY_0']=='http.'+original+'.extraHeader'
    assert os.environ['GIT_CONFIG_VALUE_0'].startswith('Authorization: Basic ')
    assert not any('synthetic-only' in arg or 'Authorization:' in arg for arg in args)
    assert os.environ['GIT_CONFIG_GLOBAL']=='/dev/null'
    assert resource.getrlimit(resource.RLIMIT_CORE)==(0,0)
    assert resource.getrlimit(resource.RLIMIT_AS)==(8589934592,8589934592)
    assert resource.getrlimit(resource.RLIMIT_CPU)[0] <= 30
    assert resource.getrlimit(resource.RLIMIT_FSIZE)==(2097152,2097152)
    assert 'http.followRedirects=false' in args
    assert 'http.sslVerify=true' in args
    assert '--no-recurse-submodules' in args
    assert 'core.hooksPath=/dev/null' in args
    endpoint={url}
    args=['-c','protocol.http.allow=always']+[endpoint if a==original else a for a in args]
    os.environ['GIT_CONFIG_KEY_0']='http.'+endpoint+'.extraHeader'
os.execv('/usr/bin/git',['git']+args)
"#,
            url = json!(url)
        ),
    );
    let output = d.path().join("download");
    let worker = Path::new(env!("CARGO_BIN_EXE_treeharbor"));
    let manifest = download::fetch(request(
        worker,
        &wrapper,
        &output,
        oid(&head),
        Duration::from_secs(30),
    ))
    .unwrap();
    assert_eq!(manifest.revision, format!("git:sha1:{head}"));
    assert_eq!(
        download::open(&output, Limits::default()).unwrap(),
        manifest
    );
    assert!(
        download::fetch(request(
            worker,
            &wrapper,
            &output,
            oid(&head),
            Duration::from_secs(30)
        ))
        .is_err()
    );
    let restored = d.path().join("native");
    let pack = treeharbor_service::pack::capture_git(treeharbor_service::pack::GitCapture {
        git: Path::new("/usr/bin/git"),
        bundle: &output.join("history.bundle"),
        commit: oid(&head),
        output: &restored,
        limits: Default::default(),
    })
    .unwrap();
    let m: Value =
        serde_json::from_slice(&fs::read(restored.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(m["source_revision"], manifest.revision);
    assert!(pack.object_count() > 5);
    let requests = fs::read_to_string(&audit).unwrap();
    assert!(requests.lines().count() >= 2);
    for line in requests.lines() {
        assert_eq!(
            serde_json::from_str::<Value>(line).unwrap()["authorized"],
            true
        );
    }
    // Neither completed download nor metadata contains the temporary token.
    for file in ["manifest.json", "history.bundle"] {
        assert!(
            !fs::read(output.join(file))
                .unwrap()
                .windows(b"synthetic-only-not-a-credential".len())
                .any(|w| w == b"synthetic-only-not-a-credential")
        );
    }
    // Reject a redirected fetch and leave no completed bundle; retry is a fresh
    // private download. The HTTP audit sees exactly the original request.
    fs::write(d.path().join("redirect"), b"redirect fixture").unwrap();
    let rejected = d.path().join("rejected");
    assert!(
        download::fetch(request(
            worker,
            &wrapper,
            &rejected,
            oid(&head),
            Duration::from_secs(30)
        ))
        .is_err()
    );
    assert!(!rejected.exists());
    assert_eq!(
        fs::read_to_string(&audit).unwrap().lines().count(),
        requests.lines().count() + 1
    );
    fs::remove_file(d.path().join("redirect")).unwrap();
    assert!(
        download::fetch(request(
            worker,
            &wrapper,
            &rejected,
            oid(&head),
            Duration::from_secs(30)
        ))
        .is_ok()
    );
    fs::remove_dir_all(repo).unwrap();
    drop(server);
}
#[test]
#[ignore = "Linux process timeout and child cleanup; no network or credentials"]
fn stalled_git_worker_cannot_publish_a_bundle_and_is_killed_at_deadline() {
    let d = private();
    let git = d.path().join("git-wrapper");
    let marker = d.path().join("pid");
    script(
        &git,
        &format!(
            r#"#!/usr/bin/python3
import os,sys,time,pathlib
if 'fetch' in sys.argv:
    pathlib.Path({marker}).write_text(str(os.getpid()))
    time.sleep(60)
os.execv('/usr/bin/git',['git']+sys.argv[1:])
"#,
            marker = json!(marker)
        ),
    );
    let output = d.path().join("download");
    let started = Instant::now();
    let result = download::fetch(request(
        Path::new(env!("CARGO_BIN_EXE_treeharbor")),
        &git,
        &output,
        GitOid::Sha1([7; 20]),
        Duration::from_secs(2),
    ));
    assert!(matches!(result, Err(treeharbor_git_bridge::Error::Limit)));
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(!output.exists());
    let pid = fs::read_to_string(marker).unwrap();
    wait_stopped(pid.trim());
}

fn wait_stopped(pid: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with("Z ") => return,
            _ => assert!(Instant::now() < deadline, "Git worker still running"),
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "Linux real parent SIGKILL and supervised Git teardown"]
fn parent_loss_stops_download_without_publishing_incomplete_files() {
    if let Some(root) = std::env::var_os("TH_GIT_DOWNLOAD_CHILD_FIXTURE") {
        let root = PathBuf::from(root);
        let _ = download::fetch(request(
            Path::new(env!("CARGO_BIN_EXE_treeharbor")),
            &root.join("git-wrapper"),
            &root.join("download"),
            GitOid::Sha1([7; 20]),
            Duration::from_secs(30),
        ));
        panic!("fixture should be killed during fetch");
    }
    let d = private();
    let marker = d.path().join("pid");
    script(
        &d.path().join("git-wrapper"),
        &format!(
            r#"#!/usr/bin/python3
import os,sys,time,pathlib
if 'fetch' in sys.argv:
    pathlib.Path({marker}).write_text(str(os.getpid())+' '+str(os.getppid()))
    time.sleep(60)
os.execv('/usr/bin/git',['git']+sys.argv[1:])
"#,
            marker = json!(marker)
        ),
    );
    let mut parent = Process(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "parent_loss_stops_download_without_publishing_incomplete_files",
            ])
            .env("TH_GIT_DOWNLOAD_CHILD_FIXTURE", d.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    let pids = loop {
        if let Ok(value) = fs::read_to_string(&marker)
            && value.split_whitespace().count() == 2
            && value
                .split_whitespace()
                .all(|p| p.parse::<u32>().is_ok_and(|p| p > 1))
        {
            break value;
        }
        assert!(parent.0.try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    };
    parent.0.kill().unwrap();
    parent.0.wait().unwrap();
    for pid in pids.split_whitespace() {
        wait_stopped(pid);
    }
    assert!(!d.path().join("download").exists());
}
