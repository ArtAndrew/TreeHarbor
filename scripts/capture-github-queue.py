#!/usr/bin/env python3
"""Read-only operator diagnostic; never submits evidence to TreeHarbor or GitHub."""

import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import time


def timestamp():
    return datetime.now(timezone.utc).isoformat()


def pagination_gaps(value, path="response"):
    gaps = []
    if isinstance(value, dict):
        if value.get("pageInfo", {}).get("hasNextPage"):
            gaps.append(path)
        for key, child in value.items():
            gaps.extend(pagination_gaps(child, f"{path}.{key}"))
    elif isinstance(value, list):
        for index, child in enumerate(value):
            gaps.extend(pagination_gaps(child, f"{path}[{index}]"))
    return gaps


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", required=True, help="explicit OWNER/NAME")
    parser.add_argument("--pull", type=int, required=True)
    parser.add_argument("--base-ref", default="refs/heads/main")
    parser.add_argument("--output", type=Path, required=True, help="new private directory")
    parser.add_argument("--gh", default="gh", help="path to the operator's authenticated GitHub CLI")
    parser.add_argument("--samples", type=int, default=1)
    parser.add_argument("--interval", type=int, default=5, help="seconds between completed samples")
    args = parser.parse_args()
    if not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,99}/[A-Za-z0-9][A-Za-z0-9_.-]{0,99}", args.repository):
        parser.error("expected OWNER/NAME")
    if not 1 <= args.pull <= 2**31 - 1 or not 1 <= args.samples <= 120 or not 2 <= args.interval <= 60:
        parser.error("pull, samples or interval is outside its supported bound")
    if not args.base_ref.startswith("refs/heads/") or len(args.base_ref) > 1024 or any(ord(c) < 32 for c in args.base_ref):
        parser.error("expected a bounded full refs/heads/ base ref")
    root = Path(__file__).resolve().parent.parent
    query = (root / "examples/github-queue/queue-observation.graphql").read_text()
    owner, name = args.repository.split("/")
    request = json.dumps({"query": query, "variables": {
        "owner": owner, "name": name, "number": args.pull, "baseRef": args.base_ref,
    }})
    # New directory only: a retry cannot overwrite earlier evidence. umask is set
    # before creation because the results can identify a private repository.
    os.umask(0o077)
    args.output.mkdir(mode=0o700, parents=False, exist_ok=False)
    environment = os.environ.copy()
    environment.pop("GH_DEBUG", None)
    environment.pop("DEBUG", None)
    environment["GH_PROMPT_DISABLED"] = "1"
    command = [args.gh, "api", "--hostname", "github.com", "graphql", "--input", "-",
               "-H", "X-GitHub-Api-Version: 2026-03-10"]
    failed = False
    for index in range(args.samples):
        started = timestamp()
        try:
            result = subprocess.run(command, input=request, env=environment, text=True,
                                    capture_output=True, timeout=40)
        except subprocess.TimeoutExpired:
            response, error = None, "transport_timeout"
        else:
            if len(result.stdout.encode()) > 2 * 1024 * 1024:
                response, error = None, "response_too_large"
            else:
                try:
                    response = json.loads(result.stdout)
                except json.JSONDecodeError:
                    response, error = None, "non_json_response"
                else:
                    error = "provider_error" if result.returncode or not isinstance(response, dict) or response.get("errors") else None
        gaps = pagination_gaps(response)
        if gaps and error is None:
            error = "truncated_connections"
        record = {
            "version": 1, "diagnostic_only": True, "merge_verified": False,
            "api_version": "2026-03-10", "query_sha256": hashlib.sha256(query.encode()).hexdigest(),
            "started_at": started, "finished_at": timestamp(),
            "repository": args.repository, "pull": args.pull, "base_ref": args.base_ref,
            "error": error, "pagination_gaps": gaps, "response": response,
        }
        path = args.output / f"snapshot-{index + 1:04}.json"
        # Exclusive creation; partial final file after process loss is not success.
        with path.open("x") as file:
            json.dump(record, file, indent=2)
            file.write("\n")
            file.flush()
            os.fsync(file.fileno())
        failed |= error is not None
        print(f"sample {index + 1}: {error or 'captured; merge remains unverified'}", flush=True)
        if index + 1 < args.samples:
            time.sleep(args.interval)
    return 1 if failed else 0


if __name__ == "__main__":
    raise SystemExit(main())
