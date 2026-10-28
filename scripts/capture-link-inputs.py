#!/usr/bin/env python3
"""Capture Linux/GNU ld file inputs during a build; never a complete SBOM.

Copyright 2026 Artishchev Andrey Borisovich. Licensed under Elastic License 2.0.

Use this executable as CARGO_TARGET_<TRIPLE>_LINKER during the canonical build.
Required env: TH_LINK_CAPTURE_DIR (existing owner-only directory), TH_LINK_REAL_CC
(absolute compiler driver), TH_LINK_TARGET, TH_LINK_SYSROOT_ROOT,
TH_LINK_REPO_ROOT, TH_LINK_TARGET_ROOT and TH_LINK_CARGO_ROOT. Also required:
TH_LINK_BUILD_SOURCE (lowercase 40-hex checkout revision) and
TH_LINK_BUILD_SESSION (canonical lowercase nonnil UUID shared by the build).
These environment values are observations, not source or build attestations.
Put TMPDIR under the target root so rustc's temporary objects classify. No
response files, linker overrides or unreviewed argument/dependency syntax.
Cargo's LD_LIBRARY_PATH is accepted only when every component is an existing
canonical absolute directory within the configured sysroot or target root.
Empty/foreign components and symlink aliases are rejected. The reviewed variable
is removed from a copied environment for GNU driver/linker probes and the link;
the parent Cargo/Rust environment is unchanged. Other tool overrides stay banned.

Private capture.json retains raw paths/argv. normalized.json contains only rooted
relative input/output identities and tool hashes, and is absent on unknown paths.
Hashes are observed after successful link, before this wrapper returns to rustc;
this does not prove immutability during linking, member attribution, native
completeness, compiler reproducibility or source/build provenance. The wrapper
checks ELF architecture; the configured target is not an independent ABI proof.
It never deletes or rewrites the compiler's output. Compiler failures/signals are
preserved; a successful link whose capture fails returns wrapper error 2.

`capture-link-inputs.py normalize PRIVATE_CAPTURE.json` re-normalizes recorded
hashes without reopening inputs, which may already have been removed by rustc.
GNU dependency-file contract: https://sourceware.org/binutils/docs/ld/Options.html

Importable collectors use read_private_capture(path, expected_source=...,
expected_session=..., expected_target=...) and normalize_capture(record, with the
same keyword arguments). Neither function reopens recorded input/output paths.
"""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import platform
import re
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import uuid

sys.dont_write_bytecode = True
PROFILE = "linux-gnu-ld-dependency-file-v1"
ROOT_KINDS = ("sysroot", "repo", "target", "cargo")
TARGETS = {"x86_64-unknown-linux-musl": ("x86_64", 62),
           "aarch64-unknown-linux-musl": ("aarch64", 183)}
MAX_DEPENDENCIES = 16384
MAX_DEPFILE = 8 * 1024 * 1024
MAX_RECORD = 16 * 1024 * 1024
MAX_INPUT_BYTES = 1024 * 1024 * 1024
MAX_TOTAL_BYTES = 16 * 1024 * 1024 * 1024
SAFE_WORD = re.compile(r"[A-Za-z0-9_./+=-]+\Z")
SHA = re.compile(r"[0-9a-f]{64}\Z")
SOURCE = re.compile(r"[0-9a-f]{40}\Z")
COMMON_FIELDS = {"version", "profile", "target", "build_source", "build_session", "cwd", "roots",
                 "arguments", "configured_compiler"}
CLAIM_FIELDS = {"source_provenance_verified", "build_provenance_verified",
                "native_component_inventory_complete", "archive_member_attribution_complete"}
SUCCESS_FIELDS = COMMON_FIELDS | CLAIM_FIELDS | {"status", "compiler_returncode", "compiler_driver",
                                                "gnu_linker", "output", "inputs"}
DIRECT_FLAGS = {"-static", "-static-pie", "-pie", "-no-pie", "-nostartfiles",
                "-nodefaultlibs", "-nostdlib", "-pthread", "-rdynamic", "-s", "-m64"}
LD_FLAGS = {"--as-needed", "--no-as-needed", "-Bstatic", "-Bdynamic", "--eh-frame-hdr",
            "--gc-sections", "--no-gc-sections", "--strip-debug", "--strip-all",
            "--build-id", "--build-id=sha1", "--build-id=none", "--hash-style=gnu",
            "--hash-style=sysv", "--export-dynamic", "--no-undefined-version",
            "--fix-cortex-a53-843419", "--no-undefined", "--fatal-warnings",
            "--enable-new-dtags", "--disable-new-dtags", "--warn-execstack",
            "--no-warn-execstack", "--warn-rwx-segments", "--no-warn-rwx-segments"}
LD_Z = {"noexecstack", "relro", "now", "pack-relative-relocs", "separate-code", "norelro", "lazy"}


class CaptureError(ValueError):
    pass


def require(condition, code):
    if not condition:
        raise CaptureError(code)


def path_word(value):
    return isinstance(value, str) and 0 < len(value) <= 4096 and SAFE_WORD.fullmatch(value) is not None


def absolute(value, cwd, exists=False):
    require(path_word(str(value)), "unsupported_path_syntax")
    path = Path(value)
    if not path.is_absolute():
        path = cwd / path
    return path.resolve(strict=exists)


def durable_json(directory, name, value):
    data = (json.dumps(value, sort_keys=True, indent=2, allow_nan=False) + "\n").encode()
    require(len(data) <= MAX_RECORD, "record_limit_exceeded")
    fd = os.open(directory / name, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "wb") as output:
        output.write(data)
        output.flush()
        os.fsync(output.fileno())
    sync_directory(directory)


def sync_directory(directory):
    directory_fd = os.open(directory, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(directory_fd)
    finally:
        os.close(directory_fd)


def file_hash(path):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        before = os.fstat(fd)
        require(stat.S_ISREG(before.st_mode) and before.st_size <= MAX_INPUT_BYTES, "input_not_bounded_regular_file")
        digest, size = hashlib.sha256(), 0
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            size += len(block)
            require(size <= MAX_INPUT_BYTES, "input_size_limit_exceeded")
            digest.update(block)
        after = os.fstat(fd)
    require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
            == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns)
            and size == before.st_size, "input_changed_while_hashing")
    return {"path": str(path), "sha256": digest.hexdigest(), "bytes": size}


def read_bounded_regular(path, limit):
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, "rb") as stream:
        info = os.fstat(fd)
        require(stat.S_ISREG(info.st_mode) and info.st_size <= limit, "record_not_bounded_regular_file")
        data = stream.read(limit + 1)
    require(len(data) <= limit, "record_size_limit_exceeded")
    return data


def build_context(source, session):
    require(isinstance(source, str) and SOURCE.fullmatch(source) is not None, "invalid_build_source")
    require(isinstance(session, str), "invalid_build_session")
    try:
        identity = uuid.UUID(session)
    except ValueError:
        raise CaptureError("invalid_build_session") from None
    require(str(identity) == session and identity.int != 0, "invalid_build_session")
    return source, session


def reviewed_roots(values):
    require(isinstance(values, dict) and set(values) == set(ROOT_KINDS), "invalid_root_classes")
    result = {}
    for kind, value in values.items():
        require(path_word(value) and value.startswith("/"), "invalid_root_path")
        path = PurePosixPath(value)
        require(str(path) == value and path != PurePosixPath("/") and ".." not in path.parts, "unsafe_root_path")
        result[kind] = path
    require(len(set(result.values())) == len(result), "ambiguous_root_classes")
    return result


def rooted(value, roots):
    require(path_word(value) and value.startswith("/"), "invalid_recorded_path")
    path = PurePosixPath(value)
    require(str(path) == value and ".." not in path.parts, "invalid_recorded_path")
    matches = [(len(root.parts), kind, root) for kind, root in roots.items() if path.is_relative_to(root)]
    require(matches, "unclassified_input_path")
    _, kind, root = max(matches)
    relative = path.relative_to(root).as_posix()
    require(relative != "." and not relative.startswith("/"), "invalid_root_relative_path")
    return {"root": kind, "path": relative}


def compiler_environment(env, roots):
    """Validate Cargo's Rust-only library search path, then omit it for GNU tools."""
    for key in ("COLLECT_LD", "COMPILER_PATH", "GCC_EXEC_PREFIX", "LD_PRELOAD"):
        require(not env.get(key), "unreviewed_tool_override_environment")
    child = dict(env)
    if "LD_LIBRARY_PATH" not in env:
        return child
    value = env["LD_LIBRARY_PATH"]
    require(isinstance(value, str) and 0 < len(value) <= 128 * 4096, "invalid_cargo_library_path")
    components = value.split(":")
    require(len(components) <= 128, "cargo_library_path_limit")
    known = reviewed_roots(roots)
    allowed = [Path(str(known[kind])) for kind in ("sysroot", "target")]
    for component in components:
        require(path_word(component) and component.startswith("/"), "invalid_cargo_library_path")
        path = Path(component)
        require(str(path) == component and str(path.resolve(strict=True)) == component and path.is_dir(),
                "noncanonical_cargo_library_directory")
        require(any(path == root or path.is_relative_to(root) for root in allowed), "foreign_cargo_library_directory")
    del child["LD_LIBRARY_PATH"]
    return child


@dataclass(frozen=True)
class Config:
    capture_dir: Path
    compiler: Path
    target: str
    build_source: str
    build_session: str
    roots: dict
    cwd: Path

    @classmethod
    def from_environment(cls, env, cwd=None):
        cwd = Path(cwd or os.getcwd()).resolve(strict=True)
        require(path_word(str(cwd)), "unsupported_working_directory")
        capture_arg = Path(env["TH_LINK_CAPTURE_DIR"])
        compiler_arg = Path(env["TH_LINK_REAL_CC"])
        require(capture_arg.is_absolute() and compiler_arg.is_absolute(), "explicit_absolute_configuration_required")
        require(not capture_arg.is_symlink(), "symlink_capture_root")
        capture = absolute(capture_arg, cwd, exists=True)
        compiler = absolute(compiler_arg, cwd, exists=True)
        info = capture.stat()
        require(stat.S_ISDIR(info.st_mode) and info.st_uid == os.geteuid() and info.st_mode & 0o077 == 0, "capture_root_not_private")
        require(compiler.is_file() and os.access(compiler, os.X_OK), "compiler_not_executable")
        require(not os.path.samefile(compiler, Path(__file__).resolve()), "recursive_compiler_wrapper")
        roots = {kind: str(absolute(env["TH_LINK_" + kind.upper() + "_ROOT"], cwd, exists=True)) for kind in ROOT_KINDS}
        require(all(Path(p).is_dir() for p in roots.values()), "root_not_directory")
        reviewed_roots(roots)
        require(env["TH_LINK_TARGET"] in TARGETS, "unsupported_target")
        source, session = build_context(env["TH_LINK_BUILD_SOURCE"], env["TH_LINK_BUILD_SESSION"])
        compiler_environment(env, roots)
        return cls(capture, compiler, env["TH_LINK_TARGET"], source, session, roots, cwd)


def validate_ld_arguments(words, cwd):
    index = 0
    while index < len(words):
        word = words[index]
        if word in LD_FLAGS or re.fullmatch(r"-O[0-3]", word):
            index += 1
        elif word == "-z":
            require(index + 1 < len(words) and words[index + 1] in LD_Z, "unsupported_ld_z_option")
            index += 2
        elif word == "-u":
            # The pinned cargo-auditable linker wrapper retains this exact symbol.
            require(index + 1 < len(words) and words[index + 1] == "AUDITABLE_VERSION_INFO", "unsupported_undefined_symbol")
            index += 2
        elif word in ("-T", "--script", "--version-script", "--dynamic-list"):
            require(index + 1 < len(words), "missing_linker_script_path")
            require(absolute(words[index + 1], cwd, exists=True).is_file(), "linker_script_not_file")
            index += 2
        elif any(word.startswith(prefix) for prefix in ("--script=", "--version-script=", "--dynamic-list=")):
            require(absolute(word.split("=", 1)[1], cwd, exists=True).is_file(), "linker_script_not_file")
            index += 1
        else:
            raise CaptureError("unsupported_linker_argument")


def validate_arguments(arguments, config):
    require(arguments and len(arguments) <= 65536 and all(isinstance(a, str) and len(a) <= 16384 for a in arguments), "invalid_argument_bounds")
    require(all("@" not in a and "\x00" not in a for a in arguments), "response_files_unsupported")
    require(arguments.count("-o") == 1, "one_explicit_output_required")
    index, output = 0, None
    while index < len(arguments):
        arg = arguments[index]
        if arg == "-o":
            require(index + 1 < len(arguments), "missing_output_argument")
            raw_output = config.cwd / arguments[index + 1] if not Path(arguments[index + 1]).is_absolute() else Path(arguments[index + 1])
            require(not raw_output.is_symlink(), "symlink_output")
            output = absolute(arguments[index + 1], config.cwd)
            require(output.parent.is_dir(), "missing_output_parent")
            require(rooted(str(output), reviewed_roots(config.roots))["root"] == "target", "output_outside_target_root")
            index += 2
        elif arg == "-L":
            require(index + 1 < len(arguments) and absolute(arguments[index + 1], config.cwd, exists=True).is_dir(), "invalid_library_search_path")
            index += 2
        elif arg.startswith("-L"):
            require(absolute(arg[2:], config.cwd, exists=True).is_dir(), "invalid_library_search_path")
            index += 1
        elif re.fullmatch(r"-l[A-Za-z0-9_+.-]+", arg) or arg in DIRECT_FLAGS:
            index += 1
        elif arg.startswith("-Wl,"):
            validate_ld_arguments(arg[4:].split(","), config.cwd)
            index += 1
        elif arg.startswith("-"):
            raise CaptureError("unsupported_compiler_argument")
        else:
            path = absolute(arg, config.cwd, exists=True)
            require(path.is_file() and path.suffix not in (".c", ".cc", ".cpp", ".cxx", ".s", ".S", ".rs"), "link_only_inputs_required")
            index += 1
    require(output is not None, "one_explicit_output_required")
    return output


def run_process(command, *, cwd, env, capture_output=False):
    return subprocess.run(command, cwd=cwd, env=env, capture_output=capture_output,
                          timeout=15 if capture_output else None, check=False)


def gnu_profile(config, env, process):
    env = compiler_environment(env, config.roots)
    selected = process([str(config.compiler), "-print-prog-name=ld"], cwd=config.cwd, env=env, capture_output=True)
    require(selected.returncode == 0 and isinstance(selected.stdout, bytes) and len(selected.stdout) <= 65536, "cannot_resolve_driver_linker")
    try:
        text = selected.stdout.decode("utf-8").strip()
    except UnicodeError:
        raise CaptureError("invalid_driver_linker_path") from None
    require(path_word(text), "invalid_driver_linker_path")
    if "/" in text:
        linker = absolute(text, config.cwd, exists=True)
    else:
        located = shutil.which(text, path=env.get("PATH", os.defpath))
        require(located, "driver_linker_not_found")
        linker = Path(located).resolve(strict=True)
    version = process([str(linker), "--version"], cwd=config.cwd, env=env, capture_output=True)
    require(version.returncode == 0 and isinstance(version.stdout, bytes) and len(version.stdout) <= 65536 and version.stdout.startswith(b"GNU ld ("), "unsupported_non_gnu_ld_profile")
    return linker


def dependencies(data, output, cwd):
    require(0 < len(data) <= MAX_DEPFILE, "dependency_file_limit")
    try:
        text = data.decode("utf-8")
    except UnicodeError:
        raise CaptureError("invalid_dependency_encoding") from None
    require("\r" not in text, "unsupported_dependency_syntax")
    text = text.replace("\\\n", " ")
    require(not any(c in text for c in "\\#$%*?[]"), "unsupported_dependency_syntax")
    rules = [line.strip() for line in text.splitlines() if line.strip()]
    require(rules and all(line.count(":") == 1 for line in rules), "unsupported_dependency_rules")
    target, words = rules[0].split(":", 1)
    require(absolute(target, cwd) == output, "dependency_target_mismatch")
    names = words.split()
    require(0 < len(names) <= MAX_DEPENDENCIES and len(set(names)) == len(names), "empty_or_duplicate_dependencies")
    require(all(path_word(name) for name in names), "unsupported_dependency_path")
    phony = []
    for rule in rules[1:]:
        name, body = rule.split(":", 1)
        require(path_word(name) and not body.strip(), "unsupported_additional_dependency_rule")
        phony.append(name)
    require(len(set(phony)) == len(phony) and set(phony) == set(names), "incomplete_or_foreign_phony_rules")
    paths = [absolute(name, cwd, exists=True) for name in names]
    require(len(set(paths)) == len(paths) and output not in paths, "aliased_or_self_dependencies")
    return paths


def normalize_capture(record, *, expected_source=None, expected_session=None, expected_target=None):
    """Validate and normalize recorded hashes without trusting paths as public text.

    Expected values bind a collector's selected build context; matching environment
    observations still establish neither signed provenance nor source attribution.
    """
    require(isinstance(record, dict) and type(record.get("version")) is int and record["version"] == 1
            and record.get("profile") == PROFILE and record.get("status") == "private_link_inputs_captured"
            and type(record.get("compiler_returncode")) is int and record["compiler_returncode"] == 0, "record_not_successful_capture")
    require(set(record) == SUCCESS_FIELDS, "unexpected_capture_fields")
    for field in ("cwd", "configured_compiler"):
        value = record.get(field)
        require(path_word(value) and value.startswith("/") and str(PurePosixPath(value)) == value
                and ".." not in PurePosixPath(value).parts, "invalid_recorded_configuration_path")
    arguments = record.get("arguments")
    require(isinstance(arguments, list) and 0 < len(arguments) <= 65536
            and all(isinstance(arg, str) and len(arg) <= 16384 and "@" not in arg and "\x00" not in arg for arg in arguments),
            "invalid_recorded_arguments")
    roots = reviewed_roots(record.get("roots"))
    require(isinstance(record.get("target"), str) and record["target"] in TARGETS, "unsupported_record_target")
    source, session = build_context(record.get("build_source"), record.get("build_session"))
    if expected_source is not None:
        build_context(expected_source, session)
        require(source == expected_source, "build_source_mismatch")
    if expected_session is not None:
        build_context(source, expected_session)
        require(session == expected_session, "build_session_mismatch")
    if expected_target is not None:
        require(isinstance(expected_target, str) and expected_target in TARGETS and record["target"] == expected_target, "build_target_mismatch")
    def mapped(value):
        require(isinstance(value, dict) and set(value) == {"path", "sha256", "bytes"}
                and isinstance(value.get("sha256"), str) and SHA.fullmatch(value["sha256"])
                and type(value.get("bytes")) is int and 0 <= value["bytes"] <= MAX_INPUT_BYTES, "invalid_recorded_file")
        return dict(rooted(value.get("path"), roots), sha256=value["sha256"], bytes=value["bytes"])
    output = mapped(record.get("output"))
    require(output["root"] == "target", "record_output_outside_target")
    inputs = record.get("inputs")
    require(isinstance(inputs, list) and 0 < len(inputs) <= MAX_DEPENDENCIES, "invalid_recorded_inputs")
    inputs = [mapped(value) for value in inputs]
    require(len({(v["root"], v["path"]) for v in inputs}) == len(inputs), "duplicate_recorded_input")
    require(all((value["root"], value["path"]) != (output["root"], output["path"]) for value in inputs), "self_dependency_record")
    require(sum(value["bytes"] for value in inputs) <= MAX_TOTAL_BYTES, "recorded_input_total_size_limit")
    tools = {}
    for role in ("compiler_driver", "gnu_linker"):
        value = record.get(role)
        require(isinstance(value, dict) and set(value) == {"path", "sha256", "bytes"}
                and path_word(value.get("path")) and value["path"].startswith("/")
                and isinstance(value.get("sha256"), str) and SHA.fullmatch(value["sha256"])
                and type(value.get("bytes")) is int and 0 <= value["bytes"] <= MAX_INPUT_BYTES, "invalid_recorded_tool")
        tools[role] = {"sha256": value["sha256"], "bytes": value["bytes"]}
    require(record["compiler_driver"]["path"] == record["configured_compiler"], "compiler_driver_path_mismatch")
    for claim in ("source_provenance_verified", "build_provenance_verified", "native_component_inventory_complete", "archive_member_attribution_complete"):
        require(record.get(claim) is False, "unsupported_verified_claim")
    return {"version": 1, "profile": PROFILE, "target": record["target"],
            "build_source": source, "build_session": session, "output": output,
            "inputs": sorted(inputs, key=lambda p: (p["root"], p["path"])), "tools": tools,
            "observation": "file hashes read after successful link and before wrapper return",
            "source_provenance_verified": False, "build_provenance_verified": False,
            "native_component_inventory_complete": False, "archive_member_attribution_complete": False}


# Kept for callers of the initial normalization interface.
normalize_record = normalize_capture


def unique_json_object(pairs):
    value = {}
    for key, item in pairs:
        require(key not in value, "duplicate_json_key")
        value[key] = item
    return value


def invalid_json_constant(value):
    raise CaptureError("invalid_json_constant")


def read_private_capture(path, *, expected_source=None, expected_session=None, expected_target=None):
    """Read one bounded, nonsymlink capture and return its validated raw record.

    This reads only the private JSON record, including its raw paths. It never
    opens recorded input/output files, which may already have been removed.
    """
    record = json.loads(read_bounded_regular(Path(path), MAX_RECORD),
                        object_pairs_hook=unique_json_object, parse_constant=invalid_json_constant)
    normalize_capture(record, expected_source=expected_source, expected_session=expected_session,
                      expected_target=expected_target)
    return record


def capture_link(config, arguments, env, process=run_process, system=None, machine=None):
    require((system or platform.system()) == "Linux", "unsupported_non_linux_host")
    require((machine or platform.machine()) == TARGETS[config.target][0], "unsupported_cross_compilation_profile")
    source, session = build_context(env.get("TH_LINK_BUILD_SOURCE"), env.get("TH_LINK_BUILD_SESSION"))
    require((source, session) == (config.build_source, config.build_session), "build_context_changed_after_configuration")
    compiler_env = compiler_environment(env, config.roots)
    output = validate_arguments(arguments, config)
    directory = Path(tempfile.mkdtemp(prefix="invocation-", dir=config.capture_dir))
    sync_directory(config.capture_dir)
    common = {"version": 1, "profile": PROFILE, "target": config.target, "cwd": str(config.cwd),
              "build_source": source, "build_session": session,
              "roots": config.roots, "arguments": list(arguments), "configured_compiler": str(config.compiler)}
    durable_json(directory, "invocation.json", common)
    compiler_returncode = None
    try:
        linker = gnu_profile(config, compiler_env, process)
        driver_before, linker_before = file_hash(config.compiler), file_hash(linker)
        depfile = directory / "inputs.d"
        command = [str(config.compiler), *arguments, "-Wl,--dependency-file=" + str(depfile)]
        completed = process(command, cwd=config.cwd, env=compiler_env, capture_output=False)
        compiler_returncode = completed.returncode
        require(type(compiler_returncode) is int and -255 <= compiler_returncode <= 255, "invalid_compiler_returncode")
        if compiler_returncode != 0:
            try:
                durable_json(directory, "capture.json", dict(common, status="compiler_failed", compiler_returncode=compiler_returncode))
            except (CaptureError, OSError):
                # A full evidence disk must not replace the compiler's exit/signal.
                pass
            return compiler_returncode
        data = read_bounded_regular(depfile, MAX_DEPFILE)
        paths = dependencies(data, output, config.cwd)
        inputs, total = [], 0
        for path in paths:
            value = file_hash(path)
            total += value["bytes"]
            require(total <= MAX_TOTAL_BYTES, "input_total_size_limit")
            inputs.append(value)
        output_record = file_hash(output)
        with output.open("rb") as output_stream:
            header = output_stream.read(20)
        require(header[:6] == b"\x7fELF\x02\x01" and len(header) == 20
                and int.from_bytes(header[18:20], "little") == TARGETS[config.target][1], "linked_output_target_mismatch")
        require(file_hash(output) == output_record, "output_changed_during_capture")
        require(file_hash(config.compiler) == driver_before and file_hash(linker) == linker_before, "tool_changed_during_link")
        raw = dict(common, status="private_link_inputs_captured", compiler_returncode=0,
                   compiler_driver=driver_before, gnu_linker=linker_before, output=output_record,
                   inputs=inputs, source_provenance_verified=False, build_provenance_verified=False,
                   native_component_inventory_complete=False, archive_member_attribution_complete=False)
        # Persist private input hashes first; normalize without reopening ephemeral files.
        durable_json(directory, "capture.json", raw)
        try:
            normalized = normalize_capture(raw, expected_source=source, expected_session=session, expected_target=config.target)
        except CaptureError:
            durable_json(directory, "normalization-error.json", {"version": 1, "status": "rejected", "error": "unclassified_or_invalid_recorded_input"})
            return 2
        durable_json(directory, "normalized.json", normalized)
        return 0
    except (CaptureError, OSError, KeyError, ValueError, subprocess.SubprocessError) as error:
        try:
            if not (directory / "capture.json").exists():
                durable_json(directory, "capture.json", dict(common, status="capture_failed", compiler_returncode=compiler_returncode,
                                                             error=str(error) if isinstance(error, CaptureError) else type(error).__name__))
        except (CaptureError, OSError):
            pass
        return compiler_returncode if type(compiler_returncode) is int and compiler_returncode != 0 else 2


def main(argv=None, env=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    env = dict(os.environ if env is None else env)
    try:
        if argv and argv[0] == "normalize":
            require(len(argv) == 2, "normalize_requires_one_private_record")
            path = Path(argv[1])
            record = read_private_capture(path)
            print(json.dumps(normalize_capture(record), sort_keys=True, indent=2))
            return 0
        config = Config.from_environment(env)
        returncode = capture_link(config, argv, env)
        if returncode < 0:
            signum = -returncode
            if signum not in (signal.SIGKILL, signal.SIGSTOP):
                signal.signal(signum, signal.SIG_DFL)
            os.kill(os.getpid(), signum)
        if returncode == 2:
            print("link input capture failed; inspect private invocation records", file=sys.stderr)
        return returncode
    except (CaptureError, OSError, KeyError, ValueError, TypeError, AttributeError, subprocess.SubprocessError):
        print("link input capture rejected unsupported configuration or syntax", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
