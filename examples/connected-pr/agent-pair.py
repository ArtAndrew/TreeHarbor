#!/usr/bin/env python3
"""Experimental operator helper: two prepared agents -> immutable changes -> PRs.

Uses the installed Rust CLI; it does not create credentials, configure a runtime,
prepare workspaces, approve checks or merge code. Preserve its private journal
on failure. Credential and service files are read only by the Rust CLI.
"""
import argparse
from concurrent.futures import ThreadPoolExecutor, as_completed
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile
import time
import uuid

sys.dont_write_bytecode = True

class DriverError(Exception):
    """Only fixed, non-secret diagnostic text may be exposed to the operator."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code = code

def require(ok, code, message):
    if not ok:
        raise DriverError(code, message)

def encoded(value):
    return json.dumps(value, ensure_ascii=False, sort_keys=True,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")

def sha(value):
    return hashlib.sha256(encoded(value)).hexdigest()

def identifier(value):
    try:
        return isinstance(value, str) and str(uuid.UUID(value)) == value and uuid.UUID(value).int != 0
    except (ValueError, TypeError, AttributeError):
        return False

def fields(value, required, optional=()):
    require(isinstance(value, dict) and set(required) <= set(value)
            and set(value) <= set(required) | set(optional), "invalid_config", "Unexpected or missing configuration fields.")

def absolute(value):
    return isinstance(value, str) and Path(value).is_absolute() and "\x00" not in value

def integer(value, low, high):
    return type(value) is int and low <= value <= high

def private_directory(path, create=False):
    if create:
        path.mkdir(mode=0o700, parents=False, exist_ok=True)
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and not path.is_symlink() and info.st_uid == os.geteuid()
            and info.st_mode & 0o077 == 0, "unsafe_state", "Private state directories must be owned by this user with mode 0700.")

def read_private(path, limit=1024*1024):
    def unique(items):
        value = {}
        for key, item in items:
            require(key not in value, "invalid_json", "Duplicate JSON keys are refused.")
            value[key] = item
        return value
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as handle:
        info = os.fstat(handle.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0
                and info.st_size <= limit, "unsafe_file", "Private inputs must be bounded regular files owned by this user, mode 0600.")
        data = handle.read(limit + 1)
    require(len(data) <= limit, "unsafe_file", "Private input exceeds its limit.")
    return json.loads(data, object_pairs_hook=unique, parse_constant=lambda _: (_ for _ in ()).throw(DriverError("invalid_json", "Nonfinite JSON values are refused.")))

def sync_directory(root):
    fd = os.open(root, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)

def write_atomic(path, value):
    fd, temporary = tempfile.mkstemp(prefix=".pending-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as handle:
            handle.write(encoded(value) + b"\n")
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(temporary, path)
        sync_directory(path.parent)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)

class Journal:
    def __init__(self, root, initialize):
        self.root, self.initialize = root, initialize

    def __enter__(self):
        private_directory(self.root, self.initialize)
        fd = os.open(self.root / "driver.lock", os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
        self.lock = os.fdopen(fd, "rb+")
        try:
            info = os.fstat(fd)
            require(stat.S_ISREG(info.st_mode) and info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0, "unsafe_state", "Unsafe journal lock.")
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise DriverError("busy", "Another driver owns this workflow journal.") from None
            sync_directory(self.root)
            return self
        except BaseException:
            self.lock.close()
            raise

    def __exit__(self, *_):
        self.lock.close()

class SubprocessRunner:
    def __call__(self, argv, label):
        try:
            process = subprocess.run(argv, capture_output=True, timeout=420, check=False)
        except (OSError, subprocess.TimeoutExpired):
            raise DriverError("command_unknown", "Command did not return a usable reply; preserve state and retry the same action or explicit recovery.") from None
        require(process.returncode == 0, "command_failed", "Product command failed; preserve its journals. No subprocess text or private paths are printed.")
        require(len(process.stdout) <= 2*1024*1024, "invalid_reply", "Product reply exceeds the driver limit.")
        try:
            result = json.loads(process.stdout)
        except (ValueError, UnicodeError):
            raise DriverError("invalid_reply", "Product returned an invalid JSON reply; preserve state.") from None
        require(isinstance(result, dict), "invalid_reply", "Product reply must be an object.")
        return result.get("result", result)

def verify_selected_binary(config, expected):
    # Pin an independently tested artifact. This is not a substitute for the
    # product's no-fallback/auth/lease tests and does not auto-enable a flag.
    fd = os.open(config["binary"], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as binary:
        info = os.fstat(binary.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_size <= 512*1024*1024 and info.st_mode & 0o111,
                "binary_unverified", "Selected task needs the exact independently tested executable artifact.")
        digest = hashlib.sha256()
        while block := binary.read(128*1024):
            digest.update(block)
    require(digest.hexdigest() == expected, "binary_unverified", "Selected task binary differs from its independently verified pin; no worker started.")

Error = DriverError
TERMINAL = ("succeeded", "failed", "cancelled", "lost")
ACTIONS = ("initialize", "request", "execute", "capture", "publish", "status")


def digest(value, native=False):
    return isinstance(value, str) and re.fullmatch(("native:" if native else "") + r"sha256:[0-9a-f]{64}", value)


def current_path(value):
    # Credential validity and grants are checked by the Rust service, never by
    # filename conventions. The operator supplies current scoped credentials.
    return absolute(value)


def validate(c):
    fields(c, ("version", "profile", "tenant", "repository", "epoch", "binary", "binary_sha256",
                      "socket", "service_config", "state", "engine_socket", "baseline", "agents", "wait"), ("capture_worker", "github"))
    require(type(c["version"]) is int and c["version"] == 1 and c["profile"] == "connected-prepared-agent-pair-v1",
            "config", "Unsupported pair stage.")
    require(identifier(c["repository"]) and integer(c["epoch"], 1, 2**63-1)
            and isinstance(c["tenant"], str) and 0 < len(c["tenant"]) <= 128, "config", "Invalid repository scope.")
    require(all(absolute(c[k]) for k in ("binary", "socket", "service_config", "state", "engine_socket"))
            and re.fullmatch(r"[0-9a-f]{64}", c["binary_sha256"]), "config", "Use absolute paths and a verified binary pin.")
    fields(c["baseline"], ("revision", "snapshot"))
    require(digest(c["baseline"]["revision"], True) and digest(c["baseline"]["snapshot"]), "config", "An exact imported native baseline is required.")
    fields(c["wait"], ("attempts", "interval_seconds"))
    require(integer(c["wait"]["attempts"], 1, 60) and integer(c["wait"]["interval_seconds"], 1, 5), "config", "Wait bounds exceed the reference profile.")
    require(isinstance(c["agents"], list) and len(c["agents"]) == 2, "config", "Exactly two prepared agents are required.")
    identities, keys = [], []
    for a in c["agents"]:
        fields(a, ("label", "workspace", "generation", "run", "seal", "changeset", "credential", "runtime_profile",
                          "argv", "node", "prepared", "selection", "keys"))
        require(a["label"] in ("A", "B") and integer(a["generation"], 1, 2**63-1)
                and all(identifier(a[k]) for k in ("workspace", "run", "seal", "changeset")), "config", "Invalid agent identities.")
        require(current_path(a["credential"]),
                "config", "Use an absolute scoped credential path.")
        require(digest(a["runtime_profile"]) and isinstance(a["argv"], list) and 1 <= len(a["argv"]) <= 128
                and absolute(a["argv"][0]) and all(isinstance(x, str) and "\0" not in x for x in a["argv"])
                and sum(len(x.encode()) for x in a["argv"]) <= 60*1024, "config", "Invalid approved task.")
        fields(a["node"], ("root", "storage", "slot", "credential"))
        require(absolute(a["node"]["root"]) and current_path(a["node"]["credential"])
                and all(identifier(a["node"][k]) for k in ("storage", "slot")), "config", "Invalid assigned node.")
        fields(a["prepared"], ("path", "manifest"))
        require(all(absolute(x) for x in a["prepared"].values()), "config", "Pin the actual prepared path/manifest.")
        fields(a["selection"], ("replace", "delete", "exclude"))
        require(a["selection"]["replace"] or a["selection"]["delete"], "config", "Select explicit task outputs.")
        for paths in a["selection"].values():
            require(isinstance(paths, list) and len(paths) <= 64 and len(set(paths)) == len(paths)
                    and all(isinstance(p, str) and 0 < len(p) <= 1024 and not p.startswith("/")
                            and "\\" not in p and "\0" not in p and all(x not in ("", ".", "..") for x in p.split("/")) for p in paths),
                    "config", "Use explicit bounded repository-relative paths; no glob or shell expansion.")
        fields(a["keys"], ("request", "seal", "accept"))
        require(all(isinstance(k, str) and re.fullmatch(r"[A-Za-z0-9_.:-]{1,128}", k) for k in a["keys"].values()), "config", "Save immutable product keys.")
        keys.extend(a["keys"].values())
        identities.extend(a[k] for k in ("workspace", "run", "seal", "changeset"))
    a, b = c["agents"]
    require({a["label"], b["label"]} == {"A", "B"} and len(set(identities)) == 8 and len(set(keys)) == 6,
            "config", "Agent identities and operation keys must be distinct.")
    for field in ("root", "storage", "slot"):
        require(a["node"][field] != b["node"][field], "config", "Parallel workers require independent node roots/storage/slots.")
    roots = [Path(x["node"]["root"]).resolve() for x in c["agents"]]
    placements = [Path(x["prepared"]["path"]).resolve() for x in c["agents"]]
    require(not roots[0].is_relative_to(roots[1]) and not roots[1].is_relative_to(roots[0])
            and a["prepared"]["path"] != b["prepared"]["path"], "config", "Prepared placements cannot overlap.")
    require(all(not Path(c["state"]).resolve().is_relative_to(r) for r in roots), "config", "Keep the pair journal outside node roots.")
    operator_paths = [c[k] for k in ("state", "binary", "service_config", "socket", "engine_socket")]
    operator_paths += [a["credential"] for a in c["agents"]] + [a["node"]["credential"] for a in c["agents"]]
    require(all(not Path(p).resolve().is_relative_to(r) for p in operator_paths for r in placements),
            "config", "Keep operator files, credentials and journals outside agent files.")
    if "capture_worker" in c:
        fields(c["capture_worker"], ("version", "mode", "verified_binary_sha256"))
        require(c["capture_worker"] == {"version": 1, "mode": "selected-seal-v1", "verified_binary_sha256": c["binary_sha256"]},
                "config", "Selected capture needs the exact independently tested --seal binary pin.")
    if "github" in c:
        g = c["github"]
        fields(g, ("binding", "owner_credential", "publisher_credential", "app_config", "git", "base", "ttl_seconds", "pulls"))
        require(identifier(g["binding"]) and integer(g["ttl_seconds"], 1, 3600), "config", "Invalid GitHub binding/deadline.")
        require(all(absolute(g[k]) for k in ("owner_credential", "publisher_credential", "app_config", "git")),
                "config", "Use absolute operator credential, App and Git paths.")
        require(isinstance(g["base"], str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._/-]{0,127}", g["base"]),
                "config", "Invalid GitHub base; Rust also validates the exact ref.")
        fields(g["pulls"], ("A", "B"))
        for item in g["pulls"].values():
            fields(item, ("title", "body", "draft"))
            require(isinstance(item["title"], str) and 0 < len(item["title"].encode()) <= 256
                    and item["title"].strip() == item["title"] and not any(ord(x) < 32 for x in item["title"])
                    and isinstance(item["body"], str) and len(item["body"].encode()) <= 8192 and "\0" not in item["body"]
                    and type(item["draft"]) is bool, "config", "Invalid explicit PR text/draft selection.")
        protected = [g[k] for k in ("owner_credential", "publisher_credential", "app_config")]
        execution_credentials = [a["credential"] for a in c["agents"]] + [a["node"]["credential"] for a in c["agents"]]
        require(len(set(protected)) == 3 and not set(protected).intersection(execution_credentials),
                "config", "Keep operator publication credentials separate from agent credentials.")
        require(all(not Path(p).resolve().is_relative_to(r) for p in protected for r in roots + placements),
                "config", "Operator credentials must remain outside agent/node files.")
    return c


class Pair:
    def __init__(self, config, runner=None, sleeper=time.sleep):
        self.c = validate(config)
        self.root = Path(config["state"])
        self.runner = runner or SubprocessRunner()
        self.sleep = sleeper

    def save(self):
        write_atomic(self.root / "pair.json", self.state)

    def cli(self, label, argv):
        value = self.runner([self.c["binary"], *map(str, argv)], label)
        require(isinstance(value, dict), "reply", "Product reply must be an object.")
        return value

    def client(self, a, label, *argv, credential=None):
        return self.cli(a["label"] + ":" + label, ["client", "--socket", self.c["socket"], "--credential-file", credential or a["credential"], *argv])

    def node_argv(self, a, action, *args):
        return ["node", action, "--config", self.c["service_config"], "--credential-file", a["node"]["credential"],
                "--root", a["node"]["root"], "--storage", a["node"]["storage"], *args]

    def mutation(self, a, action, key, argv, kind, credential=None, direct=False):
        journal = self.state["agents"][a["label"]]
        intent = {"key": key, "argv_digest": sha(argv)}
        old = journal["operations"].get(action)
        if old:
            require(all(old.get(k) == v for k, v in intent.items()), "intent_changed", "Restore the original immutable request.")
            if "reply" in old:
                return old["reply"]
            receipt = self.client(a, "operation-status", "operation-status", "--key", key, credential=credential)
            require(receipt.get("key") == key and receipt.get("state") == "committed" and receipt.get("kind") == kind
                    and isinstance(receipt.get("result"), dict), "unknown", "Unknown mutation remains stopped; inspect the exact product key.")
            result = receipt["result"]
        else:
            journal["operations"][action] = intent
            self.save()  # Written before sending. No replacement IDs after lost replies.
            result = self.cli(a["label"] + ":" + action, argv) if direct else self.client(a, action, *argv, credential=credential)
        journal["operations"][action]["reply"] = result
        self.save()
        return result

    def status(self, a):
        v = self.client(a, "run-status", "run-status", "--run", a["run"])
        require(all(v.get(k) == a[k] for k in ("run", "workspace", "generation")) and v.get("epoch") == self.c["epoch"]
                and type(v.get("generation")) is int and type(v.get("epoch")) is int
                and v.get("state") in ("requested", "launching", "running", "stopping", *TERMINAL)
                and type(v.get("runtime_removed")) is bool, "identity", "Run status does not identify the exact scoped execution.")
        return v

    def terminal(self, a, v):
        require(v["state"] == "succeeded" and v["runtime_removed"] is True and type(v.get("exit_code")) is int and v["exit_code"] == 0
                and v.get("oom_killed") is False, "not_terminal", "Both reference agents must succeed with confirmed runtime removal.")
        return v

    def readiness(self, a):
        v = self.client(a, "workspace-status", "workspace-status", "--workspace", a["workspace"])
        require(v.get("workspace") == a["workspace"] and v.get("generation") == a["generation"]
                and type(v.get("generation")) is int and type(v.get("epoch")) is int
                and v.get("epoch") == self.c["epoch"] and v.get("base") == self.c["baseline"]["revision"]
                and v.get("state") == "ready" and v.get("format") == "native" and v.get("profile_resolved") is True,
                "placement", "Workspace is not ready at the exact imported baseline.")
        v = self.cli(a["label"] + ":prepared", self.node_argv(a, "prepared", "--workspace", a["workspace"]))
        require(v.get("workspace") == a["workspace"] and v.get("state") == "files_ready" and v.get("runtime") is None
                and v.get("snapshot") == self.c["baseline"]["snapshot"]
                and all(v.get(k) == a["prepared"][k] for k in ("path", "manifest")), "placement", "Prepared source/placement differs from the saved receipt.")

    def requested_receipt(self, a, v):
        require(v.get("run") == a["run"] and v.get("workspace") == a["workspace"] and v.get("state") == "requested",
                "identity", "Run request receipt identifies a different execution.")

    def request(self):
        for a in self.c["agents"]:
            if "request" not in self.state["agents"][a["label"]]["operations"]:
                self.readiness(a)
            argv = ["run-request", "--key", a["keys"]["request"], "--authority", "connected", "--epoch", self.c["epoch"],
                    "--run", a["run"], "--workspace", a["workspace"], "--generation", a["generation"],
                    "--profile-digest", a["runtime_profile"], "--agent", "reference-" + a["label"], "--task", "reference-" + a["label"], "--", *a["argv"]]
            v = self.mutation(a, "request", a["keys"]["request"], argv, "run.request.v1")
            self.requested_receipt(a, v)
            self.status(a)

    def wait_terminal(self):
        for attempt in range(self.c["wait"]["attempts"]):
            values = {a["label"]: self.status(a) for a in self.c["agents"]}
            if all(v["state"] in TERMINAL for v in values.values()):
                for a in self.c["agents"]:
                    self.terminal(a, values[a["label"]])
                    self.state["agents"][a["label"]]["terminal"] = values[a["label"]]
                self.save()
                return
            if attempt + 1 < self.c["wait"]["attempts"]:
                self.sleep(self.c["wait"]["interval_seconds"])
        raise Error("unknown", "Bounded status wait ended; no worker is restarted. Preserve node journals.")

    def execute(self):
        scheduled = []
        for a in self.c["agents"]:
            j = self.state["agents"][a["label"]]
            require("request" in j["operations"] and "reply" in j["operations"]["request"], "request_required", "Complete both exact run requests first.")
            self.requested_receipt(a, j["operations"]["request"]["reply"])
            require(not j.get("blocked"), "blocked", "A mismatching worker response requires operator review.")
            v = self.status(a)
            if v["state"] in TERMINAL:
                self.terminal(a, v)
                continue
            if j.get("execution_issued"):
                continue  # Status-only recovery; never start another worker.
            require(v["state"] == "requested" and not v["runtime_removed"], "unknown", "An unowned active execution remains stopped.")
            argv = self.node_argv(a, "run-once", "--engine-socket", self.c["engine_socket"], "--run", a["run"])
            scheduled.append((a, argv))
        if scheduled:
            verify_selected_binary(self.c, self.c["binary_sha256"])
            for a, argv in scheduled:
                self.state["agents"][a["label"]]["execution_issued"] = {"argv_digest": sha(argv)}
            self.save()  # Both intents durable before concurrent dispatch.
            with ThreadPoolExecutor(max_workers=2) as pool:
                futures = {pool.submit(self.cli, a["label"] + ":execute", argv): a for a, argv in scheduled}
                for future in as_completed(futures):
                    a = futures[future]
                    j = self.state["agents"][a["label"]]
                    try:
                        v = future.result()
                        require(v.get("run") == a["run"] and v.get("state") == "succeeded"
                                and v.get("runtime_removed") is True, "identity", "Worker did not return the exact removed successful run.")
                        j["execution_reply"] = v
                    except Error as e:
                        j["execution_unknown"] = e.code
                        if e.code == "identity":
                            j["blocked"] = True
                    except Exception:
                        j["execution_unknown"] = "command_unknown"
                    self.save()
        require(not any(j.get("blocked") for j in self.state["agents"].values()), "blocked", "Mismatching worker response; no capture is permitted.")
        self.wait_terminal()

    def seal_status(self, a):
        v = self.client(a, "seal-status", "seal-status", "--seal", a["seal"])
        require(v.get("seal") == a["seal"] and v.get("run") == a["run"] and v.get("generation") == a["generation"]
                and type(v.get("generation")) is int and type(v.get("epoch")) is int
                and v.get("epoch") == self.c["epoch"] and v.get("state") in ("waiting", "capturing", "ready", "failed", "cancelled"),
                "identity", "Seal status does not identify the exact request.")
        return v

    def capture(self):
        for a in self.c["agents"]:
            j = self.state["agents"][a["label"]]
            require(not j.get("blocked") and "terminal" in j, "terminal_required", "Verify both actual removed terminal runs before capture.")
            self.terminal(a, self.status(a))
        for a in self.c["agents"]:
            j = self.state["agents"][a["label"]]
            if "accepted" in j:
                continue
            argv = ["seal-request", "--key", a["keys"]["seal"], "--authority", "connected", "--epoch", self.c["epoch"],
                    "--seal", a["seal"], "--run", a["run"], "--generation", a["generation"],
                    "--max-objects", 10000, "--max-bytes", 67108864, "--ttl-seconds", 900]
            for kind, paths in a["selection"].items():
                for p in paths:
                    argv.extend(["--" + kind, p])
            receipt = self.mutation(a, "seal", a["keys"]["seal"], argv, "workspace.seal.request.v1")
            require(receipt.get("seal") == a["seal"] and receipt.get("run") == a["run"], "identity", "Wrong seal request receipt.")
            v = self.seal_status(a)
            if v["state"] != "ready":
                require(self.c.get("capture_worker") is not None, "capture_selector_required", "No unqualified seal-once: opt in only after real --seal selector acceptance, or inspect a preexisting ready seal.")
                require(not j.get("capture_issued"), "unknown", "Unknown capture is status-only; never restart the worker.")
                require(v["state"] == "waiting" and v.get("unexpired") is True, "unknown", "Seal is unavailable or expired; preserve its identity.")
                verify_selected_binary(self.c, self.c["capture_worker"]["verified_binary_sha256"])
                argv = self.node_argv(a, "seal-once", "--engine-socket", self.c["engine_socket"], "--seal", a["seal"])
                j["capture_issued"] = {"argv_digest": sha(argv)}
                self.save()
                try:
                    reply = self.cli(a["label"] + ":capture", argv)
                    require(reply.get("seal") == a["seal"] and reply.get("run") == a["run"]
                            and reply.get("state") == "ready", "identity", "Selected capture returned a different seal.")
                except Error as e:
                    if e.code == "identity":
                        j["blocked"] = True
                        self.save()
                    raise
                v = self.seal_status(a)
            require(v["state"] == "ready" and isinstance(v.get("manifest_digest"), str)
                    and re.fullmatch(r"[0-9a-f]{64}", v["manifest_digest"]) and digest(v.get("operations")),
                    "capture_not_ready", "Exact immutable seal is not ready.")
            argv = ["seal-accept", "--key", a["keys"]["accept"], "--authority", "connected", "--epoch", self.c["epoch"],
                    "--seal", a["seal"], "--changeset", a["changeset"], "--previous-version", 0, "--ttl-seconds", 300]
            r = self.mutation(a, "accept", a["keys"]["accept"], argv, "workspace.seal.accept.v1")
            require(r.get("state") == "accepted" and r.get("changeset") == a["changeset"] and type(r.get("version")) is int and r["version"] == 1
                    and r.get("run") == a["run"] and r.get("seal") == a["seal"]
                    and r.get("base_revision") == self.c["baseline"]["revision"] and r.get("base_snapshot") == self.c["baseline"]["snapshot"]
                    and r.get("manifest_digest") == v["manifest_digest"] and r.get("operations") == v["operations"]
                    and digest(r.get("result_snapshot")), "identity", "Accepted ChangeSet differs from exact run/base/seal.")
            j["accepted"] = r
            self.save()

    def owner_request(self, a, action, payload, kind):
        # The filename binds the payload as well as the argv recorded by mutation.
        path = self.root / (a["label"] + "-" + action + "-" + sha(payload) + ".json")
        if path.exists():
            require(read_private(path) == payload, "intent_changed", "Restore the original owner request.")
        else:
            write_atomic(path, payload)
        return self.mutation(a, action, payload["key"], ["github", "--request", str(path)], kind,
                             credential=self.c["github"]["owner_credential"])

    def transport(self, a, stage, argv):
        j = self.state["agents"][a["label"]]["github"]
        intent = {"argv_digest": sha([str(x) for x in argv])}
        old = j.setdefault("transports", {}).get(stage)
        if old:
            require(old["argv_digest"] == intent["argv_digest"], "intent_changed", "Restore the original transport and journal.")
            if "reply" in old:
                return old["reply"]
        else:
            j["transports"][stage] = intent
            self.save()
        # Rust owns the one-shot dispatch grant and read-only recovery. Re-enter
        # only with these same IDs and the same durable Rust state directory.
        reply = self.cli(a["label"] + ":" + stage, argv)
        j["transports"][stage]["reply"] = reply
        self.save()
        return reply

    def publish(self):
        require("github" in self.c, "config", "Explicit operator GitHub publication configuration is required.")
        require(all("accepted" in j and not j.get("blocked") for j in self.state["agents"].values()),
                "capture_required", "Both immutable changes must be accepted before publication.")
        verify_selected_binary(self.c, self.c["binary_sha256"])
        g = self.c["github"]
        for a in self.c["agents"]:
            j = self.state["agents"][a["label"]]
            ids, text = j["github"], g["pulls"][a["label"]]
            projection = {"action": "projection_request", "key": ids["projection"], "epoch": self.c["epoch"],
                          "projection": ids["projection"], "binding": g["binding"],
                          "specification": {"changeset": a["changeset"], "version": 1,
                                            "title": text["title"], "ttl_seconds": g["ttl_seconds"]}}
            r = self.owner_request(a, "projection-request", projection, "github.projection.request.v1")
            require(r.get("projection") == ids["projection"] and r.get("changeset") == a["changeset"]
                    and r.get("binding") == g["binding"] and r.get("epoch") == self.c["epoch"],
                    "identity", "Projection receipt identifies another change or binding.")
            key = "prepare-" + ids["projection"]
            argv = ["github", "prepare", "--socket", self.c["socket"], "--credential-file", g["owner_credential"],
                    "--epoch", self.c["epoch"], "--projection", ids["projection"], "--key", key]
            ready = self.mutation(a, "projection-prepare", key, argv, "github.projection.prepare.v1",
                                  credential=g["owner_credential"], direct=True)
            require(ready.get("projection") == ids["projection"] and ready.get("state") == "ready"
                    and ready.get("changeset") == a["changeset"] and ready.get("change_version") == 1
                    and ready.get("run") == a["run"] and ready.get("snapshot") == j["accepted"]["result_snapshot"]
                    and digest(ready.get("revision"), True), "identity", "Prepared projection differs from the accepted change.")
            publication = {"action": "publication_request", "key": ids["publication"], "epoch": self.c["epoch"],
                           "publication": ids["publication"], "specification": {"projection": ids["projection"],
                           "expected_head": None, "ttl_seconds": g["ttl_seconds"]}}
            r = self.owner_request(a, "publication-request", publication, "github.publication.request.v1")
            require(r.get("publication") == ids["publication"] and r.get("projection") == ids["projection"]
                    and r.get("native_revision") == ready["revision"], "identity", "Publication identifies another projection.")
            branch_dir = self.root / (a["label"] + "-branch")
            private_directory(branch_dir, True)
            common = ["--socket", self.c["socket"], "--credential-file", g["publisher_credential"],
                      "--config", g["app_config"], "--epoch", self.c["epoch"]]
            observed = self.transport(a, "publish-branch", ["github", "publish", *common,
                                     "--publication", ids["publication"], "--state", branch_dir, "--git", g["git"]])
            require(observed.get("state") == "observed" and observed.get("publication") == ids["publication"]
                    and observed.get("projection") == ids["projection"] and observed.get("native_revision") == ready["revision"]
                    and observed.get("merge_authorized") is False, "identity", "The exact proposed branch was not observed.")
            pull = {"action": "pull_request", "key": ids["pull"], "epoch": self.c["epoch"], "pull": ids["pull"],
                    "specification": {"publication": ids["publication"], "base": g["base"], **text,
                                      "previous": None, "ttl_seconds": g["ttl_seconds"]}}
            r = self.owner_request(a, "pull-request", pull, "github.pull.request.v1")
            require(r.get("pull") == ids["pull"] and r.get("publication") == ids["publication"],
                    "identity", "Pull receipt identifies another publication.")
            pull_dir = self.root / (a["label"] + "-pull")
            private_directory(pull_dir, True)
            delivered = self.transport(a, "publish-pull", ["github", "pull", *common,
                                      "--pull", ids["pull"], "--state", pull_dir])
            require(delivered.get("state") == "observed" and delivered.get("pull") == ids["pull"]
                    and delivered.get("publication") == ids["publication"] and delivered.get("projection") == ids["projection"]
                    and delivered.get("native_revision") == ready["revision"] and delivered.get("merge_authorized") is False,
                    "identity", "The exact proposed PR was not observed.")
            identity = delivered.get("observation", {}).get("pull", {}).get("identity", {})
            require(integer(identity.get("number"), 1, 2**63-1) and isinstance(identity.get("node_id"), str)
                    and identity["node_id"], "identity", "Missing observed PR identity.")
            j["published_pull"] = identity
            self.save()

    def act(self, action):
        require(action in ACTIONS, "action", "Unknown pair phase.")
        with Journal(self.root, action == "initialize"):
            path = self.root / "pair.json"
            if path.exists() or path.is_symlink():
                self.state = read_private(path)
                require(self.state.get("version") == 1 and self.state.get("config_digest") == sha(self.c),
                        "config_changed", "Restore the original pair config; do not replace operation IDs.")
            else:
                require(action == "initialize", "initialize_required", "Initialize this private pair journal first.")
                self.state = {"version": 1, "config_digest": sha(self.c), "agents": {a["label"]: {"operations": {}} for a in self.c["agents"]}}
                if "github" in self.c:
                    for j in self.state["agents"].values():
                        j["github"] = {k: str(uuid.uuid4()) for k in ("projection", "publication", "pull")}
                self.save()
            if action == "request":
                self.request()
            elif action == "execute":
                self.execute()
            elif action == "capture":
                self.capture()
            elif action == "publish":
                self.publish()
            elif action == "status":
                for a in self.c["agents"]:
                    self.state["agents"][a["label"]]["last_status"] = self.status(a)
                self.save()
            return {"version": 1, "action": action, "scope": "prepared Connected agent pair only", "agents": {
                a["label"]: {"run": a["run"], "changeset": a["changeset"], "accepted": "accepted" in self.state["agents"][a["label"]],
                             "execution_issued": bool(self.state["agents"][a["label"]].get("execution_issued")),
                             "observed_state": self.state["agents"][a["label"]].get("last_status", self.state["agents"][a["label"]].get("terminal", {})).get("state"),
                             "published_pull": self.state["agents"][a["label"]].get("published_pull")} for a in self.c["agents"]},
                "admission_verified": False, "merge_verified": False, "release_approved": False}


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--config", type=Path, required=True)
    parser.add_argument("action", choices=ACTIONS)
    args = parser.parse_args(argv)
    try:
        print(json.dumps(Pair(read_private(args.config, 65536)).act(args.action), sort_keys=True))
        return 0
    except Error as e:
        print(json.dumps({"error": e.code, "message": str(e)}), file=sys.stderr)
    except KeyboardInterrupt:
        print(json.dumps({"error": "interrupted", "message": "Preserve pair/node journals; interruption does not prove runtime removal."}), file=sys.stderr)
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        print(json.dumps({"error": "unknown", "message": "Invalid input or unknown command outcome; preserve all journals."}), file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
