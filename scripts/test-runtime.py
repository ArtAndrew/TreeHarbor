#!/usr/bin/env python3
"""Real gVisor tests on a disposable Linux CI host; requires sudo and Docker.

Never runs the stress fixture as a host process. Only named containers and loop
files created by this invocation are removed. This is not a product installer or
a node service, and it has no repository/database credentials.
"""
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid


def command(*args, check=True, timeout=90):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} failed ({result.returncode}): {result.stderr[:4000]}")
    return result


def docker(*args, **kwargs):
    return command("docker", *args, **kwargs)


def main():
    if os.uname().sysname != "Linux" or os.getuid() == 0:
        raise RuntimeError("requires an unprivileged user on disposable Linux")
    root = Path(tempfile.mkdtemp(prefix="treeharbor-runtime-", dir=os.environ.get("RUNNER_TEMP")))
    containers, mounts = [], []
    image = None
    try:
        build = root / "image"
        build.mkdir()
        target = Path("target/x86_64-unknown-linux-musl/debug")
        shutil.copy2(target / "treeharbor-runtime-init", build / "init")
        shutil.copy2(target / "examples/runtime_probe", build / "probe")
        # No shell, package manager, remote base image or Git executable.
        (build / "Dockerfile").write_text("FROM scratch\nCOPY init /init\nCOPY probe /probe\n")
        image = docker("build", "--network=none", "--quiet", str(build)).stdout.strip()
        assert image.startswith("sha256:") and len(image) == 71
        for index in range(2):
            backing = root / f"slot-{index}.ext4"
            with backing.open("wb") as handle:
                handle.truncate(128 * 1024 * 1024)
            command("mkfs.ext4", "-q", "-F", "-m", "0", str(backing))
            mount = root / f"slot-{index}"
            mount.mkdir()
            command("sudo", "mount", "-o", "loop,nodev,nosuid", str(backing), str(mount))
            mounts.append(mount)
            command("sudo", "chown", f"{os.getuid()}:{os.getgid()}", str(mount))
        secret = root / "host-secret"
        secret.write_text("test-only-never-mounted")

        def create(mode, *args, slot=0, milliseconds=10000, processes=64, bad_user=False, privileges=False):
            files = mounts[slot] / uuid.uuid4().hex
            files.mkdir(mode=0o700)
            name = "treeharbor-test-" + uuid.uuid4().hex
            options = [
                "create", "--name", name, "--pull=never", "--runtime=runsc",
                "--label", "treeharbor.test=runtime-supervisor-v1",
                "--network=none", "--read-only", "--cap-drop=ALL", "--cgroupns=private",
                "--cap-add=SETUID", "--cap-add=SETGID",
                "--memory=256m", "--memory-swap=256m", "--cpus=1", "--pids-limit=256",
                "--ulimit", "nofile=1024:1024", "--ulimit", "core=0:0",
                "--shm-size=8m", "--tmpfs", "/tmp:rw,noexec,nodev,nosuid,size=16777216,mode=1777",
                "--mount", f"type=bind,source={files},target=/workspace,bind-propagation=rprivate",
                "--workdir=/", "--log-driver=local", "--log-opt=max-size=1m",
                "--log-opt=max-file=1", "--log-opt=compress=false", "--restart=no", "--entrypoint=/init",
                "--env", "TH_TEST_SECRET=not-inherited",
                "--user", f"{os.getuid()}:{os.getgid()}" if bad_user else "0:0",
            ]
            if not privileges:
                options.append("--security-opt=no-new-privileges")
            options.extend([image, "--protocol", "1", "--timeout-ms", str(milliseconds),
                            "--max-processes", str(processes), "--uid", str(os.getuid()),
                            "--gid", str(os.getgid()), "--", "/probe", mode, *args])
            container = docker(*options).stdout.strip()
            assert len(container) == 64
            containers.append(container)
            config = json.loads(docker("inspect", container).stdout)[0]
            host = config["HostConfig"]
            assert config["Image"] == image and host["Runtime"] == "runsc"
            assert host["NetworkMode"] == "none" and host["ReadonlyRootfs"]
            assert not host["Privileged"] and host["CapDrop"] == ["ALL"]
            assert {cap.removeprefix("CAP_") for cap in host["CapAdd"]} == {"SETUID", "SETGID"}, host["CapAdd"]
            assert host["Memory"] == 256 * 1024 * 1024 and host["MemorySwap"] == host["Memory"]
            assert host["NanoCpus"] == 1_000_000_000 and host["PidsLimit"] == 256
            return container, files

        def start(container):
            docker("start", container)

        def finish(container, expected):
            code = int(docker("wait", container, timeout=45).stdout.strip())
            state = json.loads(docker("inspect", container).stdout)[0]["State"]
            if code != expected:
                logs = docker("logs", "--tail=30", container, check=False)
                raise AssertionError(f"exit {code}, expected {expected}; {state}; {logs.stdout} {logs.stderr}")
            assert not state["Running"] and state["Pid"] == 0
            return state

        # Two simultaneous tasks edit the same relative filename independently.
        a, a_files = create("isolation", "agent-a", str(secret), slot=0)
        b, b_files = create("isolation", "agent-b", str(secret), slot=1)
        start(a)
        start(b)
        finish(a, 0)
        finish(b, 0)
        assert (a_files / "same.txt").read_text() == "agent-a"
        assert (b_files / "same.txt").read_text() == "agent-b"
        assert secret.read_text() == "test-only-never-mounted"
        print("PASS concurrent files, namespace/filesystem/network/credential isolation", flush=True)

        for bad_user, privileges in [(True, False), (False, True)]:
            container, _ = create("isolation", "never", str(secret), bad_user=bad_user, privileges=privileges)
            start(container)
            finish(container, 125)
        container, _ = create("exit", "37")
        start(container)
        finish(container, 37)
        print("PASS privilege guards and primary exit propagation", flush=True)

        for mode, milliseconds, code in [("descendant", 10000, 0), ("attack", 2000, 124)]:
            container, files = create(mode, milliseconds=milliseconds)
            # The initiating process exits before the task does. No worker wait
            # loop, live connection or host timeout is needed to stop the task.
            launcher = subprocess.Popen(["docker", "start", container], stdout=subprocess.DEVNULL)
            assert launcher.wait(timeout=30) == 0
            finish(container, code)
            count = (files / "heartbeat").stat().st_size
            assert count > 0
            time.sleep(0.2)
            assert (files / "heartbeat").stat().st_size == count
            assert docker("exec", container, "/probe", "idle", check=False).returncode != 0
        print("PASS deadline without live launcher, init signal/proc attacks, descendant termination", flush=True)

        container, files = create("attack", milliseconds=20000)
        start(container)
        until = time.monotonic() + 10
        while not (files / "heartbeat").exists() and time.monotonic() < until:
            time.sleep(0.01)
        assert (files / "heartbeat").exists()
        docker("kill", "--signal=KILL", container)
        finish(container, 137)
        count = (files / "heartbeat").stat().st_size
        time.sleep(0.2)
        assert (files / "heartbeat").stat().st_size == count
        print("PASS external cancellation kills namespace writers", flush=True)

        for budget in [4, 16, 64]:
            container, files = create("nproc", str(budget), processes=budget)
            start(container)
            finish(container, 0)
            total = int((files / "process-count").read_text())
            assert 2 < total <= budget
            print(f"PASS guest process budget {budget}: {total} tasks including primary; repeat after reap", flush=True)

        container, _ = create("memory", milliseconds=20000)
        start(container)
        state = finish(container, 137)
        assert state["OOMKilled"], state
        print("PASS real memory exhaustion stops sandbox", flush=True)

        container, files = create("disk", milliseconds=20000)
        start(container)
        finish(container, 0)
        assert 0 < (files / "fill").stat().st_size < 128 * 1024 * 1024
        assert (b_files / "same.txt").read_text() == "agent-b"
        print("PASS persistent dedicated filesystem capacity and neighboring volume preservation", flush=True)
    finally:
        # Confirm runtime deletion before unmounting or deleting its files.
        for container in containers:
            docker("rm", "--force", container)
        for mount in reversed(mounts):
            command("sudo", "umount", str(mount))
        if image:
            docker("image", "rm", image)
        shutil.rmtree(root)


if __name__ == "__main__":
    main()
