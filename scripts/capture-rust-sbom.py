#!/usr/bin/env python3
"""Capture binary-derived Cargo coverage and observed toolchain inputs, not provenance."""
import argparse
import hashlib
import importlib.util
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import tomllib

sys.dont_write_bytecode = True


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(filename))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


package = load("package_release", "package-release.py")
sbom = load("normalize_rust_sbom", "normalize-rust-sbom.py")
ROOT = Path(__file__).resolve().parents[1]
NAMES = ("treeharbor", "treeharbor-runtime-init")


def run(*args, cwd=ROOT):
    result = subprocess.run([str(x) for x in args], cwd=cwd, capture_output=True,
                            timeout=180, check=True)
    if len(result.stdout) > 16 * 1024 * 1024:
        raise ValueError("tool output exceeds limit")
    return result.stdout


def capture(args):
    if args.output_dir.is_relative_to(ROOT):
        raise ValueError("output directory must be outside the source checkout")
    tool = shutil.which("cargo-auditable")
    if not tool:
        raise ValueError("cargo-auditable build tool is not available")
    tool_hashes = {"cargo-auditable": sbom.sha256(Path(tool)), "syft": sbom.sha256(args.syft)}
    revision = run("git", "rev-parse", "HEAD").decode().strip()
    if run("git", "status", "--porcelain", "--untracked-files=normal").strip():
        raise ValueError("capture from a clean committed checkout")
    manifest = json.loads((ROOT / "third-party/toolchain/manifest.json").read_text())
    compiler = {}
    for line in run("rustc", "-Vv").decode().splitlines():
        if ": " in line:
            key, value = line.split(": ", 1)
            compiler[key] = value
    if (compiler.get("release") != manifest["rust_version"]
            or compiler.get("commit-hash") != manifest["rust_source_revision"]):
        raise ValueError("observed compiler differs from reviewed toolchain")
    libdir = Path(run("rustc", "--print", "target-libdir", "--target", args.target).decode().strip())
    if not libdir.is_dir():
        raise ValueError("target standard library is not installed")
    inputs = {}
    for path in sorted(libdir.rglob("*")):
        if path.is_symlink():
            raise ValueError("unexpected symlink in target sysroot")
        if path.is_file() and path.suffix in (".rlib", ".rmeta", ".a", ".o"):
            inputs[path.relative_to(libdir).as_posix()] = sbom.sha256(path)
    if not inputs or not any(n.startswith("libstd-") for n in inputs):
        raise ValueError("target sysroot has no standard library")
    metadata = json.loads(run("cargo", "metadata", "--format-version", "1", "--locked"))
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    args.output_dir.mkdir(exist_ok=False)
    with tempfile.TemporaryDirectory(prefix="treeharbor-sbom-") as temporary:
        temporary = Path(temporary)
        stage = temporary / "bin"
        stage.mkdir()
        hashes = {}
        for name in NAMES:
            data = package.binary(args.bin_dir / name, package.TARGETS[args.target])
            (stage / name).write_bytes(data)
            hashes[name] = hashlib.sha256(data).hexdigest()
        config = temporary / "syft.yaml"
        config.write_text('check-for-app-update: false\ncache:\n  dir: ""\n  ttl: 0s\n')
        report = json.loads(run(args.syft, "scan", "dir:.", "--config", config,
                                "--override-default-catalogers", "cargo-auditable-binary-cataloger",
                                "--source-name", "TreeHarbor", "--source-version",
                                tomllib.loads((ROOT / "Cargo.toml").read_text())["workspace"]["package"]["version"],
                                "--output", "cyclonedx-json", cwd=stage))
        result = sbom.normalize(report, metadata, lock,
                                {n: (hashes[n], str(stage)) for n in NAMES})
        result["metadata"]["properties"].extend([
            {"name": "treeharbor:target", "value": args.target},
            {"name": "treeharbor:packaging-source-revision", "value": revision}])
    if (run("git", "rev-parse", "HEAD").decode().strip() != revision
            or run("git", "status", "--porcelain", "--untracked-files=normal").strip()):
        raise ValueError("source changed during capture")
    # Detect mutable prebuilt files being replaced while scanning their copies.
    if any(sbom.sha256(args.bin_dir / n) != hashes[n] for n in NAMES):
        raise ValueError("binary input changed during capture")
    observations = {"version": 1, "target": args.target, "packaging_source_revision": revision,
                    "binary_sha256": hashes, "cargo_lock_sha256": sbom.sha256(ROOT / "Cargo.lock"),
                    "observed_rustc": compiler, "available_sysroot_inputs": inputs,
                    "observed_tool_sha256": tool_hashes,
                    "coverage": sbom.COVERAGE, "native_link_closure_verified": False,
                    "build_provenance_verified": False}
    sums = []
    for name, value in (("rust-dependencies.cdx.json", result), ("input-observations.json", observations)):
        data = (json.dumps(value, sort_keys=True, indent=2, ensure_ascii=False) + "\n").encode()
        with (args.output_dir / name).open("xb") as output:
            output.write(data)
        sums.append(f"{hashlib.sha256(data).hexdigest()}  {name}\n")
    with (args.output_dir / "SHA256SUMS").open("x") as output:
        output.writelines(sums)
    print(json.dumps({"target": args.target, "binary_sha256": hashes,
                      "coverage": sbom.COVERAGE, "build_provenance_verified": False}))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=package.TARGETS)
    parser.add_argument("--bin-dir", required=True, type=Path)
    parser.add_argument("--syft", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    args = parser.parse_args()
    args.bin_dir = args.bin_dir.absolute()
    args.syft = args.syft.absolute()
    args.output_dir = args.output_dir.resolve()
    try:
        capture(args)
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        # Upstream stderr can contain source/cache paths. Keep it out of reports.
        if isinstance(error, subprocess.CalledProcessError):
            message = f"{Path(error.cmd[0]).name} exited with status {error.returncode}"
        elif isinstance(error, subprocess.TimeoutExpired):
            message = f"{Path(error.cmd[0]).name} exceeded its deadline"
        else:
            message = str(error)
        parser.exit(1, f"Rust SBOM capture failed: {message}\n")


if __name__ == "__main__":
    main()
