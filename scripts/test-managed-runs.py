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
from runtime_target import runtime_target


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
        self.database = "treeharbor_test"
        self.binary = Path(os.environ.get("TH_TEST_SERVICE_BINARY", "target/debug/treeharbor")).absolute()
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
        return docker("exec", self.pg, "psql", "-U", "treeharbor_test", "-d", self.database,
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
        self.agents = [self.principal(name, "agent", ["read", "workspace.write", "change.write", "queue.request"])
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
        target = runtime_target()
        shutil.copy2(Path(os.environ.get("TH_TEST_RUNTIME_INIT", target / "treeharbor-runtime-init")), build / "init")
        shutil.copy2(target / "examples/runtime_probe", build / "probe")
        (build / "Dockerfile").write_text("FROM scratch\nCOPY init /init\nCOPY probe /probe\n")
        self.image = docker("build", "--network=none", "--quiet", build).stdout.strip()
        self.transport = self.root / "proxy"
        self.transport.mkdir(mode=0o700)
        self.start_proxy()
        self.secret = self.root / "host-secret"
        self.secret.write_text("not-mounted")

    def start_proxy(self):
        log = (self.root / "proxy.log").open("a")
        self.proxy = subprocess.Popen(["sudo", "-n", sys.executable, "-B",
                                      str(Path("scripts/test-runtime-controller.py").absolute()), "--proxy",
                                      str(self.transport), str(os.getgid())], stdout=log, stderr=subprocess.STDOUT)
        wait_until(lambda: (self.transport / "proxy.pid").exists())

    def node_args(self, node, action):
        return ["node", action, "--config", str(self.config), "--credential-file", str(node["credential"]),
                "--root", str(node["root"]), "--storage", node["storage"]]

    def placement_profile(self, node=0, timeout=10000):
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
        return number, assigned, slot, mount, digest

    def case(self, mode, *args, node=0, timeout=10000, workspace=None, selected=False):
        number, assigned, slot, mount, digest = self.placement_profile(node, timeout)
        run = str(uuid.uuid4())
        credential = self.agents[node]
        if workspace is None:
            workspace = str(uuid.uuid4())
            self.client("workspace-create", "--key", f"workspace-{number}", "--authority", "standalone", "--epoch", "1",
                        "--workspace", workspace, "--base", self.revision, "--profile-digest", self.source_profile,
                        credential=credential)
        selection = ["--workspace", workspace] if selected else []
        prepared = self.value(*self.node_args(assigned, "prepare-once"), "--slot", slot, *selection)
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
                "profile": digest, "node_index": node, "slot": slot}
        self.cases.append(case)
        return case

    def status(self, case):
        return self.client("run-status", "--run", case["run"], credential=case["credential"])

    def start(self, case):
        child = subprocess.Popen([str(self.binary), *self.node_args(case["node"], "run-once"),
                                  "--engine-socket", str(self.transport / "engine.sock"),
                                  "--run", case["run"]],
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

    def propose(self, case, changeset=None, previous_version=0):
        assert self.status(case)["runtime_removed"]
        upload, changeset = str(uuid.uuid4()), changeset or str(uuid.uuid4())
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
                   "--upload", upload, "--changeset", changeset,
                   "--previous-version", str(previous_version), "--operations", operations]
        result = self.client(*request, credential=case["credential"])
        assert result["state"] == "accepted" and result["run"] == case["run"]
        assert result["version"] == previous_version + 1
        assert result["base_revision"] == self.revision
        directory = ((1).to_bytes(4, "big") + (8).to_bytes(4, "big") + b"same.txt"
                     + (0o100644).to_bytes(4, "big") + oid(1, content))
        assert result["result_snapshot"] == "sha256:" + oid(3, oid(2, directory)).hex()
        assert self.client(*request, credential=case["credential"]) == result
        assert self.client("change-status", "--upload", upload, credential=case["credential"])["state"] == "completed"
        return result

    def request_seal(self, case):
        seal = str(uuid.uuid4())
        request = ["seal-request", "--key", "seal-" + seal, "--authority", "standalone", "--epoch", "1",
                   "--seal", seal, "--run", case["run"], "--generation", "1", "--replace", "same.txt"]
        first = self.client(*request, credential=case["credential"])
        assert first == self.client(*request, credential=case["credential"])
        assert first["state"] == "waiting"
        return seal

    def start_seal(self, case):
        child = subprocess.Popen([str(self.binary), *self.node_args(case["node"], "seal-once"),
                                  "--engine-socket", str(self.transport / "engine.sock")],
                                 env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        return child

    def accept_seal(self, case, seal, content, changeset=None, previous_version=0):
        ready = self.client("seal-status", "--seal", seal, credential=case["credential"])
        assert ready["state"] == "ready"
        request = ["seal-accept", "--key", "accept-" + seal, "--authority", "standalone", "--epoch", "1",
                   "--seal", seal, "--changeset", changeset or str(uuid.uuid4()),
                   "--previous-version", str(previous_version)]
        result = self.client(*request, credential=case["credential"])
        assert result == self.client(*request, credential=case["credential"])
        assert result["state"] == "accepted" and result["seal"] == seal
        assert result["run"] == case["run"] and result["base_revision"] == self.revision
        assert result["version"] == previous_version + 1

        def oid(kind, payload):
            return sha256(b"treeharbor\0" + (1).to_bytes(2, "big") + bytes([kind])
                          + len(payload).to_bytes(8, "big") + payload).digest()

        directory = ((1).to_bytes(4, "big") + (8).to_bytes(4, "big") + b"same.txt"
                     + (0o100644).to_bytes(4, "big") + oid(1, content))
        assert result["result_snapshot"] == "sha256:" + oid(3, oid(2, directory)).hex()
        other = self.agents[1 - case["node_index"]]
        assert self.client("seal-status", "--seal", seal, credential=other, check=False).returncode != 0
        assert self.client(*request, credential=case["node"]["credential"], check=False).returncode != 0
        return result

    def seal(self, case, changeset=None, previous_version=0):
        content = (case["files"] / "same.txt").read_bytes()
        seal = self.request_seal(case)
        child = self.start_seal(case)
        out, err = child.communicate(timeout=45)
        assert child.returncode == 0, (out, err)
        assert json.loads(out)["state"] == "ready", out
        return self.accept_seal(case, seal, content, changeset, previous_version)

    def seal_recovery(self, case, remove_marker):
        # Delay only the metadata declaration, after real stopped-file capture
        # and its durability barriers. Kill the actual worker at this boundary.
        content = (case["files"] / "same.txt").read_bytes()
        seal = self.request_seal(case)
        self.sql("CREATE FUNCTION capture_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_sleep(45); RETURN NEW; END $$; CREATE TRIGGER capture_fault BEFORE INSERT ON th_seal_artifacts FOR EACH ROW EXECUTE FUNCTION capture_fault()")
        child = self.start_seal(case)
        record = case["node"]["root"] / "seals" / seal
        wait_until(lambda: (record / "complete.json").exists() or child.poll() is not None)
        assert child.poll() is None, child.communicate()
        wait_until(lambda: self.sql("SELECT count(*) FROM pg_stat_activity WHERE application_name='treeharbor' AND wait_event='PgSleep' AND query LIKE 'INSERT INTO th_seal_artifacts%' ") == "1")
        manifest = (record / "artifact" / "manifest.json").read_bytes()
        child.kill()
        child.communicate(timeout=10)
        self.sql("DROP TRIGGER capture_fault ON th_seal_artifacts; DROP FUNCTION capture_fault()")
        assert self.client("seal-status", "--seal", seal, credential=case["credential"])["state"] == "capturing"
        self.sql(f"UPDATE th_workspace_seals SET lease_until=clock_timestamp()-interval '1 second' WHERE seal='{seal}'")
        if remove_marker:
            # Exercise the ambiguous artifact-without-node-marker state. The
            # successor must re-prove physical removal, then reuse exact bytes.
            (record / "complete.json").unlink()
        (case["files"] / "same.txt").write_bytes(b"operator changed mutable files after capture\n")
        resumed = self.start_seal(case)
        out, err = resumed.communicate(timeout=45)
        assert resumed.returncode == 0, (out, err)
        assert json.loads(out)["state"] == "ready"
        assert (record / "artifact" / "manifest.json").read_bytes() == manifest
        self.accept_seal(case, seal, content)
        (case["files"] / "same.txt").write_bytes(content)

    def start_cleanup(self, case):
        child = subprocess.Popen([str(self.binary), *self.node_args(case["node"], "cleanup-once"),
                                  "--workspace", case["workspace"],
                                  "--engine-socket", str(self.transport / "engine.sock")],
                                 env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.children.append(child)
        return child

    def cleanup_workspace(self, case, already_drained=False, fault=None):
        if not already_drained:
            self.stop(case, drain=True)
        state = self.client("workspace-status", "--workspace", case["workspace"], credential=case["credential"])
        assert state["state"] == "draining"
        placement = case["mount"] / "placement"
        identity = (placement.stat().st_dev, placement.stat().st_ino)
        if fault == "unlink":
            # Many small directories make the partial-unlink window observable
            # on fast CI disks without filling the bounded volume's inode quota.
            (case["files"] / "000-cleanup-marker").touch()
            for i in range(500):
                directory = case["files"] / f"zz-cleanup-{i:04d}"
                directory.mkdir()
                for j in range(8):
                    (directory / str(j)).touch()
        elif fault == "receipt":
            self.sql("CREATE FUNCTION cleanup_fault() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='deleted' THEN PERFORM pg_sleep(10); END IF; RETURN NEW; END $$; CREATE TRIGGER cleanup_fault BEFORE UPDATE ON th_workspace_cleanups FOR EACH ROW EXECUTE FUNCTION cleanup_fault()")
        child = self.start_cleanup(case)
        if fault:
            if fault == "unlink":
                deadline = time.monotonic() + 20
                while (case["files"] / "000-cleanup-marker").exists() and child.poll() is None:
                    assert time.monotonic() < deadline
                    time.sleep(0.001)
                assert child.poll() is None, child.communicate()
                child.kill()
                child.communicate(timeout=10)
                assert (case["files"] / "zz-cleanup-0499").exists(), "missed partial deletion boundary"
            else:
                wait_until(lambda: self.sql("SELECT count(*) FROM pg_stat_activity WHERE application_name='treeharbor' AND wait_event='PgSleep' AND query LIKE 'UPDATE th_workspace_cleanups%' ") == "1")
                child.kill()
                child.communicate(timeout=10)
                self.sql("DROP TRIGGER cleanup_fault ON th_workspace_cleanups; DROP FUNCTION cleanup_fault()")
            # Only the killed worker's lease is expired; runtime observations and
            # source/result bytes come exclusively from the actual product path.
            self.sql(f"UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()-interval '1 second' WHERE workspace='{case['workspace']}'")
            state = self.client("workspace-status", "--workspace", case["workspace"], credential=case["credential"])
            assert state["state"] == "draining" and state["cleanup"]["state"] == "deleting", state
            record = case["node"]["root"] / "cleanup" / state["cleanup"]["id"]
            intent = (record / "intent.json").read_bytes()
            assert (record / "complete.json").exists() == (fault == "receipt")
            child = self.start_cleanup(case)
        out, err = child.communicate(timeout=45)
        assert child.returncode == 0, (out, err)
        result = json.loads(out)
        assert result["state"] == "deleted" and result["workspace"] == case["workspace"]
        assert not list(placement.iterdir())
        assert identity == (placement.stat().st_dev, placement.stat().st_ino)
        assert not (case["mount"] / "treeharbor-execution.json").exists()
        if fault:
            assert (record / "intent.json").read_bytes() == intent
        replay = self.start_cleanup(case)
        out, err = replay.communicate(timeout=45)
        assert replay.returncode == 0, (out, err)
        assert json.loads(out) == result
        state = self.client("workspace-status", "--workspace", case["workspace"], credential=case["credential"])
        assert state["state"] == state["cleanup"]["state"] == "deleted"
        assert state["cleanup"]["id"] == result["cleanup"]
        other = next(c for c in self.agents if c != case["credential"])
        assert self.client("workspace-status", "--workspace", case["workspace"], credential=other, check=False).returncode != 0
        return result

    def cleanup_reuse(self, case):
        result = self.cleanup_workspace(case, fault="unlink")
        workspace = str(uuid.uuid4())
        self.client("workspace-create", "--key", "reuse-" + workspace, "--authority", "standalone", "--epoch", "1",
                    "--workspace", workspace, "--base", self.revision, "--profile-digest", self.source_profile,
                    credential=case["credential"])
        prepared = self.value(*self.node_args(case["node"], "prepare-once"), "--slot", case["slot"])
        assert prepared["workspace"] == workspace
        assert Path(prepared["path"]) == case["files"]
        (case["files"] / "same.txt").write_text("successor edits")
        run = str(uuid.uuid4())
        request = ["run-request", "--key", "reuse-run-" + run, "--authority", "standalone", "--epoch", "1",
                   "--run", run, "--workspace", workspace, "--generation", "1", "--profile-digest", case["profile"],
                   "--agent", "replacement", "--task", "reuse", "--", "/probe", "once", "3000"]
        first = self.client(*request, credential=case["credential"])
        successor = dict(case, workspace=workspace, run=run, request=request, first=first,
                         attempt=self.sql(f"SELECT attempt FROM th_managed_runs WHERE run='{run}'"))
        self.cases.append(successor)
        active = self.start(successor)
        wait_until(lambda: (case["files"] / "executions").exists())
        replay = self.start_cleanup(case)
        out, err = replay.communicate(timeout=45)
        assert replay.returncode == 0, (out, err)
        assert json.loads(out) == result
        assert (case["files"] / "same.txt").read_text() == "successor edits"
        assert self.status(successor)["state"] == "running"
        self.finish(successor, active)
        assert (case["files"] / "executions").read_bytes() == b"one execution\n"
        self.cleanup_workspace(successor)

    def exercise(self):
        earlier = str(uuid.uuid4())
        self.client("workspace-create", "--key", "selected-preparation-earlier", "--authority", "standalone", "--epoch", "1",
                    "--workspace", earlier, "--base", self.revision, "--profile-digest", self.source_profile,
                    credential=self.agents[0])
        # Two pending workspaces on the same node: prepare the later one first.
        first = self.case("once", selected=True)
        for selected in [str(uuid.uuid4()), first["workspace"]]:
            idle = self.value(*self.node_args(first["node"], "prepare-once"),
                              "--slot", first["slot"], "--workspace", selected)
            assert idle == {"state": "idle"}
        nil = self.cli(*self.node_args(first["node"], "prepare-once"),
                       "--slot", first["slot"], "--workspace", uuid.UUID(int=0), check=False)
        assert nil.returncode != 0
        pending = self.client("workspace-status", "--workspace", earlier, credential=self.agents[0])
        assert pending["state"] == "requested"
        assert self.sql(f"SELECT count(*) FROM th_workspace_preparations WHERE workspace='{earlier}'") == "0"
        second = self.case("once", workspace=earlier, selected=True)
        assert first["workspace"] != second["workspace"] == earlier
        print("PASS selected preparation: later pending workspace is prepared exactly; unknown/completed selection never consumes the earlier job", flush=True)
        idle = self.value(*self.node_args(first["node"], "run-once"),
                          "--engine-socket", self.transport / "engine.sock", "--run", uuid.uuid4())
        assert idle == {"state": "idle"}
        assert not (first["files"] / "executions").exists()
        assert not (second["files"] / "executions").exists()
        self.finish(second, self.start(second))
        assert self.status(first)["state"] == "requested"
        assert not (first["files"] / "executions").exists()
        replay = self.value(*self.node_args(first["node"], "run-once"),
                            "--engine-socket", self.transport / "engine.sock", "--run", second["run"])
        assert replay == {"state": "idle"}
        assert self.status(first)["state"] == "requested"
        self.finish(first, self.start(first))
        print("PASS selected execution: unknown or completed run never starts another task; later queued run executes before earlier work on the same node", flush=True)
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
        captured = [self.seal(case, change["changeset"], 1) for case, change in zip((a, b), changes)]
        assert [c["result_snapshot"] for c in captured] == [c["result_snapshot"] for c in changes]
        assert self.sql("SELECT count(*) FROM th_change_versions") == "4"
        print("PASS managed capture: real two-agent files, original lineage, version CAS, owner-only acceptance and exact independent trees", flush=True)
        for remove_marker in (False, True):
            self.seal_recovery(a, remove_marker)
        print("PASS managed capture recovery: actual worker death after durable files, missing completion marker and changed mutable source never recapture", flush=True)

        writing = self.case("attack", timeout=20000)
        worker = self.start(writing)
        wait_until(lambda: (writing["files"] / "heartbeat").exists())
        pending = self.request_seal(writing)
        self.finish(writing, worker, "cancelled", None)
        count = (writing["files"] / "heartbeat").stat().st_size
        capture = self.start_seal(writing)
        out, err = capture.communicate(timeout=45)
        assert capture.returncode == 0, (out, err)
        self.accept_seal(writing, pending, b"base\n")
        assert (writing["files"] / "heartbeat").stat().st_size == count
        print("PASS managed capture stop: request terminates active writers and descendants before taking selected files", flush=True)

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

        # The removal engine must handle agent permissions and untrusted links,
        # leaving outside data intact and retaining the registered placement inode.
        (case["files"] / "outside").symlink_to(self.secret)
        (case["files"] / "locked").mkdir()
        (case["files"] / "locked" / "file").write_text("remove")
        (case["files"] / "locked").chmod(0)
        case["files"].chmod(0)
        self.cleanup_workspace(case)
        assert self.secret.read_text() == "not-mounted"
        print("PASS workspace cleanup: actual removed runtime, chmod000 files, symlink containment and retained placement", flush=True)
        self.cleanup_reuse(a)
        print("PASS workspace cleanup recovery: actual worker killed during unlink, identical intent, same-slot reuse and harmless old replay", flush=True)
        self.stop(b, drain=True)
        journal = b["node"]["root"] / "runtime" / b["attempt"]
        marker = journal / "removed.json"
        saved = journal / "removed.saved"
        marker.rename(saved)
        refused = self.start_cleanup(b)
        out, err = refused.communicate(timeout=45)
        assert refused.returncode != 0, (out, err)
        assert (b["files"] / "same.txt").read_text() == "agent-b"
        saved.rename(marker)
        self.sql(f"UPDATE th_workspace_cleanups SET lease_until=clock_timestamp()-interval '1 second' WHERE workspace='{b['workspace']}'")
        print("PASS workspace cleanup proof: a missing real runtime journal marker blocks deletion despite database removal", flush=True)
        self.cleanup_workspace(b, fault="receipt")
        print("PASS workspace cleanup receipt: killed worker after durable deletion retries without recreating files", flush=True)
        self.cleanup_workspace(writing)
        assert self.sql("SELECT count(*) FROM th_change_versions") == "7"
        for change in captured:
            credential = a["credential"] if change["run"] == a["run"] else b["credential"]
            diff = self.client("diff", "--authority", "standalone", "--epoch", "1", "change",
                               "--changeset", change["changeset"], "--version", "2", credential=credential)
            assert diff["after_snapshot"] == change["result_snapshot"]
        print("PASS workspace cleanup retention: accepted overlapping agent changes remain inspectable after physical deletion", flush=True)

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
