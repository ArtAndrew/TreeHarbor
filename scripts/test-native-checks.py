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
                    changeset=self.change["changeset"], version=1)
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

    def completed_check(self, case, child, state="succeeded"):
        out, err = child.communicate(timeout=40)
        assert child.returncode == 0, (out, err)
        assert json.loads(out)["state"] == state, out
        status = self.status(case)
        assert status["state"] == state and status["runtime_removed"], status
        assert managed.docker("inspect", "treeharbor-" + case["attempt"], check=False).returncode != 0
        key = "collect-" + case["check"]
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
        self.completed_check(recovery, self.start(recovery))
        assert (recovery["files"] / "check-executions").read_bytes() == b"one check\n"
        self.native("land", self.integrator, candidate=recovery["candidate"])
        print("PASS native CI: lost start response recovers exact runtime without rerunning a check or rejecting its build outputs", flush=True)

        revoked = self.check_case()
        self.completed_check(revoked, self.start(revoked))
        self.admin("runtime-profile-revoke", *self.scope, "--profile-digest",
                   self.sql(f"SELECT 'sha256:'||encode(profile,'hex') FROM th_managed_runs WHERE run='{revoked['run']}'"))
        assert self.native("land", self.integrator, check=False, candidate=revoked["candidate"]).returncode != 0
        print("PASS native CI: revoking the execution environment invalidates pending passing evidence", flush=True)


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
