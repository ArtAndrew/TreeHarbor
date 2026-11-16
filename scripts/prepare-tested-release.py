#!/usr/bin/env python3
"""Verify a successful main CI artifact and retain its exact tested subjects."""

import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import sys
import zipfile


JOB_NAMES = {
    "aarch64": "gVisor task supervision (arm64)",
    "x86_64": "gVisor task supervision",
}
SIDECARS = {"input-observations.json", "rust-dependencies.cdx.json", "native-inputs.json", "SBOM-SHA256SUMS"}
TEST_COMMANDS = [
    "python3 scripts/test-managed-runs.py",
    "python3 scripts/test-native-checks.py --shared-node --recover-shared-worker",
]


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(path):
    with path.open("rb") as data:
        return hashlib.file_digest(data, "sha256").hexdigest()


def prepare(scratch, architecture, environment):
    repo = environment["GITHUB_REPOSITORY"]
    repo_id = int(environment["GITHUB_REPOSITORY_ID"])
    source = environment["TH_EXPECTED_SOURCE"]
    run = environment["TH_VALIDATION_RUN"]
    attempt = environment["TH_VALIDATION_ATTEMPT"]
    tag = environment["GITHUB_REF"]
    require(re.fullmatch(r"refs/tags/v[0-9]+\.[0-9]+\.[0-9]+-alpha\.[1-9][0-9]*", tag), "expected an alpha tag")
    version = tag.removeprefix("refs/tags/v")
    job_name = JOB_NAMES[architecture]
    target = architecture + "-unknown-linux-musl"

    jobs = json.loads((scratch / "main-jobs.json").read_text())
    job, = [job for job in jobs["jobs"] if job["name"] == job_name]
    require(job["status"] == "completed" and job["conclusion"] == "success"
            and job["head_sha"] == source and job["head_branch"] == "main"
            and str(job["run_id"]) == run and str(job["run_attempt"]) == attempt,
            "runtime job does not match the successful main CI attempt")
    successful_steps = {step["name"] for step in job["steps"] if step["conclusion"] == "success"}
    require({"Exercise actual managed AgentRun lifecycle",
             "Exercise actual native candidate checks and protected publication",
             "Verify the packaged control-plane image on its native architecture",
             "Retain exact archives after installed runtime acceptance"} <= successful_steps,
            "runtime job lacks successful installed tests, image verification or artifact retention")

    artifacts = json.loads((scratch / "main-artifacts.json").read_text())
    require(artifacts["total_count"] == 1, "expected one artifact for this CI attempt and architecture")
    artifact, = artifacts["artifacts"]
    artifact_name = f"treeharbor-runtime-tested-{run}-{attempt}-{architecture}-{source}"
    origin = artifact["workflow_run"]
    require(artifact["name"] == artifact_name and artifact["expired"] is False
            and str(origin["id"]) == run and origin["head_sha"] == source
            and origin["head_branch"] == "main"
            and origin["repository_id"] == origin["head_repository_id"] == repo_id,
            "artifact was not retained by this repository's main CI")
    downloaded = scratch / "main-artifact.zip"
    require(downloaded.stat().st_size == artifact["size_in_bytes"]
            and artifact["digest"] == "sha256:" + sha256(downloaded), "downloaded artifact digest differs")

    stem = f"treeharbor-{version}-{target}"
    archive_name = stem + ".tar.gz"
    image_name = stem + ".control-plane.tar.gz"
    payloads = {archive_name, image_name}
    receipt_name = "tested-runtime-artifacts.json"
    names = SIDECARS | payloads | {name + ".sha256" for name in payloads} | {receipt_name}
    package = scratch / "runtime-package"
    with zipfile.ZipFile(downloaded) as candidate:
        require(sorted(candidate.namelist()) == sorted(names), "unexpected CI artifact file set")
        receipt = json.loads(candidate.read(receipt_name))
        expected = {
            "version": 3, "scope": "installed managed/native runtime tests",
            "source_revision": source, "github_sha": source, "github_ref": "refs/heads/main",
            "event": "push", "pr_head_sha": None, "pr_merge_sha": None,
            "repository": repo, "workflow_ref": repo + "/.github/workflows/ci.yml@refs/heads/main",
            "run_id": run, "run_attempt": attempt, "job": "runtime", "matrix_job_name": job_name,
            "architecture": architecture, "target": target, "product_version": version,
            "completed_test_commands": TEST_COMMANDS,
        }
        require(all(receipt[key] == value for key, value in expected.items()), "runtime receipt identity differs")
        package.mkdir(mode=0o700)
        for name in sorted(names):
            with candidate.open(name) as original, (package / name).open("xb") as retained:
                shutil.copyfileobj(original, retained)

    for name, key in ((archive_name, "archive"), (image_name, "control_plane_image")):
        digest = sha256(package / name)
        record = {"name": name, "sha256": digest}
        if key == "control_plane_image":
            record["binary_sha256"] = receipt["binary_sha256"]["treeharbor"]
        require(receipt[key] == record and (package / (name + ".sha256")).read_text() == f"{digest}  {name}\n",
                f"{key} differs from the tested bytes")
    require(receipt["sidecar_sha256"] == {name: sha256(package / name) for name in SIDECARS},
            "sidecar differs from the runtime receipt")
    with zipfile.ZipFile(package / (stem + ".metadata.zip"), "x", compression=zipfile.ZIP_DEFLATED) as metadata:
        for name in sorted(names - payloads):
            metadata.write(package / name, arcname=name)
    print(f"Prepared {architecture} from main CI {run}/{attempt}, artifact {artifact['id']} ({artifact['digest']})")


if __name__ == "__main__":
    try:
        prepare(Path(os.environ["RUNNER_TEMP"]), sys.argv[1], os.environ)
    except (KeyError, OSError, ValueError, zipfile.BadZipFile) as error:
        sys.exit(f"Cannot attest installed runtime artifact: {error}")
