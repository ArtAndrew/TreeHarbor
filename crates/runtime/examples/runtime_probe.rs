//! Adversarial integration fixture. Invoke only in the disposable CI sandbox.
#[cfg(target_os = "linux")]
fn main() {
    use rustix::process::{Pid, Signal, getuid, kill_process};
    use std::{
        fs::{self, File, OpenOptions},
        io::Write,
        net::TcpStream,
        os::unix::process::CommandExt,
        process::Command,
        thread,
        time::Duration,
    };

    assert_ne!(
        std::process::id(),
        1,
        "the trusted supervisor must be PID 1"
    );
    assert!(!getuid().is_root());
    assert_ne!(getuid().as_raw(), 65534);
    assert_eq!(
        rustix::thread::set_thread_uid(rustix::process::Uid::ROOT),
        Err(rustix::io::Errno::PERM)
    );
    assert_eq!(
        rustix::thread::set_thread_uid(rustix::process::Uid::from_raw(65534)),
        Err(rustix::io::Errno::PERM)
    );
    let status = fs::read_to_string("/proc/self/status").unwrap();
    for key in ["CapInh:", "CapPrm:", "CapEff:", "CapAmb:"] {
        assert!(status.lines().any(|line| {
            line.strip_prefix(key)
                .is_some_and(|value| value.trim() == "0000000000000000")
        }));
    }
    // Refuse the most obvious accidental host invocation, before any stress.
    assert_eq!(
        fs::read_link("/proc/1/exe").unwrap_err().kind(),
        std::io::ErrorKind::PermissionDenied
    );
    let args: Vec<_> = std::env::args().collect();
    match args[1].as_str() {
        "check" => {
            assert!(std::env::var_os("TH_TEST_SECRET").is_none());
            assert!(!std::path::Path::new("/var/run/docker.sock").exists());
            assert!(
                TcpStream::connect_timeout(
                    &"1.1.1.1:443".parse().unwrap(),
                    Duration::from_millis(100)
                )
                .is_err()
            );
            assert!(std::env::vars().all(|(_, v)| !v.contains("thv1_")));
            let actual = fs::read_to_string("same.txt").unwrap();
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open("check-executions")
                .unwrap();
            file.write_all(b"one check\n").unwrap();
            file.sync_all().unwrap();
            if let Some(delay) = args.get(3) {
                thread::sleep(Duration::from_millis(delay.parse().unwrap()));
            }
            if actual != args[2] {
                std::process::exit(23);
            }
        }
        "once" => {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open("executions")
                .unwrap();
            file.write_all(b"one execution\n").unwrap();
            file.sync_all().unwrap();
            if let Some(delay) = args.get(2) {
                thread::sleep(Duration::from_millis(delay.parse().unwrap()));
            }
        }
        "isolation" => {
            assert!(std::env::var_os("TH_TEST_SECRET").is_none());
            assert!(!std::path::Path::new("/var/run/docker.sock").exists());
            assert!(fs::read(&args[3]).is_err());
            assert!(File::create("/escape").is_err());
            assert!(File::create("/probe").is_err());
            assert!(
                OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open("/proc/1/mem")
                    .is_err()
            );
            assert!(
                TcpStream::connect_timeout(
                    &"1.1.1.1:443".parse().unwrap(),
                    Duration::from_millis(200)
                )
                .is_err()
            );
            fs::write("same.txt", args[2].as_bytes()).unwrap();
            assert_eq!(fs::read("same.txt").unwrap(), args[2].as_bytes());
            if let Some(delay) = args.get(4) {
                thread::sleep(Duration::from_millis(delay.parse().unwrap()));
            }
        }
        "permissions" => {
            use std::os::unix::fs::PermissionsExt;
            fs::write("before-chmod", b"saved").unwrap();
            fs::set_permissions("/workspace", fs::Permissions::from_mode(0o000)).unwrap();
            thread::sleep(Duration::from_millis(1000));
        }
        "exit" => std::process::exit(args[2].parse().unwrap()),
        "idle" => loop {
            thread::sleep(Duration::from_secs(1));
        },
        "heartbeat" => loop {
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open("heartbeat")
                .unwrap();
            file.write_all(b"tick\n").unwrap();
            thread::sleep(Duration::from_millis(10));
        },
        "descendant" | "attack" => {
            // A distinct process group must not escape namespace teardown.
            let child = Command::new("/probe")
                .arg("heartbeat")
                .process_group(0)
                .spawn()
                .unwrap();
            drop(child);
            while !fs::metadata("heartbeat").is_ok_and(|metadata| metadata.len() > 0) {
                thread::sleep(Duration::from_millis(10));
            }
            if args[1] == "attack" {
                for signal in [Signal::STOP, Signal::KILL, Signal::TERM] {
                    assert_eq!(
                        kill_process(Pid::from_raw(1).unwrap(), signal),
                        Err(rustix::io::Errno::PERM)
                    );
                }
                assert!(OpenOptions::new().write(true).open("/proc/1/mem").is_err());
                loop {
                    thread::sleep(Duration::from_secs(1));
                }
            }
        }
        "nproc" => {
            let requested: usize = args[2].parse().unwrap();
            // The task must not be able to raise either inherited limit.
            let limit = rustix::process::getrlimit(rustix::process::Resource::Nproc);
            let raised = limit.maximum.unwrap() + 1;
            assert_eq!(
                rustix::process::setrlimit(
                    rustix::process::Resource::Nproc,
                    rustix::process::Rlimit {
                        current: Some(raised),
                        maximum: Some(raised),
                    },
                ),
                Err(rustix::io::Errno::PERM)
            );
            // Reap and repeat: accounting must recover after children exit.
            for _ in 0..2 {
                let mut children = Vec::new();
                let mut limited = false;
                for _ in 0..128 {
                    match Command::new("/probe").arg("idle").spawn() {
                        Ok(child) => children.push(child),
                        Err(error) => {
                            assert_eq!(error.raw_os_error(), Some(11)); // EAGAIN
                            limited = true;
                            break;
                        }
                    }
                }
                let count = children.len();
                for mut child in children {
                    assert!(child.try_wait().unwrap().is_none());
                    child.kill().unwrap();
                    child.wait().unwrap();
                }
                assert!(
                    limited && count > 1 && count < requested,
                    "guest children {count}, requested total {requested}"
                );
                fs::write("process-count", (count + 1).to_string()).unwrap();
            }
        }
        "disk" => {
            let mut file = File::create("fill").unwrap();
            let buffer = vec![1_u8; 1024 * 1024];
            for _ in 0..256 {
                match file.write_all(&buffer) {
                    Ok(()) => {}
                    Err(error) => {
                        assert_eq!(error.raw_os_error(), Some(28), "expected ENOSPC");
                        return;
                    }
                }
            }
            panic!("dedicated workspace volume was not bounded");
        }
        "memory" => {
            let mut buffers = Vec::new();
            loop {
                let mut buffer = vec![0_u8; 8 * 1024 * 1024];
                for index in (0..buffer.len()).step_by(4096) {
                    buffer[index] = 1;
                }
                buffers.push(buffer);
                std::hint::black_box(&buffers);
            }
        }
        _ => panic!("unknown test mode"),
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("Linux sandbox test fixture only");
}
