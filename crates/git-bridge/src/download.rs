//! Fixed GitHub download, separate from offline interop. The trusted executable
//! dispatches `internal-git-worker` to `worker()` before running any agent code.
use crate::{Error, Limits, Result, check_deadline, check_limits, private_parent, process};
#[cfg(target_os = "linux")]
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use treeharbor_core::{GitOid, ObjectId, RevisionRef};
use zeroize::Zeroizing;

pub struct Request<'a> {
    pub worker: &'a Path,
    pub git: &'a Path,
    pub output: &'a Path,
    pub owner: &'a str,
    pub name: &'a str,
    pub token: &'a str,
    pub commit: GitOid,
    pub limits: Limits,
}
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub version: u8,
    pub owner: String,
    pub name: String,
    pub revision: String,
    pub bundle_sha256: String,
    pub bundle_bytes: u64,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
enum Step {
    Fetch {
        owner: String,
        name: String,
        sha: String,
        token: String,
    },
    Verify {
        sha: String,
    },
    Bundle {
        output: PathBuf,
    },
}
impl Drop for Step {
    fn drop(&mut self) {
        if let Self::Fetch { token, .. } = self {
            use zeroize::Zeroize;
            token.zeroize();
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Worker {
    version: u8,
    parent_pid: u32,
    git: PathBuf,
    repo: PathBuf,
    scratch: PathBuf,
    cpu_seconds: u64,
    file_bytes: u64,
    step: Step,
}
fn component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s != "."
        && s != ".."
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}
#[cfg(target_os = "linux")]
fn sha(s: &str) -> bool {
    s.len() == 40
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn token(s: &str) -> bool {
    !s.is_empty() && s.len() <= 8192 && s.bytes().all(|b| b.is_ascii_graphic())
}
struct Guard {
    child: Child,
    finished: bool,
}
impl Drop for Guard {
    fn drop(&mut self) {
        if !self.finished
            && let Some(pid) = rustix::process::Pid::from_raw(self.child.id() as i32)
        {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
        let _ = self.child.wait();
    }
}
fn usage(root: &Path, limit: u64) -> Result<()> {
    // Git atomically renames/removes temporary objects while this soft disk
    // monitor walks the tree. Removed entries no longer consume its budget.
    fn present<T>(result: std::io::Result<T>) -> Result<Option<T>> {
        match result {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }
    let mut todo = vec![root.to_owned()];
    let mut bytes = 0u64;
    let mut entries = 0usize;
    while let Some(path) = todo.pop() {
        let Some(directory) = present(fs::read_dir(path))? else {
            continue;
        };
        for entry in directory {
            let entry = entry?;
            let Some(meta) = present(entry.file_type())? else {
                continue;
            };
            entries += 1;
            if entries > 100_000 || meta.is_symlink() {
                return Err(Error::Limit);
            }
            if meta.is_dir() {
                todo.push(entry.path());
            } else if meta.is_file() {
                let Some(metadata) = present(entry.metadata())? else {
                    continue;
                };
                bytes = bytes.checked_add(metadata.len()).ok_or(Error::Limit)?;
            } else {
                return Err(Error::Configuration);
            }
            if bytes > limit {
                return Err(Error::Limit);
            }
        }
    }
    Ok(())
}
fn run(worker: &Path, request: &Worker, deadline: Instant, max_disk: u64) -> Result<()> {
    let input = Zeroizing::new(serde_json::to_vec(request)?);
    if input.len() > 16 * 1024 {
        return Err(Error::Configuration);
    }
    let mut child = Guard {
        child: Command::new(worker)
            .arg("internal-git-worker")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .current_dir(&request.scratch)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?,
        finished: false,
    };
    let mut stdin = child.child.stdin.take().ok_or(Error::Configuration)?;
    let flags = rustix::fs::fcntl_getfl(&stdin).map_err(std::io::Error::from)?;
    rustix::fs::fcntl_setfl(&stdin, flags | rustix::fs::OFlags::NONBLOCK)
        .map_err(std::io::Error::from)?;
    let mut offset = 0;
    while offset < input.len() {
        check_deadline(deadline)?;
        match stdin.write(&input[offset..]) {
            Ok(0) => return Err(Error::Git("download worker input")),
            Ok(n) => offset += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(e) => return Err(e.into()),
        }
    }
    drop(stdin);
    loop {
        check_deadline(deadline)?;
        usage(&request.scratch, max_disk)?;
        if let Some(status) = child.child.try_wait()? {
            // A failed supervisor may still have live Git descendants.
            child.finished = status.success();
            return if status.success() {
                Ok(())
            } else {
                Err(Error::Git("isolated download worker"))
            };
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn digest(mut f: File, limit: u64, deadline: Instant) -> Result<(String, u64)> {
    let mut hash = Sha256::new();
    let mut bytes = 0;
    let mut buffer = [0; 65536];
    loop {
        check_deadline(deadline)?;
        let n = f.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        bytes += n as u64;
        if bytes > limit {
            return Err(Error::Limit);
        }
        hash.update(&buffer[..n]);
    }
    Ok((ObjectId::from_bytes(hash.finalize().into()).hex(), bytes))
}
/// The caller must retain its concurrency permit until this blocking function
/// exits. Resource limits apply per Git process on Linux. Disk usage is monitored
/// with possible overshoot; a hard aggregate limit requires an external volume quota.
pub fn fetch(request: Request<'_>) -> Result<Manifest> {
    if !cfg!(target_os = "linux") {
        return Err(Error::Unsupported(
            "GitHub download worker currently requires Linux",
        ));
    }
    check_limits(request.limits, request.git)?;
    let GitOid::Sha1(oid) = request.commit else {
        return Err(Error::Unsupported(
            "GitHub download currently supports SHA-1 repositories",
        ));
    };
    if !component(request.owner)
        || !component(request.name)
        || !token(request.token)
        || !request.worker.is_absolute()
        || !request.worker.is_file()
    {
        return Err(Error::Configuration);
    }
    let revision = RevisionRef::Git(GitOid::Sha1(oid)).to_string();
    let sha = revision
        .strip_prefix("git:sha1:")
        .ok_or(Error::Configuration)?
        .to_owned();
    let parent = private_parent(request.output)?;
    let name = request.output.file_name().ok_or(Error::Configuration)?;
    if request.output.symlink_metadata().is_ok() {
        return Err(Error::Configuration);
    }
    let stage = tempfile::Builder::new()
        .prefix(".github-fetch-")
        .tempdir_in(&parent)?;
    fs::set_permissions(stage.path(), fs::Permissions::from_mode(0o700))?;
    let repo = stage.path().join("source.git");
    fs::create_dir(&repo)?;
    let deadline = Instant::now() + request.limits.timeout;
    let git = process::Git::new(request.git.canonicalize()?, stage.path(), deadline);
    let template = stage.path().join("empty-template");
    fs::create_dir(&template)?;
    git.run(
        Some(&repo),
        "init download",
        &[
            "init".into(),
            "--bare".into(),
            "--quiet".into(),
            format!("--template={}", template.display()).into(),
            repo.as_os_str().to_owned(),
        ],
        None,
        4096,
    )?;
    let max_disk = request
        .limits
        .stream_bytes
        .checked_mul(3)
        .ok_or(Error::Limit)?;
    let make = |step| Worker {
        version: 1,
        parent_pid: std::process::id(),
        git: request.git.to_owned(),
        repo: repo.clone(),
        scratch: stage.path().to_owned(),
        cpu_seconds: request.limits.timeout.as_secs().max(1),
        file_bytes: request.limits.stream_bytes,
        step,
    };
    let mut fetch = make(Step::Fetch {
        owner: request.owner.into(),
        name: request.name.into(),
        sha: sha.clone(),
        token: request.token.into(),
    });
    let fetched = run(request.worker, &fetch, deadline, max_disk);
    if let Step::Fetch { token, .. } = &mut fetch.step {
        use zeroize::Zeroize;
        token.zeroize();
    }
    fetched?;
    run(
        request.worker,
        &make(Step::Verify { sha: sha.clone() }),
        deadline,
        max_disk,
    )?;
    let bundle = stage.path().join("history.bundle");
    run(
        request.worker,
        &make(Step::Bundle {
            output: bundle.clone(),
        }),
        deadline,
        max_disk,
    )?;
    let (bundle_sha256, bundle_bytes) =
        digest(File::open(&bundle)?, request.limits.stream_bytes, deadline)?;
    let manifest = Manifest {
        version: 1,
        owner: request.owner.into(),
        name: request.name.into(),
        revision,
        bundle_sha256,
        bundle_bytes,
    };
    fs::set_permissions(&bundle, fs::Permissions::from_mode(0o600))?;
    File::open(&bundle)?.sync_all()?;
    // Failed areas are never a warm cache; only the complete verified bundle is retained.
    fs::remove_dir_all(&repo)?;
    fs::remove_dir(&template)?;
    let metadata = stage.path().join("manifest.json");
    let mut file = File::create(&metadata)?;
    fs::set_permissions(&metadata, fs::Permissions::from_mode(0o600))?;
    serde_json::to_writer(&mut file, &manifest)?;
    file.sync_all()?;
    File::open(stage.path())?.sync_all()?;
    check_deadline(deadline)?;
    let parent_fd = File::open(&parent)?;
    rustix::fs::renameat_with(
        &parent_fd,
        stage.path().file_name().ok_or(Error::Configuration)?,
        &parent_fd,
        name,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .map_err(std::io::Error::from)?;
    parent_fd.sync_all()?;
    Ok(manifest)
}

/// Reopen only a complete bounded download. The caller still verifies the full
/// Git graph during conversion and requires fresh upstream access for a new import.
pub fn open(directory: &Path, limits: Limits) -> Result<Manifest> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    let meta = fs::symlink_metadata(directory)?;
    if !meta.is_dir()
        || meta.uid() != rustix::process::geteuid().as_raw()
        || meta.mode() & 0o077 != 0
    {
        return Err(Error::Configuration);
    }
    let read = |name: &str, max: u64| -> Result<File> {
        let f = fs::OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(directory.join(name))?;
        let m = f.metadata()?;
        if !m.is_file()
            || m.uid() != rustix::process::geteuid().as_raw()
            || m.mode() & 0o077 != 0
            || m.nlink() != 1
            || m.len() > max
        {
            return Err(Error::Configuration);
        }
        Ok(f)
    };
    let mut bytes = Vec::new();
    read("manifest.json", 16384)?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(Error::Limit);
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    if manifest.version != 1
        || !component(&manifest.owner)
        || !component(&manifest.name)
        || !matches!(
            manifest.revision.parse::<RevisionRef>(),
            Ok(RevisionRef::Git(_))
        )
    {
        return Err(Error::Configuration);
    }
    let (hash, size) = digest(
        read("history.bundle", limits.stream_bytes)?,
        limits.stream_bytes,
        Instant::now() + limits.timeout,
    )?;
    if hash != manifest.bundle_sha256 || size != manifest.bundle_bytes {
        return Err(Error::InvalidProjection);
    }
    Ok(manifest)
}
/// Entry point for a dedicated child, never call in a serving process. Receives
/// private bounded JSON through stdin, applies hard per-process limits and supervises
/// trusted Git with fixed arguments and a cleared environment. No shell involved.
pub fn worker() -> Result<()> {
    #[cfg(not(target_os = "linux"))]
    {
        Err(Error::Unsupported(
            "GitHub download worker currently requires Linux",
        ))
    }
    #[cfg(target_os = "linux")]
    {
        use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};
        setrlimit(
            Resource::Core,
            Rlimit {
                current: Some(0),
                maximum: Some(0),
            },
        )
        .map_err(std::io::Error::from)?;
        let mut bytes = Zeroizing::new(Vec::new());
        std::io::stdin()
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > 16 * 1024 {
            return Err(Error::Configuration);
        }
        let mut r: Worker = serde_json::from_slice(&bytes)?;
        use zeroize::Zeroize;
        bytes.zeroize();
        if r.version != 1
            || rustix::process::getppid().map(|pid| pid.as_raw() as u32) != Some(r.parent_pid)
            || rustix::process::getpgid(None).map_err(std::io::Error::from)?
                != rustix::process::getpid()
            || !(1..=3600).contains(&r.cpu_seconds)
            || !(22..=128 << 30).contains(&r.file_bytes)
            || !r.git.is_absolute()
            || !r.git.is_file()
            || !r.repo.is_absolute()
            || !r.scratch.is_absolute()
            || r.repo.parent() != Some(r.scratch.as_path())
        {
            return Err(Error::Configuration);
        }
        private_parent(&r.repo)?;
        for (resource, value) in [
            (Resource::Cpu, r.cpu_seconds),
            (Resource::As, 8 << 30),
            (Resource::Fsize, r.file_bytes),
            (Resource::Nofile, 256),
        ] {
            let prior = getrlimit(resource);
            let value = prior
                .current
                .unwrap_or(value)
                .min(prior.maximum.unwrap_or(value))
                .min(value);
            setrlimit(
                resource,
                Rlimit {
                    current: Some(value),
                    maximum: Some(value),
                },
            )
            .map_err(std::io::Error::from)?;
        }
        let mut command = Command::new(&r.git);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &r.scratch)
            .env("XDG_CONFIG_HOME", &r.scratch)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_DIR", &r.repo)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("LC_ALL", "C")
            .current_dir(&r.repo)
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "credential.helper=",
                "-c",
                "protocol.allow=never",
                "-c",
                "gc.auto=0",
                "-c",
                "maintenance.auto=false",
                "-c",
                "pack.threads=1",
                "-c",
                "pack.window=0",
                "-c",
                "pack.deltaCacheSize=1m",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        match &mut r.step {
            Step::Fetch {
                owner,
                name,
                sha: commit,
                token: password,
            } => {
                if !component(owner) || !component(name) || !sha(commit) || !token(password) {
                    return Err(Error::Configuration);
                }
                let url = format!("https://github.com/{owner}/{name}.git");
                let plain = Zeroizing::new(format!("x-access-token:{password}"));
                let encoded = Zeroizing::new(
                    base64::engine::general_purpose::STANDARD.encode(plain.as_bytes()),
                );
                let header = Zeroizing::new(format!("Authorization: Basic {}", encoded.as_str()));
                command
                    .env("GIT_CONFIG_COUNT", "1")
                    .env("GIT_CONFIG_KEY_0", format!("http.{url}.extraHeader"))
                    .env("GIT_CONFIG_VALUE_0", header.as_str())
                    .args([
                        "-c",
                        "protocol.https.allow=always",
                        "-c",
                        "http.followRedirects=false",
                        "-c",
                        "http.sslVerify=true",
                        "-c",
                        "http.proxy=",
                        "-c",
                        "http.saveCookies=false",
                        "-c",
                        "fetch.fsckObjects=true",
                        "-c",
                        "transfer.credentialsInUrl=die",
                        "fetch",
                        "--quiet",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--no-write-fetch-head",
                        "--no-auto-maintenance",
                        "--refmap=",
                    ])
                    .arg(url)
                    .arg(format!("+{commit}:refs/treeharbor/import"));
                password.zeroize();
            }
            Step::Verify { sha: commit } => {
                if !sha(commit) {
                    return Err(Error::Configuration);
                }
                command.args(["fsck", "--strict", "--no-reflogs", "--no-dangling", commit]);
            }
            Step::Bundle { output } => {
                if output.parent() != Some(r.scratch.as_path())
                    || output.file_name() != Some(std::ffi::OsStr::new("history.bundle"))
                {
                    return Err(Error::Configuration);
                }
                command
                    .args(["bundle", "create", "--quiet"])
                    .arg(output)
                    .arg("refs/treeharbor/import");
            }
        }
        // Keep this supervisor in the parent's dedicated process group. Its Git
        // children inherit that group and the resource limits. Parent loss does
        // not leave a fetch waiting indefinitely on the network.
        let deadline = Instant::now() + Duration::from_secs(r.cpu_seconds);
        let mut child = command.spawn()?;
        loop {
            let parent_alive =
                rustix::process::getppid().map(|pid| pid.as_raw() as u32) == Some(r.parent_pid);
            if !parent_alive
                || check_deadline(deadline).is_err()
                || usage(&r.scratch, r.file_bytes.saturating_mul(3)).is_err()
            {
                // Includes this dedicated supervisor; no serving process calls worker().
                let _ = rustix::process::kill_process_group(
                    rustix::process::getpid(),
                    rustix::process::Signal::KILL,
                );
                let _ = child.kill();
                let _ = child.wait();
                return Err(Error::Limit);
            }
            match child.try_wait()? {
                Some(status) if status.success() => return Ok(()),
                Some(_) => return Err(Error::Git("isolated download worker")),
                None => std::thread::sleep(Duration::from_millis(25)),
            }
        }
    }
}
