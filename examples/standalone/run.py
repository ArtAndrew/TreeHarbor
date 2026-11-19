#!/usr/bin/env python3
"""Run the protected two-agent Standalone example in a disposable Linux VM."""

import argparse
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import tempfile
import time
import uuid


PACKAGE = Path(__file__).resolve().parents[2]
POSTGRES = "postgres:18.6"
ROLE = "treeharbor_test"


def run(argv, env, *, capture=True, check=True, timeout=30):
    return subprocess.run(argv, env=env, text=True, capture_output=capture,
                          check=check, timeout=timeout, cwd=PACKAGE)


def ready(container, env):
    end = time.monotonic() + 45
    while time.monotonic() < end:
        # PID 1 becomes PostgreSQL after the entrypoint has finished creating
        # the database and stopped its temporary initialization server.
        result = run(["docker", "exec", container, "sh", "-ec",
                      'test "$(cat /proc/1/comm)" = postgres && psql -U "$1" -d "$1" -Atqc "SELECT 1"',
                      "ready", ROLE], env, check=False, timeout=5)
        if result.returncode == 0 and result.stdout.strip() == "1":
            return
        time.sleep(0.25)
    raise RuntimeError("the disposable PostgreSQL database did not become ready")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--treeharbor", type=Path, required=True, help="verified installed CLI")
    parser.add_argument("--busybox", type=Path, default=Path("/bin/busybox"), help="native static BusyBox")
    parser.add_argument("--export-dir", type=Path,
                        help="keep the verified Git bundle and manifest in a new directory under a private parent")
    args = parser.parse_args()
    if sys.platform != "linux" or os.getuid() == 0:
        parser.error("use a non-root account in a disposable Linux VM")
    export_args = []
    if args.export_dir is not None:
        output = args.export_dir.absolute()
        parent = output.parent.lstat()
        if (not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid()
                or parent.st_mode & 0o077 or os.path.lexists(output) or output.name in ("", ".", "..")):
            parser.error("--export-dir needs a new path under an existing directory you own with mode 0700")
        export_args = ["--demo-export-dir", str(output)]
    binary = args.treeharbor.resolve(strict=True)
    init = binary.with_name("treeharbor-runtime-init")
    busybox = args.busybox.resolve(strict=True)
    if not all(p.is_file() and os.access(p, os.X_OK) for p in (binary, init, busybox)):
        parser.error("install the verified CLI and its runtime init, and provide a static BusyBox")

    env = {"PATH": os.environ.get("PATH", os.defpath), "DOCKER_HOST": "unix:///var/run/docker.sock"}
    runtimes = run(["docker", "info", "--format", "{{json .Runtimes}}"], env)
    if "runsc" not in json.loads(runtimes.stdout):
        parser.error("configure gVisor as the runsc runtime in local Docker")
    if run(["docker", "image", "inspect", POSTGRES], env, check=False).returncode != 0:
        parser.error(f"stage the {POSTGRES} image in local Docker first")

    root = Path(tempfile.mkdtemp(prefix="th-standalone-", dir="/tmp"))
    socket = root / "pg"
    socket.mkdir()
    socket.chmod(0o777)  # The container UID can write its socket inside this private parent.
    container = "th-standalone-" + uuid.uuid4().hex
    try:
        run(["docker", "run", "--detach", "--rm", "--pull=never", "--network=none",
             "--name", container, "-e", f"POSTGRES_USER={ROLE}", "-e", f"POSTGRES_DB={ROLE}",
             "-e", "POSTGRES_HOST_AUTH_METHOD=trust",
             "--mount", f"type=bind,source={socket},target=/var/run/postgresql",
             POSTGRES, "-c", "listen_addresses="], env)
        ready(container, env)
        drill_env = dict(env, TH_TEST_SERVICE_BINARY=str(binary), TH_TEST_RUNTIME_INIT=str(init),
                         TH_RUNTIME_PG_CONTAINER=container, RUNNER_TEMP=str(root),
                         TH_RUNTIME_DATABASE_URL=f"host={socket} user={ROLE} dbname={ROLE}")
        run([sys.executable, "-B", "scripts/test-native-checks.py", "--overlap-only",
             "--demo-busybox", str(busybox), *export_args], drill_env, capture=False, timeout=None)
    finally:
        try:
            run(["docker", "stop", "--time", "5", container], env, check=False)
        finally:
            run(["docker", "rm", "--force", "--volumes", container], env, check=False)
        try:
            socket.rmdir()
            root.rmdir()
        except OSError:
            print(f"Inspect retained demo state in {root}", file=sys.stderr)
    if args.export_dir is not None:
        print(f"Verified Git bundle: {output / 'history.bundle'}")


if __name__ == "__main__":
    try:
        main()
    except (OSError, subprocess.SubprocessError, RuntimeError, ValueError) as error:
        print(f"Standalone example stopped: {error}", file=sys.stderr)
        if isinstance(error, subprocess.CalledProcessError) and error.stderr:
            print(error.stderr.strip()[-2000:], file=sys.stderr)
        sys.exit(1)
