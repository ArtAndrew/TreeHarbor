//! This is a namespace init, not a sandbox or a host process runner.
use std::{ffi::OsString, path::Path, time::Duration};

struct Task {
    timeout: Duration,
    processes: u64,
    uid: u32,
    gid: u32,
    command: Vec<OsString>,
}

impl Task {
    fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Self, &'static str> {
        // Bound argument count and bytes before retaining the whole vector. These
        // are operational limits; they are unrelated to product licensing.
        let mut bounded = Vec::new();
        let mut bytes = 0_usize;
        for arg in args {
            bytes = bytes.checked_add(arg.len()).ok_or("argument limit")?;
            if bounded.len() >= 140 || bytes > 65_536 {
                return Err("argument limit");
            }
            bounded.push(arg);
        }
        let args = bounded;
        if args.len() < 12
            || args[0] != "--protocol"
            || args[1] != "1"
            || args[2] != "--timeout-ms"
            || args[4] != "--max-processes"
            || args[6] != "--uid"
            || args[8] != "--gid"
            || args[10] != "--"
        {
            return Err("expected protocol 1, timeout, process limit and command");
        }
        let number = |arg: &OsString| -> Result<u64, &'static str> {
            let value = arg.to_str().ok_or("invalid numeric argument")?;
            if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
                return Err("invalid numeric argument");
            }
            value.parse().map_err(|_| "invalid numeric argument")
        };
        let milliseconds = number(&args[3])?;
        let processes = number(&args[5])?;
        if !(100..=86_400_000).contains(&milliseconds) || !(4..=4096).contains(&processes) {
            return Err("limit out of range");
        }
        let uid = number(&args[7])?;
        let gid = number(&args[9])?;
        if [uid, gid]
            .into_iter()
            .any(|id| !(1..=2_147_483_647).contains(&id) || id == 65534)
        {
            return Err("task UID/GID must be non-root and distinct from supervisor 65534");
        }
        if !Path::new(&args[11]).is_absolute() {
            return Err("command must be absolute");
        }
        Ok(Self {
            timeout: Duration::from_millis(milliseconds),
            processes,
            uid: uid as u32,
            gid: gid as u32,
            command: args.into_iter().skip(11).collect(),
        })
    }
}

#[cfg(target_os = "linux")]
fn supervise(task: Task) -> Result<i32, Box<dyn std::error::Error>> {
    use rustix::process::{
        DumpableBehavior, Resource, Rlimit, WaitOptions, getgid, getpid, getuid,
        set_dumpable_behavior, setrlimit, wait,
    };
    use std::{os::unix::process::CommandExt, process::Command, thread, time::Instant};

    // Only the trusted bootstrap starts as root, with SETUID/SETGID and nothing
    // else. The untrusted command never executes with these credentials.
    if getpid().as_raw_nonzero().get() != 1 || !getuid().is_root() || !getgid().is_root() {
        return Err("requires PID 1 and restricted root bootstrap in an external sandbox".into());
    }
    let status = std::fs::read_to_string("/proc/self/status")?;
    // Include saved and filesystem IDs, not only the real UID. A saved root ID
    // could otherwise be regained without a setuid executable or new capability.
    for (key, expected) in [
        ("Uid:", getuid().to_string()),
        ("Gid:", getgid().to_string()),
    ] {
        let expected = [expected.as_str(); 4];
        if !status.lines().any(|line| {
            line.strip_prefix(key)
                .is_some_and(|value| value.split_whitespace().eq(expected))
        }) {
            return Err("requires matching real/effective/saved/filesystem IDs".into());
        }
    }
    let group = getgid().to_string();
    if !status.lines().any(|line| {
        line.strip_prefix("Groups:")
            .is_some_and(|value| value.split_whitespace().all(|value| value == group))
    }) {
        return Err("additional groups are not permitted".into());
    }
    for (key, expected) in [
        ("CapInh:", "0000000000000000"),
        ("CapPrm:", "00000000000000c0"),
        ("CapEff:", "00000000000000c0"),
        ("CapBnd:", "00000000000000c0"),
        ("CapAmb:", "0000000000000000"),
        ("NoNewPrivs:", "1"),
        ("Threads:", "1"),
    ] {
        if !status.lines().any(|line| {
            line.strip_prefix(key)
                .is_some_and(|value| value.trim() == expected)
        }) {
            return Err("requires only SETUID/SETGID, one thread and no-new-privileges".into());
        }
    }
    if !rustix::thread::capabilities_secure_bits()?.is_empty() {
        return Err("non-default securebits could retain bootstrap capabilities".into());
    }
    set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
    setrlimit(
        Resource::Core,
        Rlimit {
            current: Some(0),
            maximum: Some(0),
        },
    )?;
    setrlimit(
        Resource::Nproc,
        Rlimit {
            current: Some(task.processes),
            maximum: Some(task.processes),
        },
    )?;
    let started = Instant::now();
    let child = Command::new(&task.command[0])
        .args(&task.command[1..])
        .env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", "/workspace")
        .env("TMPDIR", "/tmp")
        // Rust clears supplementary groups, sets gid and uid, then changes cwd
        // and execs. All real/effective/saved IDs change; capabilities are lost.
        .gid(task.gid)
        .uid(task.uid)
        .current_dir("/workspace")
        .spawn()?;
    let primary = child.id();
    // The supervisor and task must not share a UID: tested gVisor allows a
    // same-UID task to signal its container PID 1. This single-threaded process
    // irrevocably drops its own credentials after spawning the unprivileged task.
    rustix::thread::set_thread_groups(&[])?;
    rustix::thread::set_thread_gid(rustix::process::Gid::from_raw(65534))?;
    rustix::thread::set_thread_uid(rustix::process::Uid::from_raw(65534))?;
    set_dumpable_behavior(DumpableBehavior::NotDumpable)?;
    let status = std::fs::read_to_string("/proc/self/status")?;
    for key in ["CapPrm:", "CapEff:", "CapAmb:"] {
        if !status.lines().any(|line| {
            line.strip_prefix(key)
                .is_some_and(|value| value.trim() == "0000000000000000")
        }) {
            return Err("supervisor privilege drop failed".into());
        }
    }
    // wait() reaps the primary and adopted descendants. Using Child::try_wait
    // as well would introduce two independent consumers of the primary status.
    drop(child);
    loop {
        if started.elapsed() >= task.timeout {
            // Exiting PID 1 makes the external runtime kill the entire namespace,
            // including descendants that changed session or process group.
            return Ok(124);
        }
        match wait(WaitOptions::NOHANG) {
            Ok(Some((pid, status))) => {
                if pid.as_raw_nonzero().get() as u32 == primary {
                    return status
                        .exit_status()
                        .or_else(|| status.terminating_signal().map(|signal| 128 + signal))
                        .ok_or_else(|| "unexpected primary wait status".into());
                }
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn supervise(task: Task) -> Result<i32, Box<dyn std::error::Error>> {
    let _ = (
        task.timeout,
        task.processes,
        task.uid,
        task.gid,
        task.command,
    );
    Err("runtime init supports Linux only".into())
}

fn main() {
    let result = Task::parse(std::env::args_os().skip(1))
        .map_err(Box::<dyn std::error::Error>::from)
        .and_then(supervise);
    match result {
        Ok(code) => std::process::exit(code),
        Err(error) => {
            eprintln!("treeharbor-runtime-init: {error}");
            std::process::exit(125);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Task, &'static str> {
        Task::parse(args.iter().map(OsString::from))
    }

    #[test]
    fn command_protocol_rejects_ambiguous_or_unbounded_input() {
        let valid = [
            "--protocol",
            "1",
            "--timeout-ms",
            "1000",
            "--max-processes",
            "64",
            "--uid",
            "1001",
            "--gid",
            "1001",
            "--",
            "/probe",
        ];
        assert_eq!(parse(&valid).unwrap().timeout, Duration::from_secs(1));
        for (index, value) in [
            (1, "2"),
            (3, "99"),
            (3, "86400001"),
            (3, "+1000"),
            (3, "-1"),
            (5, "3"),
            (5, "4097"),
            (7, "0"),
            (7, "65534"),
            (7, "4294967295"),
            (9, "0"),
            (10, "--other"),
            (11, "probe"),
        ] {
            let mut invalid = valid;
            invalid[index] = value;
            assert!(parse(&invalid).is_err());
        }
        assert!(parse(&valid[..11]).is_err());
        let mut oversized: Vec<_> = valid.into_iter().map(OsString::from).collect();
        oversized.push(OsString::from("x".repeat(65_536)));
        assert!(Task::parse(oversized).is_err());
        assert!(Task::parse((0..141).map(|_| OsString::from("x"))).is_err());
    }
}
