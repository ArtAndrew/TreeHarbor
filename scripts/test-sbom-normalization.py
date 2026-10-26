#!/usr/bin/env python3
"""Binary identity, declared-source joins and privacy boundaries; no fake build proof."""
from copy import deepcopy
import importlib.util
from pathlib import Path
import sys
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location("normalize_rust_sbom", Path(__file__).with_name("normalize-rust-sbom.py"))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


def fixture():
    report = {"bomFormat": "CycloneDX", "specVersion": "1.7", "version": 1,
              "metadata": {"component": {"bom-ref": "document", "type": "file", "name": "TreeHarbor"}},
              "components": [
                  {"bom-ref": "app", "type": "library", "name": "treeharbor-service", "version": "0.1.0-dev",
                   "properties": [{"name": "syft:package:foundBy", "value": "cargo-auditable-binary-cataloger"},
                                  {"name": "syft:location:0:path", "value": "/treeharbor"}]},
                  {"bom-ref": "crate", "type": "library", "name": "sha2", "version": "0.11.1",
                   "properties": [{"name": "syft:package:foundBy", "value": "cargo-auditable-binary-cataloger"},
                                  {"name": "syft:location:0:path", "value": "/treeharbor"}]},
                  {"bom-ref": "file", "type": "file", "name": "/private/build/treeharbor",
                   "hashes": [{"alg": "SHA-256", "content": "ab" * 32}]}],
              "dependencies": [{"ref": "app", "dependsOn": ["crate"]}]}
    metadata = {"workspace_members": ["workspace-app"], "packages": [
        {"id": "workspace-app", "name": "treeharbor-service", "version": "0.1.0-dev", "source": None, "license": "Elastic-2.0"},
        {"id": "registry-sha2", "name": "sha2", "version": "0.11.1", "source": module.REGISTRY, "license": "MIT OR Apache-2.0"}]}
    lock = {"package": [{"name": "sha2", "version": "0.11.1", "source": module.REGISTRY, "checksum": "cd" * 32}]}
    binaries = {"treeharbor": ("ab" * 32, "/private/build")}
    return report, metadata, lock, binaries


class NormalizationTests(unittest.TestCase):
    def test_exact_binary_hashes_keep_graph_and_gain_declared_licenses_without_build_paths(self):
        report, metadata, lock, binaries = fixture()
        original = deepcopy(report)
        result = module.normalize(report, metadata, lock, binaries)
        self.assertEqual(report, original)
        self.assertEqual(result["dependencies"], report["dependencies"])
        app, crate, file = result["components"]
        self.assertEqual(app["licenses"], [{"expression": "Elastic-2.0"}])
        self.assertNotIn("externalReferences", app)
        self.assertEqual(crate["licenses"], [{"expression": "MIT OR Apache-2.0"}])
        self.assertEqual(crate["externalReferences"][0]["hashes"], [{"alg": "SHA-256", "content": "cd" * 32}])
        self.assertNotIn("hashes", crate)  # Source archive hash is not a compiled-library hash.
        self.assertEqual(file["name"], "bin/treeharbor")
        self.assertEqual(file["hashes"], report["components"][-1]["hashes"])
        self.assertEqual(result["metadata"]["properties"], [{"name": "treeharbor:coverage", "value": module.COVERAGE}])

    def test_source_declarations_must_be_known_unique_and_locked(self):
        for case in ("unknown", "ambiguous", "license", "registry", "path", "checksum", "checksum_format"):
            with self.subTest(case=case):
                report, metadata, lock, binaries = fixture()
                if case == "unknown":
                    metadata["packages"].pop()
                elif case == "ambiguous":
                    metadata["packages"].append(deepcopy(metadata["packages"][-1]))
                elif case == "license":
                    metadata["packages"][-1]["license"] = None
                elif case == "registry":
                    metadata["packages"][-1]["source"] = "registry+https://example.invalid/index"
                elif case == "path":
                    metadata["workspace_members"] = []
                elif case == "checksum":
                    lock["package"] = []
                else:
                    lock["package"][0]["checksum"] = "not-a-source-hash"
                with self.assertRaises(ValueError):
                    module.normalize(report, metadata, lock, binaries)

    def test_foreign_binary_cataloger_path_or_dangling_graph_cannot_pass(self):
        for case in ("hash", "missing_binary", "duplicate", "cataloger", "location", "private_path", "dangling", "filename", "version"):
            with self.subTest(case=case):
                report, metadata, lock, binaries = fixture()
                if case == "hash":
                    report["components"][-1]["hashes"][0]["content"] = "ef" * 32
                elif case == "missing_binary":
                    binaries["treeharbor-runtime-init"] = ("ef" * 32, "/private/build")
                elif case == "duplicate":
                    report["components"].append(deepcopy(report["components"][0]))
                elif case == "cataloger":
                    report["components"][0]["properties"][0]["value"] = "rust-cargo-lock-cataloger"
                elif case == "location":
                    report["components"][0]["properties"][1]["value"] = "/foreign-binary"
                elif case == "private_path":
                    report["metadata"]["component"]["description"] = "/private/build/should-not-publish"
                elif case == "dangling":
                    report["dependencies"][0]["dependsOn"] = ["unknown"]
                elif case == "filename":
                    report["components"][-1]["name"] = "/private/build/foreign-binary"
                else:
                    report["specVersion"] = "1.6"
                with self.assertRaises(ValueError):
                    module.normalize(report, metadata, lock, binaries)

    def test_no_embedded_graph_never_becomes_a_source_lockfile_sbom(self):
        report, metadata, lock, binaries = fixture()
        report["components"] = [report["components"][-1]]
        report["dependencies"] = []
        with self.assertRaisesRegex(ValueError, "embedded crate graph"):
            module.normalize(report, metadata, lock, binaries)
        report["components"] = []  # Actual scanner result for an ordinary binary.
        with self.assertRaises(ValueError):
            module.normalize(report, metadata, lock, binaries)

    def test_legacy_license_declaration_is_retained_without_guessing_spdx_semantics(self):
        report, metadata, lock, binaries = fixture()
        metadata["packages"][-1]["license"] = "MIT/Apache-2.0"
        result = module.normalize(report, metadata, lock, binaries)
        self.assertEqual(result["components"][1]["licenses"], [{"license": {"name": "MIT/Apache-2.0"}}])


if __name__ == "__main__":
    unittest.main()
