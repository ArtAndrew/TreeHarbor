use crate::{Error, Result, check_deadline};
use std::{
    ffi::OsString,
    fs::File,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

// Kill the entire private process group, including pack-objects. No shell.
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
pub(crate) struct Git<'a> {
    executable: PathBuf,
    scratch: &'a Path,
    deadline: Instant,
}
impl<'a> Git<'a> {
    pub fn new(executable: PathBuf, scratch: &'a Path, deadline: Instant) -> Self {
        Self {
            executable,
            scratch,
            deadline,
        }
    }
    pub fn run(
        &self,
        repo: Option<&Path>,
        label: &'static str,
        args: &[OsString],
        input: Option<&Path>,
        max_output: u64,
    ) -> Result<()> {
        check_deadline(self.deadline)?;
        let stdout = tempfile::tempfile_in(self.scratch)?;
        let stderr = tempfile::tempfile_in(self.scratch)?;
        let mut command = Command::new(&self.executable);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.scratch)
            .env("XDG_CONFIG_HOME", self.scratch)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CEILING_DIRECTORIES", self.scratch)
            .env("GIT_NO_REPLACE_OBJECTS", "1")
            .env("LC_ALL", "C")
            .current_dir(repo.unwrap_or(self.scratch))
            .process_group(0)
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
            .args(args)
            .stdin(match input {
                Some(p) => Stdio::from(File::open(p)?),
                None => Stdio::null(),
            })
            .stdout(stdout.try_clone()?)
            .stderr(stderr.try_clone()?);
        if let Some(repo) = repo {
            command.env("GIT_DIR", repo);
        }
        let mut child = Guard {
            child: command.spawn()?,
            finished: false,
        };
        loop {
            check_deadline(self.deadline)?;
            if stdout.metadata()?.len() > max_output || stderr.metadata()?.len() > 1 << 20 {
                return Err(Error::Limit);
            }
            if let Some(status) = child.child.try_wait()? {
                child.finished = true;
                if stdout.metadata()?.len() > max_output || stderr.metadata()?.len() > 1 << 20 {
                    return Err(Error::Limit);
                }
                return if status.success() {
                    Ok(())
                } else {
                    Err(Error::Git(label))
                };
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
