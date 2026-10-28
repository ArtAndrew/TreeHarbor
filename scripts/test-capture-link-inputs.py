#!/usr/bin/env python3
"""Link-capture boundary tests. Fake links never establish native inventory.

Copyright 2026 Artishchev Andrey Borisovich. Licensed under Elastic License 2.0.
"""
from concurrent.futures import ThreadPoolExecutor
from contextlib import redirect_stderr, redirect_stdout
from copy import deepcopy
import hashlib
import importlib.util
import io
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
SCRIPT = Path(__file__).with_name("capture-link-inputs.py")
SPEC = importlib.util.spec_from_file_location("capture_link_inputs", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = MODULE
SPEC.loader.exec_module(MODULE)
BUILD_SOURCE = "0123456789abcdef0123456789abcdef01234567"
BUILD_SESSION = "e0b7425a-863a-4e09-99a0-e8f4bdbb9b0b"


def elf(machine=62):
    data = bytearray(64)
    data[:6] = b"\x7fELF\x02\x01"
    data[18:20] = machine.to_bytes(2, "little")
    return bytes(data) + b"compiler-produced-fixture-output"


def depfile(output, inputs):
    return (str(output) + ": \\\n" + " \\\n".join(str(p) for p in inputs) + "\n\n"
            + "".join(str(p) + ":\n\n" for p in inputs)).encode()


class Fixture:
    def __init__(self, base):
        self.base = base
        self.repo = base / "repo"
        self.target = self.repo / "target"
        self.cargo = base / "cargo"
        self.sysroot = base / "rust-sysroot"
        self.capture = base / "private-captures"
        self.tools = base / "private-tools"
        for path in (self.repo, self.target, self.cargo, self.sysroot, self.tools):
            path.mkdir(parents=True, exist_ok=True)
        self.capture.mkdir(mode=0o700)
        self.compiler, self.linker = self.tools / "real-cc", self.tools / "ld.bfd"
        for path in (self.compiler, self.linker):
            path.write_bytes(b"fake tool identity: " + path.name.encode())
            path.chmod(0o700)
        self.inputs = [self.repo / "native.a", self.target / "tmp/rustc-ephemeral/symbols.o",
                       self.cargo / "registry/src/fixture-1.0/native.a", self.sysroot / "lib/libstd-fixture.rlib"]
        for index, path in enumerate(self.inputs):
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(("input bytes " + str(index)).encode())
        self.env = {"TH_LINK_CAPTURE_DIR": str(self.capture), "TH_LINK_REAL_CC": str(self.compiler),
                    "TH_LINK_BUILD_SOURCE": BUILD_SOURCE, "TH_LINK_BUILD_SESSION": BUILD_SESSION,
                    "TH_LINK_TARGET": "x86_64-unknown-linux-musl", "TH_LINK_REPO_ROOT": str(self.repo),
                    "TH_LINK_TARGET_ROOT": str(self.target), "TH_LINK_CARGO_ROOT": str(self.cargo),
                    "TH_LINK_SYSROOT_ROOT": str(self.sysroot), "PATH": os.environ.get("PATH", os.defpath)}
        self.config = MODULE.Config.from_environment(self.env, self.repo)

    def args(self, label="product"):
        return ["-Wl,--as-needed", "-L", str(self.sysroot / "lib"), "-Wl,-Bstatic",
                *map(str, self.inputs), "-lc", "-nostartfiles", "-Wl,-z,relro,-z,now",
                "-o", str(self.target / label), "-static", "-nodefaultlibs"]

    def run(self, fake, args=None):
        return MODULE.capture_link(self.config, self.args() if args is None else args, self.env,
                                   process=fake, system="Linux", machine="x86_64")

    def records(self):
        return sorted(self.capture.glob("invocation-*"))


class FakeCompiler:
    def __init__(self, fixture, code=0, version=b"GNU ld (GNU Binutils) 2.42\n", after_link=None):
        self.f = fixture
        self.code = code
        self.version = version
        self.after_link = after_link
        self.calls = []
        self.environments = []
        self.written = None

    def __call__(self, command, *, cwd, env, capture_output=False):
        self.calls.append((list(command), capture_output))
        self.environments.append(dict(env))
        if command == [str(self.f.compiler), "-print-prog-name=ld"]:
            return subprocess.CompletedProcess(command, 0, (str(self.f.linker) + "\n").encode(), b"")
        if command == [str(self.f.linker), "--version"]:
            return subprocess.CompletedProcess(command, 0, self.version, b"")
        self.written = Path(command[command.index("-o") + 1])
        self.written.write_bytes(elf() if self.code == 0 else b"partial compiler output")
        if self.code == 0:
            path, = [a.split("=", 1)[1] for a in command if a.startswith("-Wl,--dependency-file=")]
            Path(path).write_bytes(depfile(self.written, self.f.inputs))
            if self.after_link:
                self.after_link(command, Path(path))
        return subprocess.CompletedProcess(command, self.code)


class CaptureTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="th-link-capture-test-")
        self.addCleanup(self.temporary.cleanup)
        self.base = Path(self.temporary.name).resolve()
        self.serial = 0

    def fixture(self):
        self.serial += 1
        base = self.base / str(self.serial)
        base.mkdir()
        return Fixture(base)

    def test_success_forwards_arguments_and_captures_private_inputs_without_rewriting_output(self):
        fixture = self.fixture()
        fake = FakeCompiler(fixture)
        arguments = fixture.args()
        self.assertEqual(fixture.run(fake, arguments), 0)
        self.assertEqual(fake.calls[-1][0][:-1], [str(fixture.compiler), *arguments])
        self.assertTrue(fake.calls[-1][0][-1].startswith("-Wl,--dependency-file="))
        self.assertEqual(fake.written.read_bytes(), elf())
        directory, = fixture.records()
        self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o700)
        for name in ("invocation.json", "capture.json", "normalized.json"):
            self.assertEqual(stat.S_IMODE((directory / name).stat().st_mode), 0o600)
        raw = json.loads((directory / "capture.json").read_text())
        public = json.loads((directory / "normalized.json").read_text())
        self.assertEqual(raw["arguments"], arguments)
        self.assertEqual((raw["build_source"], raw["build_session"]), (BUILD_SOURCE, BUILD_SESSION))
        self.assertEqual((public["build_source"], public["build_session"]), (BUILD_SOURCE, BUILD_SESSION))
        self.assertEqual(raw["output"]["sha256"], hashlib.sha256(elf()).hexdigest())
        self.assertEqual({p["path"]: p["sha256"] for p in raw["inputs"]},
                         {str(p.resolve()): hashlib.sha256(p.read_bytes()).hexdigest() for p in fixture.inputs})
        self.assertEqual({p["root"] for p in public["inputs"]}, set(MODULE.ROOT_KINDS))
        self.assertEqual(public["output"], {"root": "target", "path": "product", "sha256": raw["output"]["sha256"], "bytes": len(elf())})
        self.assertNotIn(str(self.base), json.dumps(public))
        self.assertNotIn("arguments", public)
        for claim in ("source_provenance_verified", "build_provenance_verified", "native_component_inventory_complete", "archive_member_attribution_complete"):
            self.assertIs(public[claim], False)
        # rustc may delete its symbols/objects immediately after wrapper return.
        for path in fixture.inputs:
            path.unlink()
        self.assertEqual(MODULE.normalize_record(raw), public)

    def test_observed_context_is_required_and_normalization_binds_expected_build(self):
        fixture = self.fixture()
        for key, values in (("TH_LINK_BUILD_SOURCE", (None, "f" * 39, "g" * 40, "A" * 40)),
                            ("TH_LINK_BUILD_SESSION", (None, "", "not-a-uuid", "00000000-0000-0000-0000-000000000000",
                                                       BUILD_SESSION.upper(), "{" + BUILD_SESSION + "}"))):
            for value in values:
                with self.subTest(key=key, value=value), self.assertRaises(MODULE.CaptureError):
                    MODULE.Config.from_environment(dict(fixture.env, **{key: value}), fixture.repo)
            missing = dict(fixture.env)
            del missing[key]
            with self.assertRaises(KeyError):
                MODULE.Config.from_environment(missing, fixture.repo)
        for key, value in (("TH_LINK_BUILD_SOURCE", "f" * 40),
                           ("TH_LINK_BUILD_SESSION", "f87ce609-f7fc-4f1e-93e1-4b1183f38aac")):
            with self.assertRaises(MODULE.CaptureError):
                MODULE.capture_link(fixture.config, fixture.args(), dict(fixture.env, **{key: value}),
                                    process=FakeCompiler(fixture), system="Linux", machine="x86_64")
        self.assertEqual(fixture.records(), [])
        self.assertEqual(fixture.run(FakeCompiler(fixture)), 0)
        directory, = fixture.records()
        raw = MODULE.read_private_capture(directory / "capture.json", expected_source=BUILD_SOURCE,
                                          expected_session=BUILD_SESSION, expected_target=fixture.config.target)
        public = MODULE.normalize_capture(raw, expected_source=BUILD_SOURCE, expected_session=BUILD_SESSION,
                                          expected_target=fixture.config.target)
        self.assertEqual(public, json.loads((directory / "normalized.json").read_text()))
        for field, values in (("build_source", (None, False, "f" * 39, "G" * 40)),
                              ("build_session", (None, False, "", "not-a-uuid", "00000000-0000-0000-0000-000000000000"))):
            for value in values:
                forged = deepcopy(raw)
                forged[field] = value
                with self.subTest(field=field, value=value), self.assertRaises(MODULE.CaptureError):
                    MODULE.normalize_capture(forged)
            missing = deepcopy(raw)
            del missing[field]
            with self.assertRaises(MODULE.CaptureError):
                MODULE.normalize_capture(missing)
        for expected in ({"expected_source": "f" * 40}, {"expected_source": "bad"},
                         {"expected_session": "f87ce609-f7fc-4f1e-93e1-4b1183f38aac"},
                         {"expected_session": "00000000-0000-0000-0000-000000000000"},
                         {"expected_target": "aarch64-unknown-linux-musl"}, {"expected_target": "unknown"}):
            with self.subTest(expected=expected), self.assertRaises(MODULE.CaptureError):
                MODULE.normalize_capture(raw, **expected)
            with self.assertRaises(MODULE.CaptureError):
                MODULE.read_private_capture(directory / "capture.json", **expected)

    def test_private_reader_rejects_ambiguous_json_and_unknown_record_shapes(self):
        fixture = self.fixture()
        self.assertEqual(fixture.run(FakeCompiler(fixture)), 0)
        directory, = fixture.records()
        raw = json.loads((directory / "capture.json").read_text())
        for value in (dict(raw, unreviewed_field=True), dict(raw, arguments="not-an-array"),
                      dict(raw, output=dict(raw["output"], extra="not-reviewed")), dict(raw, target=[])):
            path = fixture.capture / "invalid.json"
            path.write_text(json.dumps(value))
            with self.assertRaises(MODULE.CaptureError):
                MODULE.read_private_capture(path)
        for text in ('{"version":1,"version":1}', '{"value":NaN}'):
            path.write_text(text)
            with self.assertRaises(MODULE.CaptureError):
                MODULE.read_private_capture(path)
        link = fixture.capture / "alias.json"
        link.symlink_to(directory / "capture.json")
        with self.assertRaises(OSError):
            MODULE.read_private_capture(link)
        # Raw capture normalization remains possible after ephemeral input cleanup.
        for file in fixture.inputs:
            file.unlink()
        self.assertEqual(MODULE.read_private_capture(directory / "capture.json"), raw)

    def test_pinned_cargo_auditable_flag_is_forwarded_and_audit_object_is_hashed(self):
        fixture = self.fixture()
        audit_object = fixture.target / "treeharbor_audit_data.o"
        audit_object.write_bytes(b"injected compressed Cargo dependency fixture")
        fixture.inputs.append(audit_object)
        arguments = fixture.args() + ["-Wl,-u,AUDITABLE_VERSION_INFO"]
        fake = FakeCompiler(fixture)
        self.assertEqual(fixture.run(fake, arguments), 0)
        self.assertEqual(fake.calls[-1][0][:-1], [str(fixture.compiler), *arguments])
        directory, = fixture.records()
        raw = MODULE.read_private_capture(directory / "capture.json")
        self.assertIn({"path": str(audit_object), "sha256": hashlib.sha256(audit_object.read_bytes()).hexdigest(),
                       "bytes": audit_object.stat().st_size}, raw["inputs"])
        for unsupported in ("-Wl,-u", "-Wl,-u,OTHER_SYMBOL", "-Wl,--undefined=AUDITABLE_VERSION_INFO"):
            with self.assertRaises(MODULE.CaptureError):
                fixture.run(FakeCompiler(fixture), fixture.args() + [unsupported])

    def test_nonzero_exit_and_signal_are_preserved_with_partial_output_untouched(self):
        for code in (1, 2, 23, 127, -signal.SIGTERM):
            fixture = self.fixture()
            fake = FakeCompiler(fixture, code=code)
            self.assertEqual(fixture.run(fake), code)
            self.assertEqual(fake.written.read_bytes(), b"partial compiler output")
            directory, = fixture.records()
            raw = json.loads((directory / "capture.json").read_text())
            self.assertEqual(raw["compiler_returncode"], code)
            self.assertEqual(raw["status"], "compiler_failed")
            self.assertNotIn("inputs", raw)
            self.assertFalse((directory / "normalized.json").exists())

    def test_failing_compiler_exit_survives_an_unwritable_evidence_record(self):
        original = MODULE.durable_json
        for code in (23, -signal.SIGTERM):
            fixture = self.fixture()
            fake = FakeCompiler(fixture, code=code)
            def no_failure_record(directory, name, value):
                if name == "capture.json":
                    raise OSError("simulated evidence disk full")
                return original(directory, name, value)
            with patch.object(MODULE, "durable_json", side_effect=no_failure_record):
                self.assertEqual(fixture.run(fake), code)
            self.assertEqual(fake.written.read_bytes(), b"partial compiler output")
            directory, = fixture.records()
            self.assertTrue((directory / "invocation.json").exists())
            self.assertFalse((directory / "normalized.json").exists())

    def test_reviewed_cargo_library_directories_are_cleared_only_for_gnu_children(self):
        for code in (0, 23, -signal.SIGTERM):
            fixture = self.fixture()
            rust_lib = fixture.sysroot / "lib"
            target_deps = fixture.target / "release/deps"
            target_deps.mkdir(parents=True)
            value = str(rust_lib) + ":" + str(target_deps)
            fixture.env["LD_LIBRARY_PATH"] = value
            fixture.config = MODULE.Config.from_environment(fixture.env, fixture.repo)
            original = dict(fixture.env)
            fake = FakeCompiler(fixture, code=code)
            self.assertEqual(fixture.run(fake), code)
            self.assertEqual(fixture.env, original)
            self.assertEqual(len(fake.environments), 3)
            for child in fake.environments:
                self.assertNotIn("LD_LIBRARY_PATH", child)
                self.assertEqual(child, {key: item for key, item in original.items() if key != "LD_LIBRARY_PATH"})
        fixture = self.fixture()
        original = dict(fixture.env)
        fake = FakeCompiler(fixture)
        self.assertEqual(fixture.run(fake), 0)
        self.assertEqual(fake.environments, [original] * 3)

    def test_unreviewed_cargo_library_directories_and_other_overrides_are_rejected(self):
        fixture = self.fixture()
        allowed = str(fixture.sysroot / "lib")
        foreign = fixture.base / "foreign-libraries"
        foreign.mkdir()
        alias = fixture.target / "escaped-libraries"
        alias.symlink_to(foreign, target_is_directory=True)
        internal_alias = fixture.target / "aliased-rust-libraries"
        internal_alias.symlink_to(fixture.sysroot / "lib", target_is_directory=True)
        invalid = ("", ":", allowed + ":", ":" + allowed, allowed + "::" + allowed,
                   str(foreign), str(fixture.cargo), str(fixture.repo), "relative/lib", "/unset-library-directory",
                   str(fixture.sysroot / "missing"), str(fixture.inputs[0]), str(alias), str(internal_alias),
                   str(fixture.target) + "/../target", allowed + "/", allowed + "/.", None)
        for value in invalid:
            env = dict(fixture.env, LD_LIBRARY_PATH=value)
            with self.subTest(value=value), self.assertRaises((MODULE.CaptureError, OSError)):
                MODULE.Config.from_environment(env, fixture.repo)
            fake = FakeCompiler(fixture)
            with self.assertRaises((MODULE.CaptureError, OSError)):
                MODULE.capture_link(fixture.config, fixture.args(), env, process=fake,
                                    system="Linux", machine="x86_64")
            self.assertEqual(fake.calls, [])
            self.assertEqual(fixture.records(), [])
        for key in ("COLLECT_LD", "COMPILER_PATH", "GCC_EXEC_PREFIX", "LD_PRELOAD"):
            env = dict(fixture.env, LD_LIBRARY_PATH=allowed, **{key: "unreviewed-override"})
            with self.subTest(key=key), self.assertRaises(MODULE.CaptureError):
                MODULE.Config.from_environment(env, fixture.repo)
            fake = FakeCompiler(fixture)
            with self.assertRaises(MODULE.CaptureError):
                MODULE.capture_link(fixture.config, fixture.args(), env, process=fake,
                                    system="Linux", machine="x86_64")
            self.assertEqual(fake.calls, [])

    def test_successful_link_with_missing_ephemeral_input_or_depfile_fails_capture(self):
        for case in ("ephemeral", "depfile", "symlink-depfile", "tool-change", "wrong-architecture"):
            fixture = self.fixture()
            def change(command, dependencies):
                if case == "ephemeral":
                    fixture.inputs[1].unlink()
                elif case == "depfile":
                    dependencies.unlink()
                elif case == "symlink-depfile":
                    other = fixture.target / "untrusted-dependencies.d"
                    dependencies.rename(other)
                    dependencies.symlink_to(other)
                elif case == "tool-change":
                    fixture.compiler.write_bytes(b"replaced compiler identity")
                else:
                    output = Path(command[command.index("-o") + 1])
                    output.write_bytes(elf(183))
            fake = FakeCompiler(fixture, after_link=change)
            self.assertEqual(fixture.run(fake), 2)
            directory, = fixture.records()
            raw = json.loads((directory / "capture.json").read_text())
            self.assertEqual(raw["status"], "capture_failed")
            self.assertEqual(raw["compiler_returncode"], 0)
            self.assertFalse((directory / "normalized.json").exists())
            self.assertEqual(fake.written.read_bytes(), elf(183) if case == "wrong-architecture" else elf())

    def test_unsupported_compiler_and_linker_arguments_are_not_forwarded(self):
        for extra in (["@response"], ["-Wl,@response"], ["-Xlinker", "@response"],
                      ["-fuse-ld=lld"], ["-B/usr/other"], ["--sysroot=/elsewhere"],
                      ["-Wl,--dependency-file=other.d"], ["-Wl,-o,other"], ["-unknown-flag"],
                      ["-o", "other"], ["-Wl,-z,unknown"], ["-Wl,--plugin=unknown"]):
            fixture = self.fixture()
            fake = FakeCompiler(fixture)
            with self.assertRaises(MODULE.CaptureError):
                fixture.run(fake, fixture.args() + extra)
            self.assertEqual(fake.calls, [])
            self.assertEqual(fixture.records(), [])
        fixture = self.fixture()
        source = fixture.repo / "compile-me.c"
        source.write_text("int main(void) { return 0; }")
        with self.assertRaises(MODULE.CaptureError):
            fixture.run(FakeCompiler(fixture), fixture.args() + [str(source)])

    def test_non_linux_cross_target_and_non_gnu_ld_are_explicitly_unsupported(self):
        fixture = self.fixture()
        for system, machine in (("Darwin", "x86_64"), ("Linux", "aarch64")):
            fake = FakeCompiler(fixture)
            with self.assertRaises(MODULE.CaptureError):
                MODULE.capture_link(fixture.config, fixture.args(), fixture.env, fake, system, machine)
            self.assertEqual(fake.calls, [])
        for banner in (b"LLD 19.0\n", b"GNU gold (GNU Binutils) 2.42\n", b"not-a-linker\n"):
            fixture = self.fixture()
            fake = FakeCompiler(fixture, version=banner)
            self.assertEqual(fixture.run(fake), 2)
            self.assertEqual(len(fake.calls), 2)
            self.assertFalse((fixture.target / "product").exists())

    def test_dependency_parser_accepts_the_restricted_make_rule_and_rejects_other_rules(self):
        fixture = self.fixture()
        output = fixture.target / "program"
        expected = [p.resolve() for p in fixture.inputs]
        canonical = depfile(output, fixture.inputs)
        self.assertEqual(MODULE.dependencies(canonical, output, fixture.repo), expected)
        relative = Path("target/program")
        self.assertEqual(MODULE.dependencies(depfile(relative, fixture.inputs), output, fixture.repo), expected)
        cases = [canonical.replace(b"symbols.o", b"symbol\\ file.o"),
                 canonical.replace(b"symbols.o", b"$(INPUT)"), canonical + b"foreign: input\n",
                 canonical + b"foreign:\n", canonical.replace(b"\n", b"\r\n"),
                 canonical.replace(str(output).encode(), b"/wrong-output", 1),
                 canonical.split(b"\n\n", 1)[0] + b"\n", canonical + b"x:y:z\n",
                 b"no-colon\n", depfile(output, [fixture.inputs[0], fixture.inputs[0]])]
        for data in cases:
            with self.subTest(data=data[:40]), self.assertRaises((MODULE.CaptureError, OSError)):
                MODULE.dependencies(data, output, fixture.repo)

    def test_unclassified_or_symlink_escaped_inputs_stay_private_and_fail_public_normalization(self):
        for via_symlink in (False, True):
            fixture = self.fixture()
            outside = fixture.base / "operator-private/native-secret.a"
            outside.parent.mkdir()
            outside.write_bytes(b"unknown native bytes")
            if via_symlink:
                alias = fixture.repo / "alias.a"
                alias.symlink_to(outside)
                fixture.inputs.append(alias)
            else:
                fixture.inputs.append(outside)
            self.assertEqual(fixture.run(FakeCompiler(fixture)), 2)
            directory, = fixture.records()
            raw = json.loads((directory / "capture.json").read_text())
            self.assertEqual(raw["status"], "private_link_inputs_captured")
            self.assertIn(str(outside), [p["path"] for p in raw["inputs"]])
            self.assertTrue((directory / "normalization-error.json").exists())
            self.assertFalse((directory / "normalized.json").exists())
            stdout, stderr = io.StringIO(), io.StringIO()
            with redirect_stdout(stdout), redirect_stderr(stderr):
                self.assertEqual(MODULE.main(["normalize", str(directory / "capture.json")], env={}), 2)
            self.assertEqual(stdout.getvalue(), "")
            self.assertNotIn(str(outside), stderr.getvalue())
            self.assertNotIn(str(fixture.base), stderr.getvalue())

    def test_private_config_and_output_boundaries_are_checked(self):
        fixture = self.fixture()
        for key, value in (("TH_LINK_REAL_CC", "relative-cc"),
                           ("TH_LINK_REPO_ROOT", "/"),
                           ("TH_LINK_TARGET_ROOT", str(fixture.repo)),
                           ("COLLECT_LD", "unreviewed-override")):
            with self.subTest(key=key), self.assertRaises(MODULE.CaptureError):
                MODULE.Config.from_environment(dict(fixture.env, **{key: value}), fixture.repo)
        fixture.capture.chmod(0o755)
        with self.assertRaises(MODULE.CaptureError):
            MODULE.Config.from_environment(fixture.env, fixture.repo)
        fixture.capture.chmod(0o700)
        arguments = fixture.args()
        arguments[arguments.index("-o") + 1] = str(fixture.base / "outside-output")
        with self.assertRaises(MODULE.CaptureError):
            fixture.run(FakeCompiler(fixture), arguments)
        protected = fixture.base / "protected"
        protected.write_bytes(b"unchanged")
        (fixture.target / "product").symlink_to(protected)
        with self.assertRaises(MODULE.CaptureError):
            fixture.run(FakeCompiler(fixture))
        self.assertEqual(protected.read_bytes(), b"unchanged")

    def test_raw_false_claims_cannot_be_promoted_and_longest_root_is_used(self):
        fixture = self.fixture()
        self.assertEqual(fixture.run(FakeCompiler(fixture)), 0)
        directory, = fixture.records()
        raw = json.loads((directory / "capture.json").read_text())
        for claim in ("source_provenance_verified", "build_provenance_verified", "native_component_inventory_complete", "archive_member_attribution_complete"):
            forged = deepcopy(raw)
            forged[claim] = True
            with self.assertRaises(MODULE.CaptureError):
                MODULE.normalize_record(forged)
        for key, value in (("version", True), ("compiler_returncode", False)):
            forged = deepcopy(raw)
            forged[key] = value
            with self.assertRaises(MODULE.CaptureError):
                MODULE.normalize_record(forged)
        normalized = MODULE.normalize_record(raw)
        self.assertEqual(next(p for p in normalized["inputs"] if p["path"].endswith("symbols.o"))["root"], "target")

    def test_concurrent_links_keep_distinct_durable_records(self):
        fixture = self.fixture()
        def one(index):
            return fixture.run(FakeCompiler(fixture), fixture.args("output-" + str(index)))
        with ThreadPoolExecutor(max_workers=3) as pool:
            self.assertEqual(list(pool.map(one, range(3))), [0, 0, 0])
        directories = fixture.records()
        self.assertEqual(len(directories), 3)
        self.assertEqual(len({json.loads((d / "capture.json").read_text())["output"]["path"] for d in directories}), 3)

    @unittest.skipUnless(platform.system() == "Linux", "actual GNU compiler fixture requires Linux; Darwin GNU profile is unsupported")
    def test_actual_gnu_cc_small_object_link_without_execution(self):
        compiler = shutil.which("cc")
        if not compiler or platform.machine() not in ("x86_64", "aarch64"):
            self.skipTest("native Linux compiler/profile unavailable")
        fixture = self.fixture()
        env = {**os.environ, **fixture.env,
               "TH_LINK_REAL_CC": str(Path(compiler).resolve()),
               "TH_LINK_TARGET": platform.machine() + "-unknown-linux-musl",
               "TH_LINK_SYSROOT_ROOT": "/usr"}
        config = MODULE.Config.from_environment(env, fixture.repo)
        try:
            MODULE.gnu_profile(config, env, MODULE.run_process)
        except MODULE.CaptureError:
            self.skipTest("selected cc does not use the supported GNU ld profile")
        source, obj = fixture.repo / "tiny.c", fixture.target / "tiny.o"
        # This freestanding fixture proves GNU capture/ELF architecture, not musl ABI.
        source.write_text("void _start(void) { for (;;) {} }\n")
        subprocess.run([compiler, "-ffreestanding", "-fno-stack-protector", "-c", str(source), "-o", str(obj)], check=True, timeout=15)
        output = fixture.target / "actual-fixture"
        self.assertEqual(MODULE.capture_link(config, [str(obj), "-nostdlib", "-static", "-o", str(output)], env), 0)
        directory, = fixture.records()
        raw = json.loads((directory / "capture.json").read_text())
        self.assertEqual(raw["output"]["sha256"], hashlib.sha256(output.read_bytes()).hexdigest())
        self.assertIn(str(obj), [p["path"] for p in raw["inputs"]])


if __name__ == "__main__":
    unittest.main(verbosity=2)
