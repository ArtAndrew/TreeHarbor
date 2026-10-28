#!/usr/bin/env python3
"""Packaging safety tests. ELF inputs are synthetic; this is not runtime acceptance."""
import contextlib
import importlib.util
import io
import hashlib
import json
import os
from pathlib import Path
import struct
import subprocess
import sys
import tarfile
import tempfile
from types import SimpleNamespace
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("package_release", Path(__file__).with_name("package-release.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
INSTALLER = Path(__file__).with_name("install-release.sh").read_bytes()


def elf(machine=62, interpreter=False):
    data = bytearray(120)
    data[:6] = b"\x7fELF\x02\x01"
    struct.pack_into("<H", data, 18, machine)
    struct.pack_into("<Q", data, 32, 64)
    struct.pack_into("<HH", data, 54, 56, 1)
    struct.pack_into("<I", data, 64, 3 if interpreter else 1)
    return data


class PackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / "source"
        self.root.mkdir()
        self.original = module.ROOT
        module.ROOT = self.root
        self.addCleanup(setattr, module, "ROOT", self.original)
        self.git("init", "-q")
        for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md", "README.md", "SECURITY.md", "Cargo.lock"):
            self.write(name, name.encode())
        self.write("Cargo.toml", b'[workspace.package]\nversion="0.1.0-alpha.1"\n')
        self.write("docs/start.md", b"public instructions\n")
        self.write("third-party/example/LICENSE", b"dependency license\n")
        self.write("rust-toolchain.toml", b'[toolchain]\nchannel="1.99.1"\n')
        notices = []
        for path, component in module.TOOLCHAIN_NOTICES.items():
            content = ("synthetic notice fixture: " + path).encode()
            self.write(path, content)
            notices.append({"path": path, "component": component,
                            "sha256": hashlib.sha256(content).hexdigest()})
        self.write("third-party/toolchain/manifest.json", json.dumps({
            "version": 1, "rust_version": "1.99.1", "rust_source_revision": "ab" * 20,
            "files": notices}).encode())
        self.write("scripts/install-release.sh", INSTALLER)
        self.commit()
        self.bins = self.base / "binaries"
        self.bins.mkdir()
        for name in ("treeharbor", "treeharbor-runtime-init"):
            p = self.bins / name
            p.write_bytes(elf())
            p.chmod(0o755)

    def git(self, *args):
        return subprocess.check_output(["git", "-c", "user.name=Packaging Test", "-c", "user.email=packaging@example.invalid", *args], cwd=self.root, stderr=subprocess.STDOUT)

    def write(self, name, data):
        p = self.root / name
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(data)

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "public fixture")

    def pack(self, output="out"):
        out = self.base / output
        with contextlib.redirect_stdout(io.StringIO()):
            module.package(SimpleNamespace(target="x86_64-unknown-linux-musl", bin_dir=self.bins, output_dir=out))
        return next(out.glob("*.tar.gz"))

    def test_canonical_archive_contains_notices_hashes_and_executable_modes(self):
        first = self.pack()
        self.assertEqual(first.read_bytes(), self.pack("again").read_bytes())
        with tarfile.open(first) as archive:
            names = archive.getnames()
            prefix = names[0].split("/")[0] + "/"
            self.assertIn(prefix + "third-party/example/LICENSE", names)
            for path in module.TOOLCHAIN_NOTICES:
                self.assertIn(prefix + path, names)
            self.assertIn(prefix + "docs/start.md", names)
            self.assertNotIn(prefix + "Cargo.toml", names)
            self.assertEqual(archive.getmember(prefix + "bin/treeharbor").mode, 0o755)
            manifest = json.load(archive.extractfile(prefix + "manifest.json"))
            self.assertEqual(manifest["target"], "x86_64-unknown-linux-musl")
            self.assertEqual(manifest["source_revision"], self.git("rev-parse", "HEAD").decode().strip())
            for name, record in manifest["files"].items():
                self.assertEqual(hashlib.sha256(archive.extractfile(prefix + name).read()).hexdigest(), record["sha256"])
        with self.assertRaises(FileExistsError):
            self.pack()

    def test_committed_missing_or_changed_toolchain_notices_refuse_packaging(self):
        # Commit each fault: the clean-checkout guard must not hide missing or
        # changed notices in an otherwise reviewable source revision.
        path = next(iter(module.TOOLCHAIN_NOTICES))
        original = (self.root / path).read_bytes()
        for content in (None, b"changed upstream notice"):
            if content is None:
                (self.root / path).unlink()
            else:
                self.write(path, content)
            self.commit()
            with self.assertRaises((ValueError, OSError)):
                self.pack()
            self.write(path, original)
        self.write("rust-toolchain.toml", b'[toolchain]\nchannel="1.100.0"\n')
        self.commit()
        with self.assertRaisesRegex(ValueError, "review"):
            self.pack()

    def test_notice_inventory_cannot_omit_or_redirect_required_files(self):
        path = self.root / "third-party/toolchain/manifest.json"
        original = json.loads(path.read_text())
        for fault in ("omit", "redirect", "duplicate"):
            inventory = json.loads(json.dumps(original))
            if fault == "omit":
                inventory["files"].pop()
            elif fault == "redirect":
                inventory["files"][0]["path"] = "../operator-private-key.pem"
            else:
                inventory["files"][0] = inventory["files"][1]
            path.write_text(json.dumps(inventory))
            self.commit()
            with self.assertRaises(ValueError):
                self.pack()

    def test_dependency_observations_bind_the_bytes_inside_the_archive(self):
        observations = {"version": 1, "target": "x86_64-unknown-linux-musl",
                        "packaging_source_revision": self.git("rev-parse", "HEAD").decode().strip(),
                        "binary_sha256": {n: hashlib.sha256((self.bins / n).read_bytes()).hexdigest()
                                          for n in ("treeharbor", "treeharbor-runtime-init")},
                        "rust_sbom_sha256": "ef" * 32,
                        "native_link_closure_verified": False, "build_provenance_verified": False}
        path = self.base / "observations.json"
        path.write_text(json.dumps(observations))
        args = SimpleNamespace(target="x86_64-unknown-linux-musl", bin_dir=self.bins,
                               output_dir=self.base / "observed", input_observations=path)
        with contextlib.redirect_stdout(io.StringIO()):
            module.package(args)
        with tarfile.open(next(args.output_dir.glob("*.tar.gz"))) as archive:
            manifest = json.load(archive.extractfile(next(m for m in archive if m.name.endswith("/manifest.json"))))
            self.assertEqual(manifest["rust_dependency_observations_sha256"], hashlib.sha256(path.read_bytes()).hexdigest())
        for field, value in (("target", "aarch64-unknown-linux-musl"),
                             ("packaging_source_revision", "00" * 20),
                             ("binary_sha256", {}), ("rust_sbom_sha256", 0),
                             ("native_link_closure_verified", True), ("build_provenance_verified", True)):
            invalid = dict(observations, **{field: value})
            path.write_text(json.dumps(invalid))
            args.output_dir = self.base / field
            with self.assertRaisesRegex(ValueError, "do not match"):
                module.package(args)
            self.assertFalse(args.output_dir.exists())
        path.write_text(json.dumps(observations))
        replacement = bytearray((self.bins / "treeharbor").read_bytes())
        replacement[-1] ^= 1  # Still valid ELF; bytes changed after capture.
        (self.bins / "treeharbor").write_bytes(replacement)
        args.output_dir = self.base / "replaced-binary"
        with self.assertRaisesRegex(ValueError, "do not match"):
            module.package(args)
        self.assertFalse(args.output_dir.exists())

    def test_dirty_sources_and_internal_docs_never_enter_archive(self):
        self.write("operator-private-key.pem", b"private test sentinel")
        with self.assertRaisesRegex(ValueError, "clean committed"):
            self.pack()
        (self.root / "operator-private-key.pem").unlink()
        self.write("docs/adr/internal.md", b"internal test sentinel")
        self.commit()
        with self.assertRaisesRegex(ValueError, "internal documentation"):
            self.pack()

    def test_native_input_references_cannot_be_silently_omitted_or_replaced(self):
        report = self.base / "native-inputs.json"
        report.write_text('{}\n')  # Deliberately invalid evidence, not a success fixture.
        observations = {"version": 1, "target": "x86_64-unknown-linux-musl",
                        "packaging_source_revision": self.git("rev-parse", "HEAD").decode().strip(),
                        "binary_sha256": {n: hashlib.sha256((self.bins / n).read_bytes()).hexdigest()
                                          for n in ("treeharbor", "treeharbor-runtime-init")},
                        "rust_sbom_sha256": "ef" * 32, "native_link_closure_verified": False,
                        "build_provenance_verified": False,
                        "native_inputs_sha256": hashlib.sha256(report.read_bytes()).hexdigest(),
                        "build_session": "12345678-1234-4234-8234-123456789abc"}
        path = self.base / "observations.json"
        path.write_text(json.dumps(observations))
        args = SimpleNamespace(target="x86_64-unknown-linux-musl", bin_dir=self.bins,
                               output_dir=self.base / "native", input_observations=path,
                               native_inputs=None)
        with self.assertRaisesRegex(ValueError, "must be supplied"):
            module.package(args)
        self.assertFalse(args.output_dir.exists())
        args.native_inputs = report
        report.write_text('{"changed":true}\n')
        with self.assertRaisesRegex(ValueError, "differs from"):
            module.package(args)
        self.assertFalse(args.output_dir.exists())
        observations.pop("native_inputs_sha256")
        path.write_text(json.dumps(observations))
        with self.assertRaisesRegex(ValueError, "not bound"):
            module.package(args)
        args.input_observations = None
        with self.assertRaisesRegex(ValueError, "require hash-bound"):
            module.package(args)
        self.assertFalse(args.output_dir.exists())

    def test_symlink_wrong_architecture_and_dynamic_interpreter_are_rejected(self):
        p = self.bins / "treeharbor"
        for data in (elf(183), elf(interpreter=True), b"not an ELF"):
            p.write_bytes(data)
            with self.assertRaises(ValueError):
                self.pack()
        p.write_bytes(elf())
        (self.root / "docs/start.md").unlink()
        (self.root / "docs/start.md").symlink_to("../README.md")
        self.commit()
        with self.assertRaisesRegex(ValueError, "not a regular file"):
            self.pack()

    @unittest.skipUnless(os.uname().sysname == "Linux", "installer uses GNU sha256sum on Linux")
    def test_offline_install_refuses_replacement_and_detects_tampering(self):
        archive_path = self.pack()
        extract = self.base / "extracted"
        with tarfile.open(archive_path) as archive:
            archive.extractall(extract, filter="data")
        source = next(extract.iterdir())
        prefix = self.base / "prefix"
        def install():
            return subprocess.run(["sh", str(source / "install.sh"), "--prefix", str(prefix)], capture_output=True)
        self.assertEqual(install().returncode, 0)
        installed = prefix / "bin/treeharbor"
        self.assertTrue(installed.is_symlink())
        self.assertEqual(installed.read_bytes(), elf())
        self.assertNotEqual(install().returncode, 0)
        self.assertEqual(installed.read_bytes(), elf())
        self.assertFalse((prefix / "lib/treeharbor/.install-lock").exists())
        (source / "bin/treeharbor").write_bytes(b"tampered fixture")
        self.assertNotEqual(install().returncode, 0)
        self.assertEqual(installed.read_bytes(), elf())


if __name__ == "__main__":
    unittest.main()
