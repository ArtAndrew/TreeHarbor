#!/usr/bin/env python3
"""Actual service/CLI/PostgreSQL -> bounded node preparation -> gVisor lifecycle.

Disposable Linux CI only. Faults kill real workers and delay real Engine RPCs.
No fixture fabricates runtime success or bypasses source import/preparation.
Lease expiry is injected in PostgreSQL only after the corresponding worker dies.
"""
import json
from hashlib import sha256
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import uuid


def command(*args, check=True, timeout=60, env=None):
    result = subprocess.run([str(arg) for arg in args], capture_output=True,
                            text=True, timeout=timeout, env=env)
    if check and result.returncode:
        raise AssertionError(f"{args[0]} failed: {result.stderr[:3000]}")
    return result


def docker(*args, **kwargs):
    return command("docker", *args, **kwargs)


def wait_until(predicate, timeout=20):
    until = time.monotonic() + timeout
    while time.monotonic() < until:
        if predicate():
            return
        time.sleep(0.03)
    raise AssertionError("managed run did not reach the required boundary")


def private_json(path, value):
    path.write_text(json.dumps(value))
    path.chmod(0o600)


class Harness:
    def __init__(self):
        if os.uname().sysname != "Linux" or os.getuid() == 0:
            raise RuntimeError("requires an unprivileged disposable Linux runner")
        self.pg = os.environ["TH_RUNTIME_PG_CONTAINER"]
        self.url = os.environ["TH_RUNTIME_DATABASE_URL"]
        self.binary = Path("target/debug/treeharbor").absolute()
        self.root = Path(tempfile.mkdtemp(prefix="th-managed-", dir=os.environ.get("RUNNER_TEMP")))
        self.schema = "th_test_" + uuid.uuid4().hex
        self.repository = str(uuid.uuid4())
        self.scope = ["--tenant", "managed-ci", "--repository", self.repository]
        self.config = self.root / "config.json"
        self.socket = self.root / "service.sock"
        self.mounts, self.children, self.cases, self.nodes = [], [], [], []
        self.image = self.proxy = self.server = None
        self.env = dict(os.environ, PATH="/treeharbor-test-no-executables", TH_TEST_SECRET="must-not-enter-task")

    def cli(self, *args, check=True):
        return command(self.binary, *args, env=self.env, check=check)

    def value(self, *args):
        output = self.cli(*args)
        assert "thv1_" not in output.stdout and "thv1_" not in output.stderr
        return json.loads(output.stdout)

    def admin(self, *args):
        return self.value("admin", "--config", self.config, *args)

    def client(self, *args, credential=None, check=True):
        output = self.cli("client", "--socket", self.socket, "--credential-file",
                          credential or self.owner, *args, check=check)
        if not check:
            return output
        return json.loads(output.stdout)["result"]

    def sql(self, query):
        return docker("exec", self.pg, "psql", "-U", "treeharbor_test", "-d", "treeharbor_test",
                      "-At", "-v", "ON_ERROR_STOP=1", "-c",
                      f"SET search_path TO {self.schema}; {query}").stdout.strip().splitlines()[-1]

    def principal(self, name, kind, permissions):
        self.admin("principal-create", "--tenant", "managed-ci", "--principal", name, "--kind", kind)
        grants = [item for permission in permissions for item in ("--permission", permission)]
        self.admin("access-set", *self.scope, "--principal", name, *grants)
        credential = self.root / f"{name}.json"
        self.admin("token-issue", *self.scope, "--principal", name, *grants, "--output", credential)
        return credential

    def setup(self):
        # Installing runsc restarts the daemon; explicitly restart this disposable
        # CI service if Docker did not restore it automatically.
        docker("start", self.pg)
        wait_until(lambda: docker("exec", self.pg, "pg_isready", "-U", "treeharbor_test", check=False).returncode == 0)
        docker("exec", self.pg, "psql", "-U", "treeharbor_test", "-d", "treeharbor_test",
               "-v", "ON_ERROR_STOP=1", "-c", f"CREATE SCHEMA {self.schema}")
        store = self.root / "objects"
        store.mkdir(mode=0o700)
        private_json(self.config, {"database_url": self.url, "schema": self.schema,
                                  "socket": str(self.socket), "storage": {"root": str(store), "backend": "managed-ci"}})
        self.admin("migrate")
        self.admin("repository-create", *self.scope, "--authority", "standalone")
        self.owner = self.principal("owner", "human", ["read", "workspace.write", "repository.write"])
        self.agents = [self.principal(name, "agent", ["read", "workspace.write", "change.write"])
                       for name in ("agent-a", "agent-b")]
        for index in range(2):
            name = f"node-{index}"
            credential = self.principal(name, "node", ["runtime.control"])
            root = self.root / name
            identity = self.value("node", "initialize", "--root", root, "--tenant", "managed-ci", "--principal", name)
            self.admin("node-register", "--tenant", "managed-ci", "--principal", name, "--storage", identity["storage"])
            self.nodes.append({"root": root, "credential": credential, "storage": identity["storage"]})
        log = (self.root / "server.log").open("w")
        self.server = subprocess.Popen([str(self.binary), "serve", "--config", str(self.config)],
                                       env=self.env, stdout=log, stderr=subprocess.STDOUT)
        self.children.append(self.server)
        wait_until(lambda: self.socket.exists() or self.server.poll() is not None)
        assert self.server.poll() is None, (self.root / "server.log").read_text()
        source = self.root / "source"
        source.mkdir(mode=0o700)
        (source / "same.txt").write_text("base\n")
        pack = self.root / "pack"
        self.value("source-pack", "--directory", source, "--output", pack, "--path", "same.txt",
                   "--author-name", "Runtime fixture", "--author-email", "fixture@example.invalid")
        imported = self.client("source-import", "--pack", pack, "--state", self.root / "import.json",
                               "--authority", "standalone", "--epoch", "1")
        self.revision = imported["revision"]
        self.source_profile = self.client("profile-put", "--key", "full", "--authority", "standalone",
                                          "--epoch", "1", "--full")["profile_digest"]
        build = self.root / "image"
        build.mkdir()
        target = Path("target/x86_64-unknown-linux-musl/debug")
        shutil.copy2(target / "treeharbor-runtime-init", build / "init")
        shutil.copy2(target / "examples/runtime_probe", build / "probe")
        (build / "Dockerfile").write_text("FROM scratch\nCOPY init /init\nCOPY probe /probe\n")
        self.image = docker("build", "--network=none", "--quiet", build).stdout.strip()
        self.transport = self.root / "proxy"
        self.transport.mkdir(mode=0o700)
        log = (self.root / "proxy.log").open("w")
        self.proxy = subprocess.Popen(["sudo", "-n", sys.executable,
                                      str(Path("scripts/test-runtime-controller.py").absolute()), "--proxy",
                                      str(self.transport), str(os.getgid())], stdout=log, stderr=subprocess.STDOUT)
        wait_until(lambda: (self.transport / "proxy.pid").exists())
        self.secret = self.root / "host-secret"
        self.secret.write_text("not-mounted")

    def node_args(self, node, action):
        return ["node", action, "--config", str(self.config), "--credential-file", str(node["credential"]),
                "--root", str(node["root"]), "--storage", node["storage"]]

    def case(self, mode, *args, node=0, timeout=10000):
        number = len(self.cases)
        backing = self.root / f"volume-{number}.ext4"
        with backing.open("wb") as file:
            file.truncate(128 * 1024 * 1024)
        command("mkfs.ext4", "-q", "-F", "-m", "0", backing)
        mount = self.root / f"volume-{number}"
        mount.mkdir()
        command("sudo", "mount", "-o", "loop,nodev,nosuid", backing, mount)
        self.mounts.append(mount)
        command("sudo", "mount", "--make-private", mount)
        command("sudo", "chown", f"{os.getuid()}:{os.getgid()}", mount)
        mount.chmod(0o700)
        placement = mount / "placement"
        placement.mkdir(mode=0o700)
        slot = str(uuid.uuid4())
        assigned = self.nodes[node]
        self.value(*self.node_args(assigned, "volume-register"), "--slot", slot, "--mount", mount,
                   "--placement", placement, "--limit-bytes", "134217728")
        profile = self.root / f"profile-{number}.json"
        private_json(profile, {"version": 1, "image": self.image, "uid": os.getuid(), "gid": os.getgid(),
                               "memory_bytes": 268435456, "nano_cpus": 1000000000, "max_processes": 64,
                               "timeout_ms": timeout, "workspace_bytes": 134217728})
        digest = self.admin("runtime-profile-register", *self.scope, "--file", profile)["profile_digest"]
        workspace, run = str(uuid.uuid4()), str(uuid.uuid4())
        credential = self.agents[node]
        self.client("workspace-create", "--key", f"workspace-{number}", "--authority", "standalone", "--epoch", "1",
                    "--workspace", workspace, "--base", self.revision, "--profile-digest", self.source_profile,
                    credential=credential)
        prepared = self.value(*self.node_args(assigned, "prepare-once"), "--slot", slot)
        assert prepared["workspace"] == workspace and prepared["state"] == "files_ready" and prepared["runtime"] is None
        files = Path(prepared["path"])
        assert (files / "same.txt").read_text() == "base\n"
        request = ["run-request", "--key", f"run-{number}", "--authority", "standalone", "--epoch", "1",
                   "--run", run, "--workspace", workspace, "--generation", "1", "--profile-digest", digest,
                   "--agent", f"agent-{node}", "--task", f"task-{number}", "--", "/probe", mode, *map(str, args)]
        first = self.client(*request, credential=credential)
        assert first == self.client(*request, credential=credential)
        assert first["state"] == "requested"
        attempt = self.sql(f"SELECT attempt FROM th_managed_runs WHERE run='{run}'")
        case = {"node": assigned, "credential": credential, "workspace": workspace, "run": run,
                "attempt": attempt, "files": files, "mount": mount, "request": request, "first": first,
                "profile": digest, "node_index": node}
        self.cases.append(case)
        return case

    def status(self, case):
        return self.client("run-status", "--run", case["run"], credential=case["credential"])

    def start(self, case):
        child = subprocess.Popen([str(self.binary), *self.node_args(case["node"], "run-once"),
                                  "--engine-socket", str(self.transport / "engine.sock")],
                                 env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        return child

    def finish(self, case, child, state="succeeded", code=0):
        out, err = child.communicate(timeout=45)
        assert child.returncode == 0, (out, err)
        reply = json.loads(out)
        actual = self.status(case)
        assert reply["state"] == actual["state"] == state, (reply, actual)
        assert reply["runtime_removed"] and actual["runtime_removed"]
        if code is not None:
            assert actual["exit_code"] == code, actual
        assert docker("inspect", "treeharbor-" + case["attempt"], check=False).returncode != 0
        assert not (case["mount"] / "treeharbor-execution.json").exists()
        assert case["files"].is_dir()  # completion does not discard saved edits
        assert self.client(*case["request"], credential=case["credential"]) == case["first"]
        return actual

    def stop(self, case, drain=False):
        if drain:
            return self.client("workspace-delete", "--key", "drain-" + case["run"], "--authority", "standalone",
                               "--epoch", "1", "--workspace", case["workspace"], "--generation", "1", credential=case["credential"])
        return self.client("run-stop", "--key", "stop-" + case["run"], "--authority", "standalone",
                           "--epoch", "1", "--run", case["run"], credential=case["credential"])

    def expire_dead_worker(self, case):
        self.sql(f"UPDATE th_outbox SET lease_until=clock_timestamp()-interval '1 second' WHERE kind='run.execute' AND payload->>'run'='{case['run']}' AND state='running'")

    def fault(self, case, mode, drain=False, release="forward"):
        for name in ("claimed", "reached", "release", "done"):
            path = self.transport / name
            if path.exists():
                command("sudo", "rm", path)
        (self.transport / "fault").write_text(mode)
        child = self.start(case)
        wait_until(lambda: (self.transport / "reached").exists() or child.poll() is not None)
        assert child.poll() is None, child.communicate()
        child.kill()
        child.communicate(timeout=10)
        if drain:
            self.stop(case, drain=True)
        (self.transport / "release").write_text(release)
        wait_until(lambda: (self.transport / "done").exists())
        (self.transport / "fault").write_text("none")
        self.expire_dead_worker(case)

    def propose(self, case):
        assert self.status(case)["runtime_removed"]
        upload, changeset = str(uuid.uuid4()), str(uuid.uuid4())
        self.client("change-begin", "--key", "change-begin-" + upload, "--authority", "standalone", "--epoch", "1",
                    "--upload", upload, "--run", case["run"], "--generation", "1", credential=case["credential"])
        # Explicit immutable API bytes, after actual runtime removal. This is
        # deliberately not a claim that a filesystem seal command exists.
        content = (case["files"] / "same.txt").read_bytes()
        blob_file = self.root / ("blob-" + upload)
        blob_file.write_bytes(content)
        blob_file.chmod(0o600)
        blob = self.client("change-put", "--upload", upload, "--file", blob_file, credential=case["credential"])["object"]

        def oid(kind, payload):
            return sha256(b"treeharbor\0" + (1).to_bytes(2, "big") + bytes([kind])
                          + len(payload).to_bytes(8, "big") + payload).digest()

        assert blob == "sha256:" + oid(1, content).hex()
        operations = self.root / ("operations-" + upload + ".json")
        private_json(operations, [{"path_hex": b"same.txt".hex(),
                                   "expected": {"mode": 0o100644, "object": "sha256:" + oid(1, b"base\n").hex()},
                                   "replacement": {"mode": 0o100644, "object": blob}}])
        request = ["change-submit", "--key", "change-submit-" + upload, "--authority", "standalone", "--epoch", "1",
                   "--upload", upload, "--changeset", changeset, "--operations", operations]
        result = self.client(*request, credential=case["credential"])
        assert result["state"] == "accepted" and result["run"] == case["run"]
        assert result["base_revision"] == self.revision
        directory = ((1).to_bytes(4, "big") + (8).to_bytes(4, "big") + b"same.txt"
                     + (0o100644).to_bytes(4, "big") + oid(1, content))
        assert result["result_snapshot"] == "sha256:" + oid(3, oid(2, directory)).hex()
        assert self.client(*request, credential=case["credential"]) == result
        assert self.client("change-status", "--upload", upload, credential=case["credential"])["state"] == "completed"
        return result

    def exercise(self):
        a = self.case("isolation", "agent-a", self.secret, "2000")
        b = self.case("isolation", "agent-b", self.secret, "2000", node=1)
        workers = [self.start(a), self.start(b)]
        wait_until(lambda: self.status(a)["state"] == self.status(b)["state"] == "running")
        ids = [json.loads(docker("inspect", "treeharbor-" + case["attempt"]).stdout)[0]["Id"] for case in (a, b)]
        assert ids[0] != ids[1]
        for case, worker, marker in zip((a, b), workers, ("agent-a", "agent-b")):
            self.finish(case, worker)
            assert (case["files"] / "same.txt").read_text() == marker
        assert self.client("run-status", "--run", a["run"], credential=b["credential"], check=False).returncode != 0
        print("PASS managed CLI: native import, simultaneous isolated agents, exact owner results and replay", flush=True)
        changes = [self.propose(case) for case in (a, b)]
        assert changes[0]["base_snapshot"] == changes[1]["base_snapshot"]
        assert changes[0]["result_snapshot"] != changes[1]["result_snapshot"]
        assert self.sql("SELECT count(*) FROM th_change_versions") == "2"
        print("PASS managed proposals: actual stopped agents submit overlapping edits with distinct durable results and original lineage", flush=True)

        bad = self.case("exit", "23")
        self.finish(bad, self.start(bad), "failed", 23)
        oom = self.case("memory")
        assert self.finish(oom, self.start(oom), "failed", 137)["oom_killed"]
        timed = self.case("attack", timeout=1500)
        self.finish(timed, self.start(timed), "failed", 124)
        count = (timed["files"] / "heartbeat").stat().st_size
        time.sleep(0.2)
        assert count > 0 and (timed["files"] / "heartbeat").stat().st_size == count
        disk = self.case("disk")
        self.finish(disk, self.start(disk))
        assert (b["files"] / "same.txt").read_text() == "agent-b"
        print("PASS managed results: nonzero exit, real OOM, autonomous deadline and bounded persistent disk", flush=True)

        for drain in (False, True):
            case = self.case("heartbeat", timeout=20000)
            worker = self.start(case)
            wait_until(lambda: self.status(case)["state"] == "running" and (case["files"] / "heartbeat").exists())
            assert self.stop(case, drain=drain)["state"] == ("draining" if drain else "stopping")
            self.finish(case, worker, "cancelled", None)
            count = (case["files"] / "heartbeat").stat().st_size
            time.sleep(0.2)
            assert (case["files"] / "heartbeat").stat().st_size == count
        print("PASS managed cancellation/drain: exact runtime gone, descendants stopped, source/files retained", flush=True)

        for mode in ("after-create", "after-remove"):
            case = self.case("once")
            self.fault(case, mode)
            self.finish(case, self.start(case))
            assert (case["files"] / "executions").read_bytes() == b"one execution\n"
        case = self.case("once")
        self.fault(case, "before-start", release="drop")
        self.finish(case, self.start(case), "lost", None)
        assert not (case["files"] / "executions").exists()
        case = self.case("once")
        self.fault(case, "before-start", drain=True)
        self.finish(case, self.start(case), "cancelled", None)
        assert (case["files"] / "executions").read_bytes() == b"one execution\n"
        case = self.case("once", "3000")
        worker = self.start(case)
        wait_until(lambda: (case["files"] / "executions").exists())
        worker.kill()
        worker.communicate(timeout=10)
        self.expire_dead_worker(case)
        self.finish(case, self.start(case))
        assert (case["files"] / "executions").read_bytes() == b"one execution\n"
        print("PASS managed recovery: worker death, lost create/remove responses, late admitted start after drain; no duplicate execution", flush=True)

        case = self.case("once", timeout=7777)
        self.admin("runtime-profile-revoke", *self.scope, "--profile-digest", case["profile"])
        self.finish(case, self.start(case), "cancelled", None)
        assert not (case["files"] / "executions").exists()
        case = self.case("heartbeat", timeout=20000)
        worker = self.start(case)
        wait_until(lambda: self.status(case)["state"] == "running" and (case["files"] / "heartbeat").exists())
        old_credential = case["node"]["credential"]
        token_id = json.loads(old_credential.read_text())["token_id"]
        self.admin("token-revoke", *self.scope, "--token-id", token_id)
        worker.communicate(timeout=40)
        assert worker.returncode != 0
        assert docker("inspect", "treeharbor-" + case["attempt"], check=False).returncode != 0
        assert not self.status(case)["runtime_removed"]  # revoked node cannot publish a completion
        count = (case["files"] / "heartbeat").stat().st_size
        time.sleep(0.2)
        assert (case["files"] / "heartbeat").stat().st_size == count
        rotated = self.root / "node-rotated.json"
        self.admin("token-issue", *self.scope, "--principal", f"node-{case['node_index']}",
                   "--permission", "runtime.control", "--output", rotated)
        case["node"]["credential"] = rotated
        self.expire_dead_worker(case)
        self.finish(case, self.start(case), "lost", None)
        print("PASS managed policy: revoked profile never starts, revoked node stops writers and fresh authorization reconciles", flush=True)

        case = self.case("permissions")
        self.finish(case, self.start(case))
        # Operator restores access only after independently confirmed runtime removal.
        case["files"].chmod(0o700)
        assert (case["files"] / "before-chmod").read_bytes() == b"saved"
        print("PASS task chmod cannot prevent exact runtime cleanup", flush=True)

    def diagnostics(self):
        # Fixture identifiers/state only; no credential files, source bytes or
        # unbounded container logs are printed into CI output.
        for case in self.cases[-3:]:
            try:
                print(self.sql(f"SELECT jsonb_build_object('run',r.run,'state',r.state,'stop',m.stop_requested,'container',m.container,'exit',m.exit_code,'oom',m.oom_killed,'removed',m.removed) FROM th_agent_runs r JOIN th_managed_runs m USING(tenant,repository,run) WHERE r.run='{case['run']}'"), flush=True)
                observed = docker("inspect", "treeharbor-" + case["attempt"], check=False)
                if observed.returncode == 0:
                    value = json.loads(observed.stdout)[0]
                    print(json.dumps({"id": value["Id"], "state": value["State"]}), flush=True)
            except Exception as error:
                print(f"Diagnostic unavailable: {type(error).__name__}", flush=True)
        if self.proxy and (self.root / "proxy.log").exists():
            print((self.root / "proxy.log").read_text()[-2000:], flush=True)

    def cleanup(self):
        for child in self.children:
            if child.poll() is None:
                child.kill()
                child.wait(timeout=10)
        for case in self.cases:
            docker("rm", "--force", "treeharbor-" + case["attempt"], check=False)
        if self.proxy:
            pid = self.transport / "proxy.pid"
            if pid.exists():
                command("sudo", "kill", "-TERM", pid.read_text().strip(), check=False)
            self.proxy.wait(timeout=10)
        for mount in reversed(self.mounts):
            command("sudo", "umount", mount)
        if self.image:
            docker("image", "rm", self.image, check=False)
        docker("exec", self.pg, "psql", "-U", "treeharbor_test", "-d", "treeharbor_test",
               "-c", f"DROP SCHEMA IF EXISTS {self.schema} CASCADE", check=False)
        command("sudo", "rm", "-rf", "--", self.root)


if __name__ == "__main__":
    harness = Harness()
    try:
        harness.setup()
        harness.exercise()
    except Exception:
        harness.diagnostics()
        raise
    finally:
        harness.cleanup()
