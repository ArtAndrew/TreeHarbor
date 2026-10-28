#!/usr/bin/env python3
"""Self-contained collector tests; no private work artifacts or canonical build.

ARM sysroot paths/tool identity shapes are drawn from the retained synthetic GNU
ARM probe. Executable bytes, source/session and invocation records below are
controlled fixtures, not a TreeHarbor build or provenance evidence.
"""
from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SPEC = importlib.util.spec_from_file_location("native_inputs", Path(__file__).with_name("capture-native-inputs.py"))
collector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(collector)
SOURCE = "a" * 40
SESSION = "00000000-0000-4000-8000-000000000001"
TARGET = "aarch64-unknown-linux-musl"


def elf(name, machine=183):
    data = bytearray(64)
    data[:6] = b"\x7fELF\x02\x01"
    data[18:20] = machine.to_bytes(2, "little")
    return bytes(data) + name.encode()


class CollectorTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="native-input-test-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.capture_dir = self.base / "private-captures"
        self.capture_dir.mkdir(mode=0o700)
        self.bin_dir = self.base / "target" / "release"
        self.bin_dir.mkdir(parents=True)
        self.records = {}
        for index, name in enumerate(collector.NAMES):
            binary = self.bin_dir / name
            binary.write_bytes(elf(name))
            binary.chmod(0o700)
            directory = self.capture_dir / ("invocation-fixture" + str(index))
            directory.mkdir(mode=0o700)
            roots = {kind: str(self.base / kind) for kind in collector.wrapper.ROOT_KINDS}
            output = str(self.base / "target" / TARGET / "release/deps" / (name.replace("-", "_") + "-0123456789abcdef"))
            raw = {"version": 1, "profile": collector.wrapper.PROFILE, "target": TARGET,
                   "build_source": SOURCE, "build_session": SESSION, "cwd": roots["repo"], "roots": roots,
                   "arguments": ["-static", "-o", output], "configured_compiler": "/toolchain/bin/cc",
                   "status": "private_link_inputs_captured", "compiler_returncode": 0,
                   "compiler_driver": {"path": "/toolchain/bin/cc", "bytes": 990040,
                                       "sha256": "a20520ee21543f243d40636a9181a142c45ecd989de31ab86b99a8ea5ada870d"},
                   "gnu_linker": {"path": "/toolchain/bin/ld.bfd", "bytes": 1710520,
                                  "sha256": "1e4d3369b76845fa8e099b83513bf28f6f722e046f9b2cde1378c9e27f96d19c"},
                   "output": {"path": output, "bytes": binary.stat().st_size,
                              "sha256": hashlib.sha256(binary.read_bytes()).hexdigest()},
                   "inputs": [
                       {"path": roots["sysroot"] + "/lib/rustlib/" + TARGET + "/lib/libaddr2line-fbc53bd7e939c40a.rlib", "bytes": 30026,
                        "sha256": "a7d2da7d5f4d8722e74114c10002249f4d6f8550f67a99c242cfcff1262636a5"},
                       {"path": roots["target"] + "/tmp/rustc-ephemeral/symbols.o", "bytes": 32, "sha256": "1" * 64},
                       {"path": roots["target"] + "/deps/" + name.replace("-", "_") + "_audit_data.o", "bytes": 64, "sha256": "2" * 64}],
                   **{c: False for c in collector.CLAIMS[:-1]}}
            self.records[name] = directory, raw
            self.persist(name)

    def write(self, path, value):
        path.write_bytes(collector.encode(value))
        path.chmod(0o600)

    def persist(self, name):
        directory, raw = self.records[name]
        self.write(directory / "capture.json", raw)
        self.write(directory / "invocation.json", {k: raw[k] for k in collector.wrapper.COMMON_FIELDS})
        self.write(directory / "normalized.json", collector.wrapper.normalize_capture(raw))

    def collect(self, **overrides):
        return collector.collect(**{"capture_dir": self.capture_dir, "bin_dir": self.bin_dir,
                                    "target": TARGET, "source_revision": SOURCE,
                                    "build_session": SESSION, **overrides})

    def validate(self, report, **overrides):
        return collector.validate_report(report, **{"expected_source": SOURCE, "expected_target": TARGET,
                                                    "expected_session": SESSION,
                                                    "binary_sha256": self.collect()["binary_sha256"], **overrides})

    def rejects(self, operation):
        with self.assertRaises((ValueError, OSError)):
            operation()

    def test_two_actual_binary_hashes_and_arm_input_shapes_without_ephemeral_reopens(self):
        before = {p: p.read_bytes() for p in self.capture_dir.glob("*/*.json")}
        # Recorded objects and link outputs intentionally do not exist anymore.
        with patch.object(collector.wrapper, "file_hash", side_effect=AssertionError("ephemeral input reopened")):
            report = self.collect()
        self.assertIs(self.validate(report), report)
        for name in collector.NAMES:
            binary = report["binaries"][name]
            self.assertEqual(binary["binary_sha256"], hashlib.sha256((self.bin_dir / name).read_bytes()).hexdigest())
            self.assertTrue(any(v["path"].endswith("_audit_data.o") for v in binary["inputs"]))
        text = json.dumps(report)
        self.assertNotIn(str(self.base), text)
        self.assertNotIn("/toolchain", text)
        self.assertTrue(all(report[c] is False for c in collector.CLAIMS))
        self.assertEqual(before, {p: p.read_bytes() for p in before})

    def test_cached_missing_incomplete_or_failed_invocation_is_not_inventory(self):
        first, _ = self.records[collector.NAMES[0]]
        (first / "normalized.json").unlink()
        self.rejects(self.collect)
        shutil.rmtree(first)
        self.rejects(self.collect)
        shutil.rmtree(self.records[collector.NAMES[1]][0])
        self.rejects(self.collect)

    def test_replaced_binary_duplicate_capture_and_wrong_output_name_fail(self):
        path = self.bin_dir / collector.NAMES[0]
        original = path.read_bytes()
        path.write_bytes(original + b"replacement")
        self.rejects(self.collect)
        path.write_bytes(original)
        directory, raw = self.records[collector.NAMES[0]]
        duplicate = self.capture_dir / "invocation-duplicate"
        shutil.copytree(directory, duplicate)
        self.rejects(self.collect)
        shutil.rmtree(duplicate)
        raw["output"]["path"] = raw["roots"]["target"] + "/unknown-program"
        self.persist(collector.NAMES[0])
        self.rejects(self.collect)

    def test_stale_or_counterfeit_observed_context_rejected_even_if_normalized_matches(self):
        for field, value in [("build_source", "b" * 40), ("build_session", "00000000-0000-4000-8000-000000000002"),
                             ("target", "x86_64-unknown-linux-musl")]:
            with self.subTest(field=field):
                name = collector.NAMES[0]
                _, raw = self.records[name]
                old = raw[field]
                raw[field] = value
                self.persist(name)
                self.rejects(self.collect)
                raw[field] = old
                self.persist(name)

    def test_normalized_private_path_unknown_field_or_forged_claim_not_trusted(self):
        directory, _ = self.records[collector.NAMES[0]]
        path = directory / "normalized.json"
        original = json.loads(path.read_bytes())
        for mutation in [lambda v: v["output"].update(path="/private/secret"),
                         lambda v: v.update(extra="unknown"),
                         lambda v: v.update(build_provenance_verified=True),
                         lambda v: v.update(build_provenance_verified=0)]:
            value = deepcopy(original)
            mutation(value)
            self.write(path, value)
            self.rejects(self.collect)
        self.write(path, original)

    def test_raw_forged_claim_unknown_keys_and_invocation_mismatch_rejected(self):
        directory, raw = self.records[collector.NAMES[0]]
        for mutation in [lambda v: v.update(source_provenance_verified=True),
                         lambda v: v.update(extra="/private/unknown")]:
            changed = deepcopy(raw)
            mutation(changed)
            self.write(directory / "capture.json", changed)
            self.rejects(self.collect)
        self.persist(collector.NAMES[0])
        value = json.loads((directory / "invocation.json").read_bytes())
        value["build_source"] = "b" * 40
        self.write(directory / "invocation.json", value)
        self.rejects(self.collect)

    def test_duplicate_json_symlink_or_public_capture_input_rejected(self):
        directory, _ = self.records[collector.NAMES[0]]
        path = directory / "normalized.json"
        original = path.read_bytes()
        path.write_bytes(b'{"version":1,"version":1}')
        self.rejects(self.collect)
        path.unlink()
        target = self.base / "private-normalized.json"
        target.write_bytes(original)
        target.chmod(0o600)
        path.symlink_to(target)
        self.rejects(self.collect)
        path.unlink()
        path.write_bytes(original)
        path.chmod(0o644)
        self.rejects(self.collect)

    def test_mixed_tools_and_unknown_capture_entries_rejected(self):
        name = collector.NAMES[1]
        _, raw = self.records[name]
        raw["gnu_linker"]["sha256"] = "f" * 64
        self.persist(name)
        self.rejects(self.collect)
        raw["gnu_linker"]["sha256"] = self.records[collector.NAMES[0]][1]["gnu_linker"]["sha256"]
        self.persist(name)
        (self.capture_dir / "unknown").write_text("not an invocation")
        self.rejects(self.collect)

    def test_validator_rejects_counterfeit_context_hashes_paths_flags_and_inventory(self):
        report = self.collect()
        mutations = [lambda r: r.update(build_source="b" * 40),
                     lambda r: r.update(build_session="00000000-0000-4000-8000-000000000002"),
                     lambda r: r.update(native_link_closure_verified=True),
                     lambda r: r.update(extra="private"),
                     lambda r: r["binary_sha256"].update(treeharbor="f" * 64),
                     lambda r: r["binaries"]["treeharbor"]["output"].update(path="/private/output"),
                     lambda r: r["binaries"]["treeharbor"]["inputs"][0].update(sha256="f" * 64),
                     lambda r: r["binaries"]["treeharbor"].update(inputs=[])]
        for mutation in mutations:
            changed = deepcopy(report)
            mutation(changed)
            self.rejects(lambda: self.validate(changed))

    def test_binary_target_permissions_and_symlink_rejected(self):
        path = self.bin_dir / collector.NAMES[0]
        original = path.read_bytes()
        path.write_bytes(elf("wrong arch", 62))
        self.rejects(self.collect)
        path.write_bytes(original)
        path.chmod(0o600)
        self.rejects(self.collect)
        path.chmod(0o700)
        path.unlink()
        target = self.base / "actual-binary"
        target.write_bytes(original)
        target.chmod(0o700)
        path.symlink_to(target)
        self.rejects(self.collect)

    def test_cli_explicit_options_write_once_and_redact_failure(self):
        output = self.base / "public-sidecar"
        arguments = ["--capture-dir", str(self.capture_dir), "--bin-dir", str(self.bin_dir),
                     "--target", TARGET, "--source-revision", SOURCE, "--build-session", SESSION,
                     "--output-dir", str(output)]
        stdout = io.StringIO()
        with redirect_stdout(stdout):
            self.assertEqual(collector.main(arguments), 0)
        report = json.loads((output / "native-inputs.json").read_bytes())
        self.validate(report)
        result = json.loads(stdout.getvalue())
        self.assertEqual(result["native_inputs_sha256"], hashlib.sha256((output / "native-inputs.json").read_bytes()).hexdigest())
        stderr = io.StringIO()
        with redirect_stderr(stderr), self.assertRaises(SystemExit) as caught:
            collector.main(arguments)
        self.assertEqual(caught.exception.code, 1)
        self.assertNotIn(str(self.base), stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
