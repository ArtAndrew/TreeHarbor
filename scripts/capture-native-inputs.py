#!/usr/bin/env python3
"""Bind two actual executables to retained canonical linker-input observations.

All six CLI options are explicit. Private records are re-normalized with the
sibling strict wrapper; ephemeral linker inputs are never reopened. A missing
link invocation (including a cached build), stale context or ambiguous match
fails. No executable, capture or Cargo graph sidecar is rewritten.

This is an unsigned observation inventory, NOT a full component SBOM, archive
member attribution, native closure proof, reproducibility or source/build
provenance. Source/session are observed build-environment labels. Local records
remain mutable by their producer; matching labels cannot authenticate a build.

Reusable read-only API:
validate_report(report, *, expected_source, expected_target, expected_session,
                binary_sha256) -> the unchanged validated dict.
All keyword arguments are mandatory. ValidationError derives from ValueError;
diagnostics contain fixed codes, never private paths or upstream record text.
"""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import stat
import sys

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("treeharbor_link_capture", Path(__file__).with_name("capture-link-inputs.py"))
wrapper = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = wrapper
SPEC.loader.exec_module(wrapper)

NAMES = ("treeharbor", "treeharbor-runtime-init")
PROFILE = wrapper.PROFILE
CLAIMS = ("source_provenance_verified", "build_provenance_verified",
          "native_component_inventory_complete", "archive_member_attribution_complete",
          "native_link_closure_verified")
KIND = "canonical-linked-input-observations"
SCOPE = "Link-input observations, not a full component SBOM."
OBSERVATION = "Retained post-link file observations matched to actual executable bytes."
MAX_INVOCATIONS = 256
MAX_CAPTURE_BYTES = 256 * 1024 * 1024


class ValidationError(ValueError):
    pass


def require(condition, code):
    if not condition:
        raise ValidationError(code)


def encode(value):
    return (json.dumps(value, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()


def digest(data):
    return hashlib.sha256(data).hexdigest()


def keys(value, expected):
    require(isinstance(value, dict) and set(value) == set(expected), "unexpected_fields")


def hash_value(value):
    return isinstance(value, str) and wrapper.SHA.fullmatch(value) is not None


def public_file(value):
    keys(value, ("root", "path", "sha256", "bytes"))
    path = value["path"]
    require(value["root"] in wrapper.ROOT_KINDS and wrapper.path_word(path), "invalid_root_relative_path")
    parsed = PurePosixPath(path)
    require(not parsed.is_absolute() and str(parsed) == path and path != "."
            and ".." not in parsed.parts, "invalid_root_relative_path")
    require(hash_value(value["sha256"]) and type(value["bytes"]) is int
            and 0 <= value["bytes"] <= wrapper.MAX_INPUT_BYTES, "invalid_file_identity")


def output_name(path, name):
    stem = name.replace("-", "_")
    return re.fullmatch(r"(?:" + re.escape(name) + "|" + re.escape(stem)
                        + r")(?:-[0-9a-f]{16})?", PurePosixPath(path).name) is not None


def normalized_payload(report, binary):
    return {"version": 1, "profile": report["profile"], "target": report["target"],
            "build_source": report["build_source"], "build_session": report["build_session"],
            "output": binary["output"], "inputs": binary["inputs"], "tools": binary["tools"],
            "observation": "file hashes read after successful link and before wrapper return",
            **{claim: False for claim in CLAIMS[:-1]}}


def validate_report(report, *, expected_source, expected_target, expected_session, binary_sha256):
    """Strictly validate a report against independently selected context/bin hashes."""
    wrapper.build_context(expected_source, expected_session)
    require(expected_source != "0" * 40 and isinstance(expected_target, str)
            and expected_target in wrapper.TARGETS, "invalid_expected_context")
    keys(binary_sha256, NAMES)
    require(all(hash_value(v) for v in binary_sha256.values()), "invalid_binary_hashes")
    keys(report, ("version", "kind", "profile", "target", "build_source", "build_session",
                  "binary_sha256", "binaries", "observation", "scope", *CLAIMS))
    require(type(report["version"]) is int and report["version"] == 1 and report["kind"] == KIND
            and report["profile"] == wrapper.PROFILE and report["target"] == expected_target
            and report["build_source"] == expected_source and report["build_session"] == expected_session,
            "report_context_mismatch")
    require(report["observation"] == OBSERVATION and report["scope"] == SCOPE
            and all(report[c] is False for c in CLAIMS), "unsupported_coverage_claim")
    keys(report["binary_sha256"], NAMES)
    keys(report["binaries"], NAMES)
    require(report["binary_sha256"] == binary_sha256, "binary_identity_mismatch")
    tools = None
    for name in NAMES:
        binary = report["binaries"][name]
        keys(binary, ("bytes", "binary_sha256", "normalized_sha256", "output", "inputs", "tools"))
        require(binary["binary_sha256"] == binary_sha256[name] and hash_value(binary["normalized_sha256"]), "binary_identity_mismatch")
        public_file(binary["output"])
        require(type(binary["bytes"]) is int and binary["bytes"] > 0
                and binary["output"]["bytes"] == binary["bytes"]
                and binary["output"]["sha256"] == binary_sha256[name]
                and binary["output"]["root"] == "target" and output_name(binary["output"]["path"], name), "binary_output_mismatch")
        inputs = binary["inputs"]
        require(isinstance(inputs, list) and 0 < len(inputs) <= wrapper.MAX_DEPENDENCIES, "missing_inputs")
        for value in inputs:
            public_file(value)
        identities = [(v["root"], v["path"]) for v in inputs]
        require(identities == sorted(set(identities)) and sum(v["bytes"] for v in inputs) <= wrapper.MAX_TOTAL_BYTES,
                "duplicate_or_unbounded_inputs")
        require((binary["output"]["root"], binary["output"]["path"]) not in identities, "self_input")
        keys(binary["tools"], ("compiler_driver", "gnu_linker"))
        for value in binary["tools"].values():
            keys(value, ("sha256", "bytes"))
            require(hash_value(value["sha256"]) and type(value["bytes"]) is int
                    and 0 <= value["bytes"] <= wrapper.MAX_INPUT_BYTES, "invalid_tool_identity")
        require(tools is None or tools == binary["tools"], "tool_identity_mismatch")
        tools = binary["tools"]
        require(digest(encode(normalized_payload(report, binary))) == binary["normalized_sha256"], "normalized_digest_mismatch")
    return report


def private_directory(path):
    info = path.lstat()
    require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0, "capture_directory_not_private")


def signature(info):
    return info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns, info.st_ctime_ns


def read_json(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        before = os.fstat(source.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_uid == os.geteuid()
                and before.st_mode & 0o077 == 0 and before.st_size <= wrapper.MAX_RECORD, "unsafe_capture_file")
        data = source.read(wrapper.MAX_RECORD + 1)
        require(len(data) <= wrapper.MAX_RECORD and signature(before) == signature(os.fstat(source.fileno())), "capture_changed_or_unbounded")
    value = json.loads(data, object_pairs_hook=wrapper.unique_json_object,
                       parse_constant=wrapper.invalid_json_constant)
    return value, data


def binary_identity(path, target):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as source:
        before = os.fstat(source.fileno())
        require(stat.S_ISREG(before.st_mode) and before.st_mode & 0o111 and 20 <= before.st_size <= wrapper.MAX_INPUT_BYTES, "binary_not_executable")
        header = source.read(20)
        require(header[:6] == b"\x7fELF\x02\x01" and int.from_bytes(header[18:20], "little") == wrapper.TARGETS[target][1], "binary_target_mismatch")
        result = hashlib.sha256(header)
        size = len(header)
        for block in iter(lambda: source.read(1024 * 1024), b""):
            size += len(block)
            require(size <= wrapper.MAX_INPUT_BYTES, "binary_size_limit")
            result.update(block)
        require(size == before.st_size and signature(before) == signature(os.fstat(source.fileno())), "binary_changed")
    return {"sha256": result.hexdigest(), "bytes": size}


def collect(*, capture_dir, bin_dir, target, source_revision, build_session):
    wrapper.build_context(source_revision, build_session)
    require(source_revision != "0" * 40 and isinstance(target, str)
            and target in wrapper.TARGETS, "invalid_expected_context")
    require(all(p.is_absolute() and not p.is_symlink() for p in (capture_dir, bin_dir)), "explicit_nonsymlink_directories_required")
    private_directory(capture_dir)
    require(bin_dir.is_dir(), "missing_binary_directory")
    observed = {name: binary_identity(bin_dir / name, target) for name in NAMES}
    selected, retained, listings, roots, total = {}, [], [], None, 0
    directories = sorted(capture_dir.iterdir())
    require(0 < len(directories) <= MAX_INVOCATIONS, "missing_or_unbounded_invocations")
    for directory in directories:
        require(re.fullmatch(r"invocation-[A-Za-z0-9_-]+", directory.name), "unknown_capture_entry")
        private_directory(directory)
        entries = {p.name for p in directory.iterdir()}
        require({"invocation.json", "capture.json", "normalized.json"} <= entries
                and entries <= {"invocation.json", "capture.json", "normalized.json", "inputs.d"}, "missing_or_unknown_capture_record")
        listings.append((directory, entries))
        values = {}
        for filename in ("invocation.json", "capture.json", "normalized.json"):
            value, data = read_json(directory / filename)
            total += len(data)
            require(total <= MAX_CAPTURE_BYTES, "capture_total_limit")
            retained.append((directory / filename, digest(data)))
            values[filename] = value, data
        raw = values["capture.json"][0]
        normalized = wrapper.normalize_capture(raw, expected_source=source_revision,
                                               expected_session=build_session, expected_target=target)
        invocation = values["invocation.json"][0]
        keys(invocation, wrapper.COMMON_FIELDS)
        require(encode(invocation) == encode({k: raw[k] for k in wrapper.COMMON_FIELDS}), "invocation_context_mismatch")
        require(values["normalized.json"][1] == encode(normalized), "normalized_record_mismatch")
        output = normalized["output"]
        matches = [name for name in NAMES if observed[name] == {"sha256": output["sha256"], "bytes": output["bytes"]}]
        if not matches:
            continue
        require(len(matches) == 1, "ambiguous_binary_match")
        name = matches[0]
        require(output_name(output["path"], name), "binary_output_name_mismatch")
        require(name not in selected, "duplicate_binary_invocation")
        require(roots is None or roots == raw["roots"], "mixed_root_context")
        roots = raw["roots"]
        selected[name] = {"bytes": output["bytes"], "binary_sha256": output["sha256"],
                          "normalized_sha256": digest(values["normalized.json"][1]),
                          "output": output, "inputs": normalized["inputs"], "tools": normalized["tools"]}
    require(set(selected) == set(NAMES), "missing_matching_link_invocation")
    report = {"version": 1, "kind": KIND, "profile": wrapper.PROFILE, "target": target,
              "build_source": source_revision, "build_session": build_session,
              "binary_sha256": {n: observed[n]["sha256"] for n in NAMES}, "binaries": selected,
              "observation": OBSERVATION, "scope": SCOPE, **{c: False for c in CLAIMS}}
    validate_report(report, expected_source=source_revision, expected_target=target,
                    expected_session=build_session, binary_sha256=report["binary_sha256"])
    require(all(binary_identity(bin_dir / n, target) == observed[n] for n in NAMES), "binary_changed")
    require(all(digest(read_json(p)[1]) == value for p, value in retained)
            and sorted(capture_dir.iterdir()) == directories
            and all({p.name for p in directory.iterdir()} == entries for directory, entries in listings), "capture_changed")
    return report


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("capture-dir", "bin-dir", "output-dir"):
        parser.add_argument("--" + name, required=True, type=Path)
    parser.add_argument("--target", required=True, choices=wrapper.TARGETS)
    parser.add_argument("--source-revision", required=True)
    parser.add_argument("--build-session", required=True)
    args = parser.parse_args(argv)
    try:
        require(args.output_dir.is_absolute() and not args.output_dir.is_symlink(), "explicit_output_directory_required")
        require(not any(args.output_dir.resolve().is_relative_to(p.resolve())
                        for p in (args.capture_dir, args.bin_dir)), "output_overlaps_capture_or_binaries")
        report = collect(capture_dir=args.capture_dir, bin_dir=args.bin_dir, target=args.target,
                         source_revision=args.source_revision, build_session=args.build_session)
        args.output_dir.mkdir(mode=0o700, exist_ok=True)
        require(args.output_dir.is_dir(), "invalid_output_directory")
        wrapper.durable_json(args.output_dir, "native-inputs.json", report)
        print(json.dumps({"version": 1, "native_inputs_sha256": digest(encode(report)),
                          "binary_sha256": report["binary_sha256"], **{c: False for c in CLAIMS}}))
        return 0
    except (OSError, ValueError, KeyError, TypeError, AttributeError):
        parser.exit(1, "Native link-input observations rejected; preserve private captures and inspect the selected build context.\n")


if __name__ == "__main__":
    sys.exit(main())
