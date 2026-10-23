#![cfg(target_os = "linux")]
#[path = "support/git.rs"]
mod git_reference;
use serde_json::json;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use treeharbor_core::{
    ChangeOperation, Changes, Commit, Directory, Entry, EntryMode, EntryRef, GitOid, Name, Object,
    ObjectId, ObjectStore, RepoPath, RevisionRef,
};
use treeharbor_git_bridge::{
    Limits,
    publish::{self, Prepared, Push},
};
use treeharbor_storage::{LocalStore, TenantId};
use uuid::Uuid;
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
fn script(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn native(store: &LocalStore, body: &[u8]) -> ObjectId {
    let dir = store
        .put(&Object::Directory(Directory::new(vec![]).unwrap()))
        .unwrap();
    let snapshot = store.put(&Object::Snapshot { root: dir }).unwrap();
    let changes = store
        .put(&Object::ChangeOperations(Changes::new(vec![]).unwrap()))
        .unwrap();
    let base = Commit {
        parents: vec![],
        snapshot,
        changes,
        author_name: "Fixture".into(),
        author_email: "fixture@example.invalid".into(),
        timestamp_seconds: 1,
        timezone_minutes: 0,
        message: "base".into(),
        git_origin: None,
    };
    let parent = store.put(&Object::NativeCommit(base.clone())).unwrap();
    let blob = store.put(&Object::Blob(body.to_vec())).unwrap();
    let entry = EntryRef {
        mode: EntryMode::File,
        target: blob,
    };
    let root = store
        .put(&Object::Directory(
            Directory::new(vec![Entry {
                name: Name::new(b"file".to_vec()).unwrap(),
                value: entry,
            }])
            .unwrap(),
        ))
        .unwrap();
    let snapshot = store.put(&Object::Snapshot { root }).unwrap();
    let changes = store
        .put(&Object::ChangeOperations(
            Changes::new(vec![ChangeOperation {
                path: RepoPath::new(b"file".to_vec()).unwrap(),
                expected: None,
                replacement: Some(entry),
            }])
            .unwrap(),
        ))
        .unwrap();
    store
        .put(&Object::NativeCommit(Commit {
            parents: vec![parent],
            snapshot,
            changes,
            message: "change".into(),
            ..base
        }))
        .unwrap()
}
struct Fixture {
    dir: tempfile::TempDir,
    repo: PathBuf,
    wrapper: PathBuf,
    store: LocalStore,
    _server: Process,
}
impl Fixture {
    fn new() -> Self {
        let dir = private();
        let repo = dir.path().join("demo.git");
        git_reference::git(
            dir.path(),
            &["init", "--bare", "--template=", repo.to_str().unwrap()],
            b"",
        );
        git_reference::git(&repo, &["config", "http.receivepack", "true"], b"");
        let tree = String::from_utf8(git_reference::git(&repo, &["mktree"], b"")).unwrap();
        let main=git_reference::hash(&repo,"commit",format!("tree {}\nauthor Fixture <fixture@example.invalid> 1 +0000\ncommitter Fixture <fixture@example.invalid> 1 +0000\n\nmain\n",tree.trim()).as_bytes());
        git_reference::bundle(&repo, &dir.path().join("original-main.bundle"), &main);
        let port = dir.path().join("port");
        let audit = dir.path().join("audit.jsonl");
        let mut server = Process(
            Command::new("/usr/bin/python3")
                .arg(concat!(
                    env!("CARGO_MANIFEST_DIR"),
                    "/tests/support/git-http.py"
                ))
                .arg(dir.path())
                .arg(&port)
                .arg(audit)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let until = Instant::now() + Duration::from_secs(5);
        while !port.exists() {
            assert!(server.0.try_wait().unwrap().is_none());
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let url = format!(
            "http://127.0.0.1:{}/demo.git",
            fs::read_to_string(port).unwrap()
        );
        let wrapper = dir.path().join("git-wrapper");
        script(
            &wrapper,
            &format!(
                r#"#!/usr/bin/python3
import os, sys, resource, pathlib, time, subprocess
args=sys.argv[1:]
if 'push' in args:
    original='https://github.com/example-org/demo.git'
    assert original in args
    assert os.environ['GIT_CONFIG_KEY_0']=='http.'+original+'.extraHeader'
    assert os.environ['GIT_CONFIG_VALUE_0'].startswith('Authorization: Basic ')
    assert not any('synthetic-only' in arg or 'Authorization:' in arg for arg in args)
    assert os.environ['GIT_CONFIG_GLOBAL']=='/dev/null'
    assert resource.getrlimit(resource.RLIMIT_CORE)==(0,0)
    assert resource.getrlimit(resource.RLIMIT_AS)==(8589934592,8589934592)
    assert resource.getrlimit(resource.RLIMIT_CPU)[0] <= 30
    assert 'http.followRedirects=false' in args and 'http.sslVerify=true' in args
    assert '--no-verify' in args and '--no-recurse-submodules' in args
    assert 'core.hooksPath=/dev/null' in args
    assert '--force' not in args and '--mirror' not in args and '--all' not in args
    leases=[a for a in args if a.startswith('--force-with-lease=')]
    assert len(leases)==1 and leases[0].startswith('--force-with-lease=refs/heads/treeharbor/')
    assert args[-1].count(':')==1 and args[-1].split(':')[1].startswith('refs/heads/treeharbor/')
    root=pathlib.Path({root})
    if (root/'stall-push').exists():
        child=subprocess.Popen(['/usr/bin/sleep','60'])
        (root/'push-pids').write_text(str(os.getpid())+' '+str(os.getppid())+' '+str(child.pid))
        time.sleep(60)
    endpoint={url}
    args=['-c','protocol.http.allow=always']+[endpoint if a==original else a for a in args]
    os.environ['GIT_CONFIG_KEY_0']='http.'+endpoint+'.extraHeader'
os.execv('/usr/bin/git',['git']+args)
"#,
                url = json!(url),
                root = json!(dir.path())
            ),
        );
        let store =
            LocalStore::open(dir.path(), &TenantId::new("publisher-fixture").unwrap()).unwrap();
        Self {
            dir,
            repo,
            wrapper,
            store,
            _server: server,
        }
    }
    fn prepare(&self, body: &[u8]) -> Prepared {
        self.prepare_with_timeout(body, Duration::from_secs(30))
    }
    fn prepare_with_timeout(&self, body: &[u8], timeout: Duration) -> Prepared {
        publish::prepare(
            &self.store,
            &self.wrapper,
            self.dir.path(),
            Uuid::from_bytes([7; 16]),
            native(&self.store, body),
            Limits {
                timeout,
                source_bytes: 1 << 20,
                stream_bytes: 2 << 20,
                ..Default::default()
            },
        )
        .unwrap()
    }
    fn push(&self, p: Prepared, expected: Option<GitOid>) -> treeharbor_git_bridge::Result<()> {
        push(p, expected)
    }
    fn head(&self, branch: &str) -> String {
        String::from_utf8(git_reference::git(&self.repo, &["rev-parse", branch], b""))
            .unwrap()
            .trim()
            .to_owned()
    }
}
fn push(p: Prepared, expected: Option<GitOid>) -> treeharbor_git_bridge::Result<()> {
    p.push(Push {
        worker: Path::new(env!("CARGO_BIN_EXE_treeharbor")),
        owner: "example-org",
        name: "demo",
        token: "synthetic-only-not-a-credential",
        expected,
    })
}
fn sha(id: GitOid) -> String {
    RevisionRef::Git(id)
        .to_string()
        .trim_start_matches("git:sha1:")
        .to_owned()
}

#[test]
#[ignore = "Linux real smart HTTP push, exact lease and lost applied response; synthetic token only"]
fn publisher_pushes_one_exact_branch_rejects_stale_writers_and_retains_uncertain_outcome() {
    let f = Fixture::new();
    let main = f.head("refs/heads/main");
    let a = f.prepare(b"one\n");
    let branch = a.branch().to_owned();
    let one = a.git_oid();
    f.push(a, None).unwrap();
    assert_eq!(f.head(&branch), sha(one));
    assert!(f.push(f.prepare(b"two\n"), None).is_err());
    assert_eq!(f.head(&branch), sha(one));
    let b = f.prepare(b"two\n");
    let two = b.git_oid();
    f.push(b, Some(one)).unwrap();
    assert_eq!(f.head(&branch), sha(two));
    assert!(f.push(f.prepare(b"stale\n"), Some(one)).is_err());
    assert_eq!(f.head(&branch), sha(two));
    let c = f.prepare(b"accepted despite response loss\0\xff");
    let three = c.git_oid();
    fs::write(f.dir.path().join("drop-push-response"), b"").unwrap();
    assert!(matches!(
        f.push(c, Some(two)),
        Err(treeharbor_git_bridge::Error::PublicationUnknown)
    ));
    assert_eq!(f.head(&branch), sha(three));
    assert_eq!(f.head("refs/heads/main"), main);
    assert_eq!(
        git_reference::git(&f.repo, &["show", &format!("{branch}:file")], b""),
        b"accepted despite response loss\0\xff"
    );
    git_reference::git(&f.repo, &["fsck", "--strict"], b"");
    let audit = fs::read_to_string(f.dir.path().join("audit.jsonl")).unwrap();
    let entries: Vec<serde_json::Value> = audit
        .lines()
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    assert!(entries.iter().all(|v| v["authorized"] == true));
    assert_eq!(entries.iter().filter(|v| v["method"] == "POST").count(), 3); // no blind retry after response loss
    assert!(
        !fs::read_to_string(f.repo.join("config"))
            .unwrap()
            .contains("synthetic-only")
    );
}

#[test]
#[ignore = "Linux publisher refuses redirected endpoint and tampered private bundle before any remote mutation"]
fn publisher_refuses_redirects_and_tampered_artifact_without_changing_refs() {
    let f = Fixture::new();
    let main = f.head("refs/heads/main");
    let p = f.prepare(b"new");
    fs::write(f.dir.path().join("redirect"), b"").unwrap();
    assert!(f.push(p, None).is_err());
    let audit = fs::read_to_string(f.dir.path().join("audit.jsonl")).unwrap();
    assert_eq!(audit.lines().count(), 1);
    fs::remove_file(f.dir.path().join("redirect")).unwrap();
    let p = f.prepare(b"new");
    let artifact = fs::read_dir(f.dir.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".treeharbor-publish-")
        })
        .unwrap();
    fs::write(artifact.join("projection/history.bundle"), b"tampered").unwrap();
    assert!(matches!(
        f.push(p, None),
        Err(treeharbor_git_bridge::Error::InvalidProjection)
    ));
    assert_eq!(
        fs::read_to_string(f.dir.path().join("audit.jsonl")).unwrap(),
        audit
    );
    assert_eq!(f.head("refs/heads/main"), main);
    assert_eq!(
        String::from_utf8(git_reference::git(
            &f.repo,
            &["for-each-ref", "--format=%(refname)"],
            b""
        ))
        .unwrap()
        .trim(),
        "refs/heads/main"
    );
}

#[test]
#[ignore = "Linux competing smart HTTP pushes use the same expected old commit; only one wins"]
fn concurrent_publishers_cannot_both_replace_the_same_expected_commit() {
    let f = Fixture::new();
    let initial = f.prepare(b"initial");
    let branch = initial.branch().to_owned();
    let old = initial.git_oid();
    let main = f.head("refs/heads/main");
    f.push(initial, None).unwrap();
    let a = f.prepare(b"agent A");
    let b = f.prepare(b"agent B");
    let a_oid = sha(a.git_oid());
    let b_oid = sha(b.git_oid());
    let start = std::sync::Barrier::new(2);
    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| {
            start.wait();
            push(a, Some(old))
        });
        let b = scope.spawn(|| {
            start.wait();
            push(b, Some(old))
        });
        (a.join().unwrap(), b.join().unwrap())
    });
    assert_ne!(a.is_ok(), b.is_ok());
    assert_eq!(f.head(&branch), if a.is_ok() { a_oid } else { b_oid });
    assert_eq!(f.head("refs/heads/main"), main);
    git_reference::git(&f.repo, &["fsck", "--strict"], b"");
}

fn wait_stopped(pid: &str) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Ok(stat) if stat.rsplit_once(") ").unwrap().1.starts_with("Z ") => return,
            _ => assert!(
                Instant::now() < deadline,
                "publisher descendant still running"
            ),
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
#[ignore = "Linux stalled publisher is stopped at its deadline, including descendants"]
fn stalled_push_stops_at_deadline_without_changing_remote_refs() {
    let f = Fixture::new();
    let main = f.head("refs/heads/main");
    let p = f.prepare_with_timeout(b"stalled", Duration::from_secs(3));
    fs::write(f.dir.path().join("stall-push"), b"").unwrap();
    let started = Instant::now();
    assert!(matches!(
        f.push(p, None),
        Err(treeharbor_git_bridge::Error::PublicationUnknown)
    ));
    assert!(started.elapsed() < Duration::from_secs(8));
    for pid in fs::read_to_string(f.dir.path().join("push-pids"))
        .unwrap()
        .split_whitespace()
    {
        wait_stopped(pid);
    }
    assert_eq!(f.head("refs/heads/main"), main);
    assert!(!f.dir.path().join("audit.jsonl").exists());
}

#[test]
#[ignore = "Linux real publisher parent SIGKILL stops the supervised push process group"]
fn parent_loss_stops_push_descendants_before_network_effects() {
    if let Some(root) = std::env::var_os("TH_GIT_PUBLISH_CHILD_FIXTURE") {
        let root = PathBuf::from(root);
        let store = LocalStore::open(&root, &TenantId::new("publisher-fixture").unwrap()).unwrap();
        let p = publish::prepare(
            &store,
            &root.join("git-wrapper"),
            &root,
            Uuid::from_bytes([7; 16]),
            native(&store, b"stalled"),
            Limits {
                timeout: Duration::from_secs(30),
                source_bytes: 1 << 20,
                stream_bytes: 2 << 20,
                ..Default::default()
            },
        )
        .unwrap();
        let _ = push(p, None);
        panic!("fixture should be killed during push");
    }
    let f = Fixture::new();
    let main = f.head("refs/heads/main");
    fs::write(f.dir.path().join("stall-push"), b"").unwrap();
    let mut parent = Process(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "parent_loss_stops_push_descendants_before_network_effects",
            ])
            .env("TH_GIT_PUBLISH_CHILD_FIXTURE", f.dir.path())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(15);
    let pids = loop {
        if let Ok(value) = fs::read_to_string(f.dir.path().join("push-pids"))
            && value.split_whitespace().count() == 3
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
    assert_eq!(f.head("refs/heads/main"), main);
    assert!(!f.dir.path().join("audit.jsonl").exists());
}
