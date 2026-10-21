//! This is a namespace init, not a sandbox or a host process runner.
use std::{ffi::OsString, path::Path, time::Duration};

struct Task {
    timeout: Duration,
    processes: u64,
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
            if bounded.len() >= 136 || bytes > 65_536 {
                return Err("argument limit");
            }
            bounded.push(arg);
        }
        let args = bounded;
        if args.len() < 8
            || args[0] != "--protocol"
            || args[1] != "1"
            || args[2] != "--timeout-ms"
            || args[4] != "--max-processes"
            || args[6] != "--"
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
        if !Path::new(&args[7]).is_absolute() {
            return Err("command must be absolute");
        }
        Ok(Self {
            timeout: Duration::from_millis(milliseconds),
            processes,
            command: args.into_iter().skip(7).collect(),
        })
    }
}

#[cfg(target_os = "linux")]
fn supervise(task: Task) -> Result<i32, Box<dyn std::error::Error>> {
    use rustix::process::{
        DumpableBehavior, Resource, Rlimit, WaitOptions, getpid, getuid, set_dumpable_behavior,
        setrlimit, wait,
    };
    use std::{process::Command, thread, time::Instant};

    // Never run the command on the host, as root, or with privileges which could
    // defeat RLIMIT_NPROC or allow the child to inspect/modify this supervisor.
    if getpid().as_raw_nonzero().get() != 1 || getuid().is_root() {
        return Err("requires PID 1 and a non-root user in an external sandbox".into());
    }
    let status = std::fs::read_to_string("/proc/self/status")?;
    for (key, expected) in [
        ("CapInh:", "0000000000000000"),
        ("CapPrm:", "0000000000000000"),
        ("CapEff:", "0000000000000000"),
        ("CapBnd:", "0000000000000000"),
        ("CapAmb:", "0000000000000000"),
        ("NoNewPrivs:", "1"),
    ] {
        if !status.lines().any(|line| {
            line.strip_prefix(key)
                .is_some_and(|value| value.trim() == expected)
        }) {
            return Err("requires empty capabilities and no-new-privileges".into());
        }
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
        .current_dir("/workspace")
        .spawn()?;
    let primary = child.id();
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
    let _ = (task.timeout, task.processes, task.command);
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
            (6, "--other"),
            (7, "probe"),
        ] {
            let mut invalid = valid;
            invalid[index] = value;
            assert!(parse(&invalid).is_err());
        }
        assert!(parse(&valid[..7]).is_err());
        let mut oversized: Vec<_> = valid.into_iter().map(OsString::from).collect();
        oversized.push(OsString::from("x".repeat(65_536)));
        assert!(Task::parse(oversized).is_err());
        assert!(Task::parse((0..137).map(|_| OsString::from("x"))).is_err());
    }
}
