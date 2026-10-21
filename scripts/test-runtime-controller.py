#!/usr/bin/env python3
"""Real Rust controller + Docker/runsc, including lost responses after SIGKILL.

The Unix proxy below only delays/drops real Engine traffic at explicit boundaries.
It never fabricates a successful Engine response. Run only on disposable CI Linux.
"""
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid


def command(*args, check=True, timeout=60):
    result = subprocess.run(args, capture_output=True, text=True, timeout=timeout)
    if check and result.returncode:
        raise RuntimeError(f"{args[0]} failed: {result.stderr[:3000]}")
    return result


def docker(*args, **kwargs):
    return command("docker", *args, **kwargs)


def wait_until(predicate, timeout=20):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        if predicate():
            return
        time.sleep(0.02)
    raise AssertionError("fault boundary or runtime state did not arrive")


def proxy(root, gid):
    """Root-owned test socket preserves the production Engine peer-UID check."""
    if os.getuid() != 0:
        raise RuntimeError("proxy requires the disposable runner's root identity")
    root = Path(root)
    listener = socket.socket(socket.AF_UNIX)
    listener.bind(str(root / "engine.sock"))
    os.chown(root / "engine.sock", 0, gid)
    os.chmod(root / "engine.sock", 0o660)
    listener.listen(16)
    (root / "proxy.pid").write_text(str(os.getpid()))

    def pause():
        try:
            with (root / "claimed").open("x") as handle:
                handle.write("claimed")
        except FileExistsError:
            return "forward", False
        (root / "reached").write_text("reached")
        wait_until(lambda: (root / "release").exists(), 25)
        return (root / "release").read_text(), True

    def handle(client):
        paused = False
        try:
            client.settimeout(35)
            request = b""
            while b"\r\n\r\n" not in request:
                chunk = client.recv(8192)
                if not chunk:
                    return
                request += chunk
                if len(request) > 32768:
                    raise AssertionError("test proxy header limit")
            head, body = request.split(b"\r\n\r\n", 1)
            fields = head.split(b"\r\n")
            lengths = [int(line.split(b":", 1)[1]) for line in fields[1:] if line.lower().startswith(b"content-length:")]
            length = lengths[0] if lengths else 0
            if length > 256 * 1024:
                raise AssertionError("test proxy body limit")
            while len(body) < length:
                chunk = client.recv(min(8192, length - len(body)))
                if not chunk:
                    return
                body += chunk
            first = fields[0]
            mode = (root / "fault").read_text() if (root / "fault").exists() else "none"
            before = mode == "before-start" and first.startswith(b"POST ") and b"/start " in first
            after = mode == "after-create" and first.startswith(b"POST /v1.47/containers/create?")
            after = after or (mode == "after-remove" and first.startswith(b"DELETE "))
            if before:
                decision, paused = pause()
                if decision == "drop":
                    return
            with socket.socket(socket.AF_UNIX) as backend:
                backend.settimeout(35)
                backend.connect("/var/run/docker.sock")
                backend.sendall(head + b"\r\n\r\n" + body)
                response = b""
                while True:
                    chunk = backend.recv(65536)
                    if not chunk:
                        break
                    response += chunk
                    if len(response) > 2 * 1024 * 1024:
                        raise AssertionError("test proxy response limit")
            if after:
                _, paused = pause()
            try:
                client.sendall(response)
            except (BrokenPipeError, ConnectionResetError):
                pass  # Intentional client SIGKILL; the real Engine effect remains.
        except Exception as error:
            print(f"proxy error: {type(error).__name__}: {error}", flush=True)
        finally:
            client.close()
            if paused:
                (root / "done").write_text("done")

    while True:
        client, _ = listener.accept()
        threading.Thread(target=handle, args=(client,), daemon=True).start()


def main():
    if os.uname().sysname != "Linux" or os.getuid() == 0:
        raise RuntimeError("requires an unprivileged disposable Linux runner")
    root = Path(tempfile.mkdtemp(prefix="treeharbor-controller-", dir=os.environ.get("RUNNER_TEMP")))
    mounts, names = [], []
    image = process = None
    driver = Path("target/x86_64-unknown-linux-musl/debug/examples/runtime_controller").absolute()
    try:
        build = root / "image"
        build.mkdir()
        target = Path("target/x86_64-unknown-linux-musl/debug")
        shutil.copy2(target / "treeharbor-runtime-init", build / "init")
        shutil.copy2(target / "examples/runtime_probe", build / "probe")
        (build / "Dockerfile").write_text("FROM scratch\nCOPY init /init\nCOPY probe /probe\n")
        image = docker("build", "--network=none", "--quiet", str(build)).stdout.strip()
        for index in range(2):
            backing = root / f"slot-{index}.ext4"
            with backing.open("wb") as handle:
                handle.truncate(128 * 1024 * 1024)
            command("mkfs.ext4", "-q", "-F", "-m", "0", str(backing))
            mount = root / f"slot-{index}"
            mount.mkdir()
            command("sudo", "mount", "-o", "loop,nodev,nosuid", str(backing), str(mount))
            mounts.append(mount)
            command("sudo", "mount", "--make-private", str(mount))
            command("sudo", "chown", f"{os.getuid()}:{os.getgid()}", str(mount))
            os.chmod(mount, 0o700)
        journals = root / "journals"
        journals.mkdir(mode=0o700)
        transport = root / "proxy"
        transport.mkdir(mode=0o700)
        log = (root / "proxy.log").open("w")
        process = subprocess.Popen(["sudo", "-n", sys.executable, str(Path(__file__).absolute()), "--proxy", str(transport), str(os.getgid())], stdout=log, stderr=subprocess.STDOUT)
        wait_until(lambda: (transport / "proxy.pid").exists())
        secret = root / "host-secret"
        secret.write_text("not-mounted")

        def config(mode, *args, slot=0, timeout=10000):
            attempt = str(uuid.uuid4())
            files = mounts[slot] / attempt
            files.mkdir(mode=0o700)
            value = {"socket": str(transport / "engine.sock"), "journal": str(journals / attempt),
                     "mount": str(mounts[slot]), "files": str(files), "attempt": attempt,
                     "profile": {"version": 1, "image": image, "uid": os.getuid(), "gid": os.getgid(),
                                 "memory_bytes": 268435456, "nano_cpus": 1000000000, "max_processes": 64,
                                 "timeout_ms": timeout, "workspace_bytes": 134217728},
                     "argv": ["/probe", mode, *args]}
            path = root / f"{attempt}.json"
            path.write_text(json.dumps(value))
            os.chmod(path, 0o600)
            names.append("treeharbor-" + attempt)
            return path, value, files

        def call(c, action, check=True):
            result = command(str(driver), str(c[0]), action, check=False)
            if check and result.returncode:
                actual = docker("inspect", "treeharbor-" + c[1]["attempt"], check=False)
                print("Failed test fixture inspect: " + actual.stdout[:12000], flush=True)
                raise AssertionError(result.stderr)
            return json.loads(result.stdout) if result.returncode == 0 else result

        def finish(c):
            last = None
            def exited():
                nonlocal last
                last = call(c, "status")
                return last["state"] == "exited"
            wait_until(exited)
            return last

        a = config("isolation", "agent-a", str(secret), slot=0)
        b = config("isolation", "agent-b", str(secret), slot=1)
        aa, bb = call(a, "create"), call(b, "create")
        assert aa["id"] != bb["id"] and aa["state"] == bb["state"] == "created"
        children = [subprocess.Popen([str(driver), str(c[0]), "start"], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) for c in (a, b)]
        for child, case in zip(children, (a, b)):
            out, err = child.communicate(timeout=40)
            if child.returncode:
                actual = docker("inspect", "treeharbor-" + case[1]["attempt"], check=False)
                print("Failed start fixture inspect: " + actual.stdout[:12000], flush=True)
            assert child.returncode == 0, (out, err)
        assert finish(a)["exit_code"] == finish(b)["exit_code"] == 0
        assert (a[2] / "same.txt").read_text() == "agent-a"
        assert (b[2] / "same.txt").read_text() == "agent-b"
        competing = config("once", slot=0)
        assert call(competing, "create", check=False).returncode != 0
        assert docker("inspect", names[-1], check=False).returncode != 0
        call(a, "remove")
        call(b, "remove")
        print("PASS Rust controller: independent tasks, verified profiles and retained volume ownership", flush=True)

        def fault(mode, c, action, release):
            for path in ["claimed", "reached", "release", "done"]:
                if (transport / path).exists():
                    command("sudo", "rm", str(transport / path))
            (transport / "fault").write_text(mode)
            child = subprocess.Popen([str(driver), str(c[0]), action], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                wait_until(lambda: (transport / "reached").exists() or child.poll() is not None)
                assert child.poll() is None, child.communicate()
                child.kill()
                child.communicate(timeout=10)
                (transport / "release").write_text(release)
                wait_until(lambda: (transport / "done").exists())
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
                (transport / "fault").write_text("none")

        c = config("once")
        fault("after-create", c, "create", "forward")
        assert not (Path(c[1]["journal"]) / "container.json").exists()
        expected = json.loads(docker("inspect", "treeharbor-" + c[1]["attempt"]).stdout)[0]["Id"]
        assert call(c, "create")["id"] == expected
        fault("before-start", c, "start", "forward")
        wait_until(lambda: (c[2] / "executions").exists())
        stopped = finish(c)
        assert stopped["exit_code"] == 0
        assert call(c, "start")["started_at"] == stopped["started_at"]
        assert call(c, "start")["started_at"] == stopped["started_at"]
        assert (c[2] / "executions").read_bytes() == b"one execution\n"
        fault("after-remove", c, "remove", "forward")
        assert not (Path(c[1]["journal"]) / "removed.json").exists()
        assert (mounts[0] / "treeharbor-execution.json").exists()
        call(c, "remove")
        assert call(c, "status")["state"] == "removed"
        assert call(c, "start", check=False).returncode != 0
        print("PASS SIGKILL after real create/remove and before delayed real start; recovery never reexecutes", flush=True)

        d = config("once")
        call(d, "create")
        fault("before-start", d, "start", "drop")
        assert call(d, "start", check=False).returncode != 0
        assert call(d, "status")["state"] == "created"
        assert not (d[2] / "executions").exists()
        call(d, "remove")
        # Old removal replay cannot release a new attempt's persistent assignment.
        e = config("once")
        call(e, "create")
        call(d, "remove")
        owner = json.loads((mounts[0] / "treeharbor-execution.json").read_text())
        assert owner["attempt"] == e[1]["attempt"]
        call(e, "start")
        assert finish(e)["exit_code"] == 0
        docker("update", "--cpus=0.5", "treeharbor-" + e[1]["attempt"])
        assert call(e, "status", check=False).returncode != 0
        print("PASS uncertain unsent start is retained, stale cleanup cannot release successor, drift is refused", flush=True)
        # Restore only this deliberately altered test fixture, then use exact
        # controller removal before testing resource exhaustion on its volume.
        docker("update", "--cpus=1", "treeharbor-" + e[1]["attempt"])
        call(e, "remove")
        memory = config("memory")
        call(memory, "start")
        outcome = finish(memory)
        assert outcome["exit_code"] == 137 and outcome["oom_killed"]
        call(memory, "remove")
        timed = config("attack", timeout=1500)
        call(timed, "start")
        assert finish(timed)["exit_code"] == 124
        count = (timed[2] / "heartbeat").stat().st_size
        time.sleep(0.2)
        assert count > 0 and (timed[2] / "heartbeat").stat().st_size == count
        call(timed, "remove")
        disk = config("disk")
        call(disk, "start")
        assert finish(disk)["exit_code"] == 0
        assert (b[2] / "same.txt").read_text() == "agent-b"
        call(disk, "remove")
        print("PASS Rust controller: real OOM, autonomous deadline/descendant stop and bounded persistent disk", flush=True)
    finally:
        for name in names:
            if docker("inspect", name, check=False).returncode == 0:
                docker("rm", "--force", name)
        if process:
            pid_file = root / "proxy/proxy.pid"
            if pid_file.exists():
                command("sudo", "kill", "-TERM", pid_file.read_text().strip(), check=False)
            process.wait(timeout=10)
        for mount in reversed(mounts):
            command("sudo", "umount", str(mount))
        if image:
            docker("image", "rm", image)
        # Root-owned fault-proxy artifacts are confined to this invocation root.
        command("sudo", "rm", "-rf", "--", str(root))


if __name__ == "__main__":
    if len(sys.argv) == 4 and sys.argv[1] == "--proxy":
        proxy(sys.argv[2], int(sys.argv[3]))
    else:
        main()
