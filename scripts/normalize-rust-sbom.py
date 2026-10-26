#!/usr/bin/env python3
"""Validate and enrich cargo-auditable/Syft Rust coverage; not a full native SBOM."""
import argparse
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import re
import stat
import tomllib

REGISTRY = "registry+https://github.com/rust-lang/crates.io-index"
COVERAGE = "cargo-auditable Rust crates only; precompiled std and native subcomponents are not covered"


def sha256(path):
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError("binary must be a regular file")
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(block)
    return h.hexdigest()


def normalize(report, metadata, lock, binaries):
    if not all(isinstance(value, dict) for value in (report, metadata, lock)):
        raise ValueError("expected JSON/TOML objects")
    if report.get("bomFormat") != "CycloneDX" or report.get("specVersion") != "1.7":
        raise ValueError("expected reviewed CycloneDX 1.7 output")
    if not binaries or len(binaries) > 8:
        raise ValueError("expected one to eight named binaries")
    expected = {}
    for name, (digest, _) in binaries.items():
        if not re.fullmatch(r"[A-Za-z0-9_-]{1,80}", name) or not re.fullmatch(r"[0-9a-f]{64}", digest):
            raise ValueError("invalid binary name or hash")
        if digest in expected:
            raise ValueError("ambiguous identical binary inputs")
        expected[digest] = name
    packages = {}
    members = set(metadata.get("workspace_members", []))
    for p in metadata.get("packages", []):
        packages.setdefault((p.get("name"), p.get("version")), []).append(p)
    doc = deepcopy(report)
    components = doc.get("components")
    if not isinstance(components, list) or not components or len(components) > 4096:
        raise ValueError("invalid component set")
    ids, files, crates = set(), set(), 0
    for c in components:
        if not isinstance(c, dict):
            raise ValueError("invalid component record")
        identity = c.get("bom-ref")
        if not isinstance(identity, str) or not identity or identity in ids:
            raise ValueError("duplicate or missing component identity")
        ids.add(identity)
        if c.get("type") == "file":
            hashes = [h.get("content") for h in c.get("hashes", []) if h.get("alg") == "SHA-256"]
            if len(hashes) != 1 or hashes[0] not in expected:
                raise ValueError("unmatched binary file hash")
            name = expected[hashes[0]]
            if name in files or Path(c.get("name", "")).name != name:
                raise ValueError("ambiguous binary file identity")
            files.add(name)
            c["name"] = "bin/" + name
            continue
        found = [p.get("value") for p in c.get("properties", []) if p.get("name") == "syft:package:foundBy"]
        if c.get("type") != "library" or found != ["cargo-auditable-binary-cataloger"]:
            raise ValueError("non-auditable component in Rust-only report")
        locations = [p.get("value") for p in c.get("properties", [])
                     if re.fullmatch(r"syft:location:[0-9]+:path", p.get("name", ""))]
        if not locations or any(location not in {"/" + name for name in binaries} for location in locations):
            raise ValueError("crate location is not one of the scanned binaries")
        matches = packages.get((c.get("name"), c.get("version")), [])
        if len(matches) != 1:
            raise ValueError("unknown or ambiguous crate declaration")
        p = matches[0]
        license_expression = p.get("license")
        if (not isinstance(license_expression, str) or not license_expression.strip()
                or len(license_expression) > 4096 or any(ord(x) < 32 for x in license_expression)):
            raise ValueError("crate has no usable declared license expression")
        # Old Cargo declarations may use slash notation. Preserve those as
        # named declarations, without inventing an SPDX interpretation.
        c["licenses"] = ([{"license": {"name": license_expression}}] if "/" in license_expression
                         else [{"expression": license_expression}])
        if p.get("source") is None and p.get("id") in members:
            pass  # Original workspace package; no invented registry checksum.
        elif p.get("source") == REGISTRY:
            records = [r for r in lock.get("package", []) if r.get("name") == p["name"]
                       and r.get("version") == p["version"] and r.get("source") == REGISTRY]
            if len(records) != 1 or not re.fullmatch(r"[0-9a-f]{64}", records[0].get("checksum", "")):
                raise ValueError("crate lacks exact locked source checksum")
            reference = {"type": "distribution",
                         "url": "https://crates.io/api/v1/crates/" + p["name"] + "/" + p["version"] + "/download",
                         "hashes": [{"alg": "SHA-256", "content": records[0]["checksum"]}]}
            refs = c.setdefault("externalReferences", [])
            if reference not in refs:
                refs.append(reference)
        else:
            raise ValueError("unsupported crate source; do not infer provenance")
        crates += 1
    if files != set(binaries) or not crates:
        raise ValueError("each binary and an embedded crate graph are required")
    root = doc.get("metadata", {}).get("component", {}).get("bom-ref")
    allowed = ids | ({root} if isinstance(root, str) else set())
    for d in doc.get("dependencies", []):
        if d.get("ref") not in allowed or any(x not in allowed for x in d.get("dependsOn", [])):
            raise ValueError("dependency refers to an unknown component")
    properties = doc.setdefault("metadata", {}).setdefault("properties", [])
    if any(p.get("name") == "treeharbor:coverage" for p in properties):
        raise ValueError("input already carries a coverage assertion")
    properties.append({"name": "treeharbor:coverage", "value": COVERAGE})
    encoded = json.dumps(doc, ensure_ascii=False)
    for _, directory in binaries.values():
        if directory and directory in encoded:
            raise ValueError("local build path remains in report")
    return doc


def read_json(path):
    if path.stat().st_size > 16 * 1024 * 1024:
        raise ValueError("JSON input exceeds limit")
    return json.loads(path.read_text())


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--report", type=Path, required=True)
    p.add_argument("--metadata", type=Path, required=True)
    p.add_argument("--lockfile", type=Path, required=True)
    p.add_argument("--binary", action="append", required=True, metavar="NAME=PATH")
    p.add_argument("--output", type=Path, required=True)
    args = p.parse_args()
    try:
        binaries = {}
        for value in args.binary:
            name, path = value.split("=", 1)
            if name in binaries:
                raise ValueError("duplicate binary argument")
            path = Path(path).absolute()
            binaries[name] = (sha256(path), str(path.parent))
        result = normalize(read_json(args.report), read_json(args.metadata),
                           tomllib.loads(args.lockfile.read_text()), binaries)
        with args.output.open("x") as output:
            json.dump(result, output, indent=2, ensure_ascii=False)
            output.write("\n")
    except (OSError, ValueError, KeyError, TypeError) as error:
        p.exit(1, f"Rust SBOM normalization failed: {error}\n")


if __name__ == "__main__":
    main()
