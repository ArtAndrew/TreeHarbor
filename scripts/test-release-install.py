#!/usr/bin/env python3
"""Install and execute an actual native release archive on a disposable Linux host."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    args = parser.parse_args()
    if os.uname().sysname != "Linux":
        parser.error("run this acceptance check on Linux")
    with tempfile.TemporaryDirectory(prefix="th-release-install-") as temp:
        root = Path(temp)
        with tarfile.open(args.archive) as archive:
            archive.extractall(root / "extract", filter="data")
        source, = (root / "extract").iterdir()
        manifest = json.loads((source / "manifest.json").read_text())
        for name, record in manifest["files"].items():
            actual = hashlib.sha256((source / name).read_bytes()).hexdigest()
            if actual != record["sha256"]:
                raise RuntimeError(f"integrity mismatch: {name}")
        prefix = root / "install"
        command = ["sh", str(source / "install.sh"), "--prefix", str(prefix)]
        subprocess.run(command, check=True)
        binary = prefix / "bin/treeharbor"
        version = subprocess.check_output([str(binary), "--version"], text=True, timeout=15).strip()
        expected = "treeharbor " + manifest["product_version"]
        if version != expected:
            raise RuntimeError(f"executable version differs from manifest: {version!r}")
        subprocess.run([str(binary), "--help"], check=True, stdout=subprocess.DEVNULL, timeout=15)
        for subcommand in (["admin", "--help"], ["serve", "--help"], ["github", "queue", "--help"]):
            subprocess.run([str(binary), *subcommand], check=True, stdout=subprocess.DEVNULL, timeout=15)
        if subprocess.run(command, capture_output=True).returncode == 0:
            raise RuntimeError("installer unexpectedly replaced an existing installation")
        subprocess.run([str(binary), "--version"], check=True, timeout=15)
        print(json.dumps({"version": 1, "target": manifest["target"],
                          "product_version": manifest["product_version"],
                          "source_revision": manifest["source_revision"],
                          "offline_install": True, "executable_smoke": True,
                          "database_or_runtime_acceptance": False}))


if __name__ == "__main__":
    main()
