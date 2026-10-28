#!/usr/bin/env python3
"""Controlled fake-runner tests, not GitHub/gVisor/Connected acceptance."""
from copy import deepcopy
import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import threading
import time
from types import SimpleNamespace
import unittest
from unittest.mock import patch
import uuid

SPEC = importlib.util.spec_from_file_location("reference_pair", Path(__file__).resolve().parents[1] / "examples/connected-pr/agent-pair.py")
pair = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(pair)


def uid(n):
    return str(uuid.UUID(int=n))


class Fake:
    def __init__(self, c):
        self.c = c
        self.jobs = {a["label"]: {"run": None, "seal": None} for a in c["agents"]}
        self.ops, self.calls = {}, []
        self.counts = {k: {x: 0 for x in ("request", "execute", "seal", "capture", "accept")} for k in self.jobs}
        self.barrier, self.lock = threading.Barrier(2), threading.Lock()
        self.active = self.maximum = 0
        self.fault = None
        self.status_wrong = False
        self.base_wrong = False
        self.prepared_wrong = False
        self.removed_false = False

    def seal(self, a):
        ready = self.jobs[a["label"]]["seal"] == "ready"
        return {"seal": a["seal"], "run": a["run"], "generation": a["generation"], "epoch": self.c["epoch"],
                "state": "ready" if ready else "waiting", "unexpired": True, "failure": None,
                "manifest_digest": "d" * 64 if ready else None, "operations": "sha256:" + "e" * 64 if ready else None}

    def __call__(self, argv, label):
        who, action = label.split(":")
        a = next(v for v in self.c["agents"] if v["label"] == who)
        j = self.jobs[who]
        with self.lock:
            self.calls.append((label, list(argv)))
        if action == "workspace-status":
            return {"workspace": a["workspace"], "base": "native:sha256:" + "9" * 64 if self.base_wrong else self.c["baseline"]["revision"],
                    "state": "ready", "generation": a["generation"], "epoch": self.c["epoch"], "format": "native", "profile_resolved": True}
        if action == "prepared":
            return {"workspace": a["workspace"], "state": "files_ready", "runtime": None,
                    "snapshot": "sha256:" + "9" * 64 if self.prepared_wrong else self.c["baseline"]["snapshot"], **a["prepared"]}
        if action == "run-status":
            if j["run"] is None:
                raise pair.Error("command_failed", "Fixture not found.")
            terminal = j["run"] in pair.TERMINAL
            return {"run": uid(999) if self.status_wrong else a["run"], "workspace": a["workspace"], "generation": a["generation"],
                    "epoch": self.c["epoch"], "state": j["run"], "runtime_removed": terminal and not self.removed_false,
                    "stop_requested": False, "exit_code": 0 if terminal else None, "oom_killed": False if terminal else None}
        if action == "operation-status":
            key = argv[argv.index("--key") + 1]
            if key not in self.ops:
                raise pair.Error("command_unknown", "Fixture unknown operation.")
            kind, result = self.ops[key]
            return {"key": key, "state": "committed", "kind": kind, "result": deepcopy(result)}
        if action in ("request", "seal", "accept"):
            saved = json.loads((Path(self.c["state"]) / "pair.json").read_text())
            self.assertion(saved["agents"][who]["operations"][action]["key"] == a["keys"][action])
            if self.fault == (who, "before-request"):
                raise pair.Error("command_unknown", "Fixture stopped before send.")
            self.counts[who][action] += 1
            if action == "request":
                self.assertion("connected" in argv and argv[-len(a["argv"]):] == a["argv"])
                j["run"] = "requested"
                result = {"run": uid(999) if self.fault == (who, "wrong-request") else a["run"], "workspace": a["workspace"], "state": "requested"}
                kind = "run.request.v1"
            elif action == "seal":
                self.assertion(argv[argv.index("--replace") + 1] == "src/lib.rs")
                j["seal"] = "waiting"
                result, kind = self.seal(a), "workspace.seal.request.v1"
            else:
                result = {"state": "accepted", "changeset": a["changeset"], "version": 1, "run": a["run"], "seal": a["seal"],
                          "base_revision": self.c["baseline"]["revision"], "base_snapshot": self.c["baseline"]["snapshot"],
                          "operations": "sha256:" + "e" * 64, "result_snapshot": "sha256:" + ("7" if who == "A" else "8") * 64,
                          "manifest_digest": "d" * 64}
                kind = "workspace.seal.accept.v1"
            self.ops[a["keys"][action]] = kind, deepcopy(result)
            if self.fault == (who, action):
                self.fault = None
                raise pair.Error("command_unknown", "Fixture committed reply lost.")
            return result
        if action == "execute":
            self.assertion(argv[argv.index("--run") + 1] == a["run"])
            saved = json.loads((Path(self.c["state"]) / "pair.json").read_text())
            self.assertion(all(v.get("execution_issued") for v in saved["agents"].values()))
            with self.lock:
                self.counts[who][action] += 1
                self.active += 1
                self.maximum = max(self.maximum, self.active)
                j["run"] = "running"
            self.barrier.wait(timeout=3)  # Proves both commands overlap; serial code fails.
            time.sleep(0.01)
            with self.lock:
                self.active -= 1
                j["run"] = "requested" if self.fault == (who, "unknown-execute") else "succeeded"
            if self.fault in ((who, "execute"), (who, "unknown-execute")):
                raise pair.Error("command_unknown", "Fixture worker response lost.")
            return {"run": uid(999) if self.fault == (who, "wrong-execute") else a["run"], "state": "succeeded", "runtime_removed": True}
        if action == "seal-status":
            return self.seal(a)
        if action == "capture":
            self.assertion(argv[argv.index("--seal") + 1] == a["seal"])
            self.counts[who][action] += 1
            j["seal"] = "ready"
            if self.fault == (who, "capture"):
                self.fault = None
                raise pair.Error("command_unknown", "Fixture ready response lost.")
            result = self.seal(a)
            if self.fault == (who, "wrong-capture"):
                result["seal"] = uid(999)
            return result
        raise AssertionError("Unexpected fixture command")

    @staticmethod
    def assertion(ok):
        if not ok:
            raise AssertionError("Fixture command/WAL mismatch")


class PairTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="pair-stage-synthetic-")
        self.addCleanup(self.temp.cleanup)
        root = Path(self.temp.name)
        binary = root / "never-executed-fixture"
        binary.write_bytes(b"fixture bytes, never a runtime")
        binary.chmod(0o700)
        self.c = {"version": 1, "profile": "connected-prepared-agent-pair-v1", "tenant": "fixture", "repository": uid(100), "epoch": 1,
                  "binary": str(binary), "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                  "socket": str(root / "command.sock"), "service_config": str(root / "service.json"), "state": str(root / "pair"),
                  "engine_socket": str(root / "engine.sock"), "baseline": {"revision": "native:sha256:" + "1" * 64, "snapshot": "sha256:" + "2" * 64},
                  "wait": {"attempts": 2, "interval_seconds": 1}, "agents": []}
        for i, label in enumerate(("A", "B")):
            self.c["agents"].append({"label": label, "workspace": uid(1+i*4), "run": uid(2+i*4), "seal": uid(3+i*4), "changeset": uid(4+i*4),
                "generation": 1, "credential": str(root / (label + "-agent.json")), "runtime_profile": "sha256:" + "3" * 64,
                "argv": ["/agent", "memory" if label == "A" else "workers"],
                "node": {"root": str(root / ("node-" + label)), "storage": uid(20+i), "slot": uid(30+i), "credential": str(root / (label + "-node.json"))},
                "prepared": {"path": str(root / label / "files"), "manifest": str(root / label / "manifest.json")},
                "selection": {"replace": ["src/lib.rs"], "delete": [], "exclude": []},
                "keys": {k: "pair-" + label + "-" + k for k in ("request", "seal", "accept")}})
        self.c["capture_worker"] = {"version": 1, "mode": "selected-seal-v1", "verified_binary_sha256": self.c["binary_sha256"]}
        self.fake = Fake(self.c)
        self.act("initialize")

    def act(self, action, config=None):
        return pair.Pair(config or self.c, runner=self.fake, sleeper=lambda _: None).act(action)

    def running_pair(self):
        self.act("request")
        self.act("execute")

    def test_actual_metadata_envelope_keeps_nested_operation_receipt_and_raw_node_reply(self):
        receipt = {"run": self.c["agents"][0]["run"], "state": "requested"}
        operation = {"key": "original-key", "state": "committed", "kind": "run.request.v1", "result": receipt}
        for wire, expected in (({"version": 1, "result": operation}, operation),
                               ({"run": receipt["run"], "state": "succeeded", "runtime_removed": True},
                                {"run": receipt["run"], "state": "succeeded", "runtime_removed": True})):
            process = SimpleNamespace(returncode=0, stdout=json.dumps(wire).encode())
            with patch.object(pair.subprocess, "run", return_value=process) as run:
                value = pair.SubprocessRunner()(["never-executed-fixture"], "envelope-fixture")
            self.assertEqual(value, expected)
            self.assertEqual(run.call_args.kwargs["timeout"], 420)

    def test_real_concurrent_dispatch_wal_exact_selectors_and_immutable_capture(self):
        self.running_pair()
        self.assertEqual(self.fake.maximum, 2)
        self.act("capture")
        result = self.act("capture")
        self.assertTrue(all(v["accepted"] for v in result["agents"].values()))
        self.assertIs(result["merge_verified"], False)
        self.assertIs(result["admission_verified"], False)
        for counts in self.fake.counts.values():
            self.assertEqual(counts, {k: 1 for k in counts})
        self.act("execute")
        self.assertTrue(all(v["execute"] == 1 for v in self.fake.counts.values()))
        self.assertFalse(any(Path(a["credential"]).exists() for a in self.c["agents"]))

    def test_lost_committed_mutation_replies_recover_original_key_without_duplicate(self):
        self.fake.fault = ("A", "request")
        with self.assertRaises(pair.Error):
            self.act("request")
        self.act("request")
        self.assertEqual(self.fake.counts["A"]["request"], 1)
        self.act("execute")
        for fault in ("seal", "accept"):
            self.fake.fault = ("A", fault)
            with self.assertRaises(pair.Error):
                self.act("capture")
        self.act("capture")
        self.assertTrue(all(c["seal"] == c["accept"] == 1 for c in self.fake.counts.values()))

    def test_written_intent_without_committed_operation_stops_without_resending(self):
        self.fake.fault = ("A", "before-request")
        for _ in range(2):
            with self.assertRaises(pair.Error):
                self.act("request")
        self.assertEqual(sum(label == "A:request" for label, _ in self.fake.calls), 1)
        self.assertEqual(self.fake.ops, {})

    def test_wrong_request_receipt_cannot_be_bypassed_by_execute(self):
        self.fake.fault = ("A", "wrong-request")
        with self.assertRaises(pair.Error):
            self.act("request")
        with self.assertRaises(pair.Error):
            self.act("execute")
        self.assertTrue(all(v["execute"] == 0 for v in self.fake.counts.values()))

    def test_terminal_state_without_actual_removal_never_requests_capture(self):
        self.act("request")
        self.fake.removed_false = True
        with self.assertRaises(pair.Error):
            self.act("execute")
        with self.assertRaises(pair.Error):
            self.act("capture")
        self.assertTrue(all(v["seal"] == 0 for v in self.fake.counts.values()))

    def test_lost_terminal_worker_reply_is_status_only_on_resume(self):
        self.act("request")
        self.fake.fault = ("A", "execute")
        self.act("execute")
        self.act("execute")
        self.assertEqual(self.fake.counts["A"]["execute"], 1)
        self.assertTrue(self.act("capture")["agents"]["A"]["accepted"])

    def test_unknown_execution_wait_is_bounded_and_never_restarts_requested_job(self):
        self.act("request")
        self.fake.fault = ("A", "unknown-execute")
        for _ in range(2):
            with self.assertRaisesRegex(pair.Error, "Bounded"):
                self.act("execute")
        self.assertEqual(self.fake.counts["A"]["execute"], 1)
        with self.assertRaises(pair.Error):
            self.act("capture")
        self.assertEqual(self.fake.counts["A"]["seal"], 0)

    def test_mismatching_worker_response_blocks_capture_even_if_status_is_terminal(self):
        self.act("request")
        self.fake.fault = ("A", "wrong-execute")
        with self.assertRaises(pair.Error):
            self.act("execute")
        with self.assertRaises(pair.Error):
            self.act("capture")
        self.assertEqual(self.fake.counts["A"]["seal"], 0)

    def test_no_legacy_unqualified_capture_and_ready_status_recovery(self):
        # Use a separate immutable config/journal for the default no-selector path.
        c = deepcopy(self.c)
        c.pop("capture_worker")
        c["state"] += "-default"
        self.c = c
        self.fake = Fake(c)
        self.act("initialize")
        self.running_pair()
        with self.assertRaisesRegex(pair.Error, "No unqualified"):
            self.act("capture")
        self.assertEqual(self.fake.counts["A"]["capture"], 0)
        self.fake.jobs["A"]["seal"] = self.fake.jobs["B"]["seal"] = "ready"
        # B request is not yet issued; request creates waiting again. Complete only
        # A's saved ready identity, then stop rather than assume an empty queue.
        with self.assertRaises(pair.Error):
            self.act("capture")
        self.assertEqual(self.fake.counts["A"]["accept"], 1)
        self.assertTrue(all(v["capture"] == 0 for v in self.fake.counts.values()))

    def test_lost_ready_capture_reply_uses_status_without_another_worker(self):
        self.running_pair()
        self.fake.fault = ("A", "capture")
        with self.assertRaises(pair.Error):
            self.act("capture")
        self.act("capture")
        self.assertEqual(self.fake.counts["A"]["capture"], 1)

    def test_wrong_ready_seal_never_gets_accepted(self):
        self.running_pair()
        self.fake.fault = ("A", "wrong-capture")
        for _ in range(2):
            with self.assertRaises(pair.Error):
                self.act("capture")
        self.assertEqual(self.fake.counts["A"]["accept"], 0)

    def test_baseline_preparation_and_status_mismatches_stop_before_execution(self):
        self.fake.base_wrong = True
        with self.assertRaises(pair.Error):
            self.act("request")
        self.fake.base_wrong, self.fake.prepared_wrong = False, True
        with self.assertRaises(pair.Error):
            self.act("request")
        self.fake.prepared_wrong = False
        self.act("request")
        self.fake.status_wrong = True
        with self.assertRaises(pair.Error):
            self.act("execute")
        self.assertTrue(all(v["execute"] == 0 for v in self.fake.counts.values()))

    def test_config_identity_nodes_and_binary_pins_cannot_be_changed_on_resume(self):
        c = deepcopy(self.c)
        c["agents"][1]["node"]["root"] = c["agents"][0]["node"]["root"]
        with self.assertRaises(pair.Error):
            pair.validate(c)
        c = deepcopy(self.c)
        c["agents"][0]["keys"]["request"] += "changed"
        with self.assertRaisesRegex(pair.Error, "original pair config"):
            self.act("request", c)
        self.act("request")
        Path(self.c["binary"]).write_bytes(b"replacement")
        with self.assertRaises(pair.Error):
            self.act("execute")
        self.assertTrue(all(v["execute"] == 0 for v in self.fake.counts.values()))


class PublishingFake(Fake):
    """Fake coordinator/transport replies; no claim of GitHub acceptance."""
    def __init__(self, config):
        super().__init__(config)
        self.remote_writes, self.host_calls = {}, []
        self.lose = None
        self.wrong_projection = False

    def __call__(self, argv, label):
        who, action = label.split(":")
        if action not in ("projection-request", "projection-prepare", "publication-request", "pull-request", "publish-branch", "publish-pull"):
            return super().__call__(argv, label)
        self.host_calls.append((label, list(argv)))
        a = next(a for a in self.c["agents"] if a["label"] == who)
        saved = json.loads((Path(self.c["state"]) / "pair.json").read_text())
        ids = saved["agents"][who]["github"]
        native = "native:sha256:" + ("a" if who == "A" else "b") * 64
        result = {"projection": ids["projection"], "changeset": a["changeset"], "binding": self.c["github"]["binding"],
                  "epoch": self.c["epoch"], "run": a["run"], "change_version": 1, "revision": native,
                  "snapshot": "sha256:" + ("7" if who == "A" else "8") * 64, "state": "requested"}
        expected_credential = self.c["github"]["publisher_credential" if action.startswith("publish-") else "owner_credential"]
        self.assertion(argv[argv.index("--credential-file")+1] == expected_credential)
        if action.startswith("publish-"):
            stage = "branch" if action == "publish-branch" else "pull"
            self.assertion(action in ids["transports"])
            state = Path(argv[argv.index("--state")+1])
            self.assertion(state.is_dir() and state.parent == Path(self.c["state"]))
            identity = (who, stage, str(state))
            self.remote_writes.setdefault(identity, 1)  # Models Rust's one-shot transport, not a real remote.
            result = {"state": "observed", "publication": ids["publication"], "projection": ids["projection"],
                      "native_revision": native, "merge_authorized": False}
            if stage == "pull":
                result.update(pull=ids["pull"], observation={"pull": {"identity": {"number": 11 if who == "A" else 12, "node_id": "fixture-"+who}}})
        else:
            if action == "projection-prepare":
                key = argv[argv.index("--key")+1];kind="github.projection.prepare.v1"
                result["state"] = "ready"
            else:
                payload = json.loads(Path(argv[argv.index("--request")+1]).read_text())
                key = payload["key"]
                kind = {"projection-request":"github.projection.request.v1", "publication-request":"github.publication.request.v1", "pull-request":"github.pull.request.v1"}[action]
                self.assertion(saved["agents"][who]["operations"][action]["key"] == key)
                if action == "projection-request":
                    self.assertion(payload["specification"]["changeset"] == a["changeset"] and payload["specification"]["version"] == 1)
                elif action == "publication-request":
                    self.assertion(payload["specification"]["projection"] == ids["projection"] and payload["specification"]["expected_head"] is None)
                    result.update(publication=ids["publication"],native_revision=native)
                else:
                    self.assertion(payload["specification"]["publication"] == ids["publication"] and payload["specification"]["previous"] is None)
                    result.update(pull=ids["pull"],publication=ids["publication"])
            self.ops[key] = kind, deepcopy(result)
        if self.wrong_projection and action == "projection-prepare":
            result["snapshot"] = "sha256:" + "0" * 64
        if self.lose == (who, action):
            self.lose = None
            raise pair.Error("command_unknown", "Fixture committed reply lost.")
        return result


class PublicationTests(unittest.TestCase):
    setUp = PairTests.setUp
    act = PairTests.act
    running_pair = PairTests.running_pair
    # Reuse setup/support without re-running all inherited preparation contracts.
    def publication_setup(self):
        c = deepcopy(self.c);c["state"] += "-published"
        root = Path(c["state"]).parent
        c["github"] = {"binding": uid(500), "owner_credential": str(root/"owner.json"),
                       "publisher_credential": str(root/"publisher.json"), "app_config": str(root/"app.json"),
                       "git": "/usr/bin/git", "base": "main", "ttl_seconds": 3600,
                       "pulls": {label: {"title": "Fixture " + label, "body": "Synthetic test", "draft": True} for label in ("A", "B")}}
        self.c=c;self.fake=PublishingFake(c);self.act("initialize")
        self.running_pair();self.act("capture")

    def test_publication_uses_operator_credentials_and_preserves_two_exact_intents(self):
        self.publication_setup()
        result=self.act("publish");calls=len(self.fake.host_calls)
        replay=self.act("publish")
        self.assertEqual(result["agents"],replay["agents"])
        self.assertEqual(len(self.fake.host_calls),calls)
        self.assertEqual([v["published_pull"]["number"] for v in result["agents"].values()],[11,12])
        self.assertEqual(len(self.fake.remote_writes),4)
        self.assertFalse(result["merge_verified"])
        self.assertFalse(any("queue" in value for _,argv in self.fake.host_calls for value in map(str,argv)))

    def test_lost_owner_and_transport_replies_keep_original_ids_and_state_directories(self):
        self.publication_setup()
        self.fake.lose=("A","projection-request")
        with self.assertRaises(pair.Error):self.act("publish")
        self.fake.lose=("A","publish-branch")
        with self.assertRaises(pair.Error):self.act("publish")
        self.fake.lose=("A","publish-pull")
        with self.assertRaises(pair.Error):self.act("publish")
        result=self.act("publish")
        self.assertTrue(all(v["published_pull"] for v in result["agents"].values()))
        self.assertEqual(sum(label=="A:projection-request" for label,_ in self.fake.host_calls),1)
        self.assertEqual(len(self.fake.remote_writes),4)
        for stage in ("publish-branch","publish-pull"):
            calls=[argv for label,argv in self.fake.host_calls if label=="A:"+stage]
            self.assertEqual(len(calls),2);self.assertEqual(calls[0],calls[1])

    def test_wrong_prepared_snapshot_never_reaches_the_host_transport(self):
        self.publication_setup();self.fake.wrong_projection=True
        with self.assertRaises(pair.Error):self.act("publish")
        self.assertFalse(self.fake.remote_writes)
        with self.assertRaises(pair.Error):self.act("publish")
        self.assertFalse(self.fake.remote_writes)

    def test_operator_inputs_and_journal_cannot_be_inside_agent_files(self):
        self.publication_setup()
        for field in ("state","service_config"):
            c=deepcopy(self.c);c[field]=str(Path(c["agents"][0]["prepared"]["path"])/"private.json")
            with self.assertRaises(pair.Error):pair.validate(c)
        c=deepcopy(self.c);c["github"]["publisher_credential"]=c["agents"][0]["credential"]
        with self.assertRaises(pair.Error):pair.validate(c)
        c=deepcopy(self.c);c["github"]["app_config"]=str(Path(c["agents"][0]["prepared"]["path"])/"app.json")
        with self.assertRaises(pair.Error):pair.validate(c)


if __name__ == "__main__":
    unittest.main(verbosity=2)
