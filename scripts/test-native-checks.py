#!/usr/bin/env python3
"""Actual native proposal -> approved offline check -> protected publication.

Uses the existing disposable gVisor runtime harness; never injects source content,
exit status or validation outcomes through SQL. SQL is read-only except for dead
worker lease expiry. Native paths run with no Git/executables on PATH.
"""
import importlib.util
import json
from hashlib import sha256
from pathlib import Path
import subprocess
import socket
import uuid

spec = importlib.util.spec_from_file_location("managed", Path(__file__).with_name("test-managed-runs.py"))
managed = importlib.util.module_from_spec(spec)
spec.loader.exec_module(managed)


class Checks(managed.Harness):
    def native(self, action, credential, key=None, check=True, **fields):
        request = dict(action=action, **fields)
        if action not in ("check_status", "candidate_status", "queue_status", "ref_resolve"):
            request.update(key=key or str(uuid.uuid4()), epoch=1)
        path = self.root / ("native-" + str(uuid.uuid4()) + ".json")
        managed.private_json(path, request)
        return self.client("native", "--request", path, credential=credential, check=check)

    def build_candidate(self, program):
        identities = self.native("program_put", self.policy_admin, program=program)
        policy = {"version": 1, "checks": [dict(name="tests", validators=["ci"], **identities)],
                  "reviewers": ["reviewer"], "approvals": 1}
        policy_id = self.native("policy_put", self.policy_admin, policy=policy)["policy"]
        target = "main-" + str(uuid.uuid4())
        self.native("ref_create", self.policy_admin, name=target, head=self.revision, policy=policy_id)
        entry, candidate, check = [str(uuid.uuid4()) for _ in range(3)]
        self.native("enqueue", self.agents[0], entry=entry, target=target,
                    changeset=self.change["changeset"], version=self.change.get("version", 1))
        self.native("candidate_start", self.integrator, entry=entry, candidate=candidate)
        result = self.client("native-prepare", "--key", "prepare-" + candidate, "--epoch", "1",
                             "--candidate", candidate, credential=self.integrator)
        assert result["state"] == "checking"
        self.native("check_assign", self.integrator, candidate=candidate, name="tests", run=check, validator="ci")
        self.native("review", self.reviewer, candidate=candidate, review=str(uuid.uuid4()), decision="approve")
        return dict(candidate=candidate, check=check, entry=entry, target=target, policy=policy_id, candidate_snapshot=result["snapshot"])

    def check_case(self, expected="candidate\n", delay=0, tamper=False):
        number, node, slot, mount, profile = self.placement_profile(timeout=15000)
        program = {"version": 1, "runtime_profile": profile,
                   "argv": ["/probe", "check", expected, str(delay)]}
        case = self.build_candidate(program)
        key = "check-prepare-" + case["check"]
        first = self.native("check_prepare", self.ci, key=key, run=case["check"])
        assert first == self.native("check_prepare", self.ci, key=key, run=case["check"])
        case.update(node=node, credential=self.ci, workspace=first["workspace"], run=first["run"], mount=mount)
        # Neither validator nor agent can inject a passing receipt or choose argv.
        assert self.native("validation_report", self.ci, check=False, run=case["check"],
                           receipt=str(uuid.uuid4()), outcome="passed", evidence="sha256:" + "ab" * 32).returncode != 0
        assert self.native("check_prepare", self.agents[0], check=False, run=case["check"]).returncode != 0
        assert self.native("check_start", self.ci, check=False, run=case["check"], argv=["/probe", "exit", "0"]).returncode != 0
        prepared = self.value(*self.node_args(node, "prepare-once"), "--slot", slot)
        assert prepared["workspace"] == case["workspace"]
        case["files"] = Path(prepared["path"])
        assert (case["files"] / "same.txt").read_bytes() == b"candidate\n"
        if tamper:
            (case["files"] / "same.txt").write_text("tampered\n")
        start_key = "check-start-" + case["check"]
        result = self.native("check_start", self.ci, key=start_key, run=case["check"])
        assert result == self.native("check_start", self.ci, key=start_key, run=case["check"])
        case["attempt"] = self.sql(f"SELECT attempt FROM th_managed_runs WHERE run='{case['run']}'")
        self.cases.append(case)
        assert self.native("check_collect", self.ci, check=False, run=case["check"]).returncode != 0
        assert self.native("land", self.integrator, check=False, candidate=case["candidate"]).returncode != 0
        return case

    def lose_collection_response(self, case, key):
        credential = json.loads(self.ci.read_text())
        body = json.dumps({"version": 1, "tenant": credential["tenant"],
                           "repository": credential["repository"], "command": {
                               "method": "native", "request": {"action": "check_collect",
                               "key": key, "epoch": 1, "run": case["check"]}}}).encode()
        response = socket.socket(socket.AF_UNIX)
        response.settimeout(20)
        response.connect(str(self.socket))
        header = ("POST /v1/commands HTTP/1.1\r\nHost: treeharbor.local\r\n"
                  "Content-Type: application/json\r\nAuthorization: Bearer " + credential["token"]
                  + "\r\nContent-Length: " + str(len(body)) + "\r\nConnection: close\r\n\r\n").encode()
        response.sendall(header + body)
        managed.wait_until(lambda: self.sql(f"SELECT count(*) FROM th_check_execution_evidence WHERE check_run='{case['check']}'") == "1")
        # Kill after durable collection, before this client reads any reply.
        self.server.kill()
        self.server.wait(timeout=10)
        response.close()
        log = (self.root / "server-restart.log").open("w")
        self.server = subprocess.Popen([str(self.binary), "serve", "--config", str(self.config)],
                                       env=self.env, stdout=log, stderr=subprocess.STDOUT)
        self.children.append(self.server)
        managed.wait_until(lambda: self.client("repository-info", credential=self.ci, check=False).returncode == 0)

    def completed_check(self, case, child, state="succeeded", lose_reply=False):
        out, err = child.communicate(timeout=40)
        assert child.returncode == 0, (out, err)
        assert json.loads(out)["state"] == state, out
        status = self.status(case)
        assert status["state"] == state and status["runtime_removed"], status
        assert managed.docker("inspect", "treeharbor-" + case["attempt"], check=False).returncode != 0
        key = "collect-" + case["check"]
        if lose_reply:
            self.lose_collection_response(case, key)
        result = self.native("check_collect", self.ci, key=key, run=case["check"])
        assert result == self.native("check_collect", self.ci, key=key, run=case["check"])
        assert result == self.native("check_collect", self.ci, run=case["check"])
        assert result == self.native("check_status", self.ci, run=case["check"])
        assert result["state"] == "collected"
        evidence = json.loads(self.sql(f"SELECT jsonb_build_object('hex',encode(e.definition,'hex'),'digest',encode(e.digest,'hex'),'outcome',v.outcome) FROM th_check_execution_evidence e JOIN th_native_validations v USING(tenant,repository,receipt) WHERE e.check_run='{case['check']}'"))
        raw = bytes.fromhex(evidence["hex"])
        assert sha256(b"treeharbor-check-execution-evidence\0v1\0" + raw).hexdigest() == evidence["digest"]
        data = json.loads(raw)
        assert data[3:5] == [case["check"], case["candidate"]]
        assert data[9:11] == [case["workspace"], case["run"]]
        assert data[12] == case["candidate_snapshot"]
        assert evidence["outcome"] == {"succeeded": "passed", "failed": "failed", "lost": "error", "cancelled": "cancelled"}[state]
        return result

    def exercise(self):
        self.policy_admin = self.principal("policy-admin", "human", ["read", "repository.write", "policy.write"])
        self.reviewer = self.principal("reviewer", "human", ["read", "review"])
        self.integrator = self.principal("integrator", "integrator", ["read", "integration.control"])
        self.ci = self.principal("ci", "validator", ["read", "validation.report"])
        agent = self.case("isolation", "candidate\n", self.secret)
        self.finish(agent, self.start(agent))
        self.change = self.propose(agent)
        passed = self.check_case()
        self.completed_check(passed, self.start(passed))
        assert (passed["files"] / "check-executions").read_bytes() == b"one check\n"
        landed = self.native("land", self.integrator, candidate=passed["candidate"])
        assert landed["state"] == "landed"
        assert self.native("ref_resolve", self.policy_admin, name=passed["target"])["head"] == landed["revision"]
        print("PASS native CI: actual candidate files, immutable offline program, protected collector and publication", flush=True)

        failed = self.check_case(expected="different")
        self.completed_check(failed, self.start(failed), "failed")
        assert self.native("land", self.integrator, check=False, candidate=failed["candidate"]).returncode != 0
        changed = self.check_case(tamper=True)
        self.completed_check(changed, self.start(changed), "lost")
        assert not (changed["files"] / "check-executions").exists()
        assert self.native("land", self.integrator, check=False, candidate=changed["candidate"]).returncode != 0
        print("PASS native CI: real failing check and changed preparation cannot become passing validation", flush=True)

        for reason in ("rerun", "policy", "cancel"):
            case = self.check_case(delay=5000)
            child = self.start(case)
            managed.wait_until(lambda: (case["files"] / "check-executions").exists())
            if reason == "rerun":
                self.native("check_assign", self.integrator, candidate=case["candidate"], name="tests", run=str(uuid.uuid4()), validator="ci")
            elif reason == "policy":
                self.native("ref_policy", self.policy_admin, name=case["target"], generation=1, policy=case["policy"])
            else:
                self.native("cancel", self.agents[0], entry=case["entry"])
            self.completed_check(case, child, "cancelled")
            assert self.native("land", self.integrator, check=False, candidate=case["candidate"]).returncode != 0
        print("PASS native CI: rerun, policy change and cancellation stop actual checks and fence publication", flush=True)

        recovery = self.check_case(delay=2000)
        self.fault(recovery, "before-start")
        self.completed_check(recovery, self.start(recovery), lose_reply=True)
        assert (recovery["files"] / "check-executions").read_bytes() == b"one check\n"
        self.native("land", self.integrator, candidate=recovery["candidate"])
        print("PASS native CI: lost start/collection replies and killed server recover exact runtime/receipt without repeating checks", flush=True)

        untrusted = self.check_case()
        self.completed_check(untrusted, self.start(untrusted))
        self.admin("access-set", *self.scope, "--principal", "node-0", "--revoke-all")
        assert self.native("land", self.integrator, check=False, candidate=untrusted["candidate"]).returncode != 0
        self.admin("access-set", *self.scope, "--principal", "node-0", "--permission", "runtime.control")
        print("PASS native CI: revoked node access blocks previously passing evidence", flush=True)

        revoked = self.check_case()
        self.completed_check(revoked, self.start(revoked))
        self.admin("runtime-profile-revoke", *self.scope, "--profile-digest",
                   self.sql(f"SELECT 'sha256:'||encode(profile,'hex') FROM th_managed_runs WHERE run='{revoked['run']}'"))
        assert self.native("land", self.integrator, check=False, candidate=revoked["candidate"]).returncode != 0
        print("PASS native CI: revoking the execution environment invalidates pending passing evidence", flush=True)

        # Rebase is an owner operation, not approval. It supersedes an actively
        # checked old proposal; the runtime fence must tear that check down.
        old = self.check_case(delay=5000)
        child = self.start(old)
        managed.wait_until(lambda: (old["files"] / "check-executions").exists())
        rebase = str(uuid.uuid4())
        self.client("rebase-start", "--key", "start-" + rebase, "--epoch", "1",
                    "--rebase", rebase, "--changeset", self.change["changeset"],
                    "--source-version", "1", "--onto", landed["revision"], credential=self.agents[0])
        rebased = self.client("rebase-prepare", "--key", "prepare-" + rebase, "--epoch", "1",
                              "--rebase", rebase, credential=self.agents[0])
        assert rebased["state"] == "ready" and rebased["result_version"] == 2
        self.completed_check(old, child, "cancelled")
        assert self.native("land", self.integrator, check=False, candidate=old["candidate"]).returncode != 0
        self.change["version"] = 2
        self.revision = landed["revision"]
        fresh = self.check_case()
        assert self.native("land", self.integrator, check=False, candidate=fresh["candidate"]).returncode != 0
        self.completed_check(fresh, self.start(fresh))
        assert self.native("land", self.integrator, candidate=fresh["candidate"])["state"] == "landed"
        print("PASS native CI: owner rebase cancels a running old check and new version requires actual fresh check/review", flush=True)


if __name__ == "__main__":
    h = Checks()
    try:
        h.setup()
        h.exercise()
    except Exception:
        h.diagnostics()
        raise
    finally:
        h.cleanup()
