#!/usr/bin/env python3
"""Exercise the host-acceptance workload locally; this is not live-host evidence."""

import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent.parent
KIT = ROOT / "examples" / "github-queue"


def main():
    specification = json.loads((KIT / "scenarios.json").read_text())
    assert specification["version"] == 1
    with tempfile.TemporaryDirectory(prefix="treeharbor-queue-fixture-") as temporary:
        root = Path(temporary)
        repository = root / "repository"
        shutil.copytree(KIT / "seed", repository, ignore=shutil.ignore_patterns("target"))
        environment = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
        environment.update(
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_TERMINAL_PROMPT="0",
            CARGO_TARGET_DIR=str(root / "target"),
            RUSTUP_TOOLCHAIN="1.99.1",
        )

        def run(*arguments, success=True):
            result = subprocess.run(
                arguments, cwd=repository, env=environment, text=True,
                stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=120,
            )
            if (result.returncode == 0) != success:
                raise AssertionError(f"unexpected exit {result.returncode}: {arguments}\n{result.stdout}")
            return result.stdout

        def edit(case):
            change = specification["cases"][case]
            path = repository / specification["file"]
            previous = path.read_text()
            assert previous.count(change["before"]) == 1, f"stale input: {case}"
            path.write_text(previous.replace(change["before"], change["after"], 1))

        def commit(message):
            run("git", "add", "--", ".")
            run("git", "commit", "-m", message)

        def check(success=True):
            output = run("cargo", "test", "--locked", "--offline", success=success)
            if success:
                assert "3 passed; 0 failed" in output, output
            else:
                assert "combined configuration exceeds budget: 2048 > 1024" in output, output
                assert "2 passed; 1 failed" in output, output

        run("git", "init", "--initial-branch=main")
        run("git", "config", "user.name", "TreeHarbor fixture")
        run("git", "config", "user.email", "fixture@example.invalid")
        commit("Synthetic baseline")
        base = run("git", "rev-parse", "HEAD").strip()
        check()
        for case in ["agent-a", "agent-b", "text-conflict"]:
            run("git", "switch", "--create", case, base)
            edit(case)
            commit(case)
            check()
        run("git", "switch", "agent-a")
        run("git", "merge", "--no-ff", "--no-edit", "agent-b")
        assert not run("git", "ls-files", "--unmerged").strip()
        failed_group = run("git", "rev-parse", "HEAD").strip()
        check(success=False)
        edit("repair")
        commit("Explicitly repair the combined configuration")
        assert run("git", "rev-parse", "HEAD").strip() != failed_group
        check()
        run("git", "switch", "text-conflict")
        run("git", "merge", "--no-ff", "--no-edit", "agent-a", success=False)
        assert "src/lib.rs" in run("git", "ls-files", "--unmerged")
        run("git", "merge", "--abort")
        print("PASS: baseline and three independent proposals")
        print("PASS: textually clean combination fails the semantic budget check")
        print("PASS: explicit repair passes on a different commit")
        print("PASS: overlapping replacement produces a Git text conflict")
        print("Local workload only; installed-App and queue acceptance remain unverified.")


if __name__ == "__main__":
    main()
