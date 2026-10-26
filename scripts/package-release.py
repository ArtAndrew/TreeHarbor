#!/usr/bin/env python3
"""Package prebuilt Linux binaries; never downloads, compiles or publishes."""
import argparse
import gzip
import hashlib
import io
import json
import os
from pathlib import Path
import re
import stat
import struct
import subprocess
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"x86_64-unknown-linux-musl": 62, "aarch64-unknown-linux-musl": 183}
TOOLCHAIN_NOTICES = {
    "third-party/toolchain/rust-1.99.1/COPYRIGHT": "rust-stdlib",
    "third-party/toolchain/rust-1.99.1/COPYRIGHT-library.html": "rust-stdlib",
    "third-party/toolchain/rust-1.99.1/LICENSE-MIT": "rust-stdlib",
    "third-party/toolchain/rust-1.99.1/LICENSE-APACHE": "rust-stdlib",
    "third-party/toolchain/compiler-builtins/LICENSE.txt": "compiler-builtins",
    "third-party/toolchain/libm/LICENSE.txt": "libm",
    "third-party/toolchain/musl-1.2.5/COPYRIGHT": "musl",
}


def verify_toolchain_notices(tracked):
    """Check the reviewed notice set, not how the supplied binaries were built."""
    name = "third-party/toolchain/manifest.json"
    if name not in tracked:
        raise ValueError("tracked toolchain notice manifest is required")
    inventory = json.loads(regular(ROOT / name))
    if not isinstance(inventory, dict):
        raise ValueError("invalid toolchain notice inventory")
    channel = tomllib.loads(regular(ROOT / "rust-toolchain.toml").decode())["toolchain"]["channel"]
    if (inventory.get("version") != 1 or inventory.get("rust_version") != channel
            or channel != "1.99.1"
            or not re.fullmatch(r"[0-9a-f]{40}", inventory.get("rust_source_revision", ""))):
        raise ValueError("toolchain notices require review for the pinned compiler")
    records = inventory.get("files")
    if not isinstance(records, list) or len(records) != len(TOOLCHAIN_NOTICES):
        raise ValueError("incomplete toolchain notice inventory")
    seen = set()
    for record in records:
        if not isinstance(record, dict):
            raise ValueError("invalid toolchain notice record")
        path = record.get("path")
        if (not isinstance(path, str) or path not in TOOLCHAIN_NOTICES
                or path not in tracked or path in seen
                or record.get("component") != TOOLCHAIN_NOTICES[path]):
            raise ValueError("unexpected or untracked toolchain notice path")
        seen.add(path)
        if hashlib.sha256(regular(ROOT / path)).hexdigest() != record.get("sha256"):
            raise ValueError(f"toolchain notice hash mismatch: {path}")
    if seen != set(TOOLCHAIN_NOTICES):
        raise ValueError("incomplete toolchain notice inventory")


def regular(path):
    if not stat.S_ISREG(path.lstat().st_mode):
        raise ValueError(f"not a regular file: {path}")
    return path.read_bytes()


def binary(path, machine):
    data = regular(path)
    if not os.access(path, os.X_OK):
        raise ValueError(f"not executable: {path}")
    if len(data) < 64 or data[:6] != b"\x7fELF\x02\x01":
        raise ValueError(f"not a 64-bit little-endian ELF: {path}")
    if struct.unpack_from("<H", data, 18)[0] != machine:
        raise ValueError(f"binary architecture differs from target: {path}")
    # Musl artifacts must not depend on an ELF interpreter on the target host.
    phoff = struct.unpack_from("<Q", data, 32)[0]
    size, count = struct.unpack_from("<HH", data, 54)
    if size < 56 or count > 1024 or phoff + size * count > len(data):
        raise ValueError(f"malformed ELF headers: {path}")
    if any(struct.unpack_from("<I", data, phoff + i * size)[0] == 3 for i in range(count)):
        raise ValueError(f"binary requires a dynamic interpreter: {path}")
    return data


def package(args):
    version = tomllib.loads(regular(ROOT / "Cargo.toml").decode())["workspace"]["package"]["version"]
    if not re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[0-9A-Za-z.-]+)?", version):
        raise ValueError("unsupported package version")
    revision = subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip()
    if not re.fullmatch(r"[0-9a-f]{40}", revision):
        raise ValueError("invalid source revision")
    if subprocess.check_output(["git", "status", "--porcelain", "--untracked-files=normal"], cwd=ROOT).strip():
        raise ValueError("package from a clean committed checkout")
    epoch = int(subprocess.check_output(["git", "show", "-s", "--format=%ct", "HEAD"], cwd=ROOT))
    files = {}
    def add(name, data, mode=0o644):
        if name in files or name.startswith("/") or any(p in ("", ".", "..") for p in name.split("/")):
            raise ValueError(f"invalid or duplicate archive path: {name}")
        files[name] = (data, mode)
    for name in ("treeharbor", "treeharbor-runtime-init"):
        add("bin/" + name, binary(args.bin_dir / name, TARGETS[args.target]), 0o755)
    for name in ("LICENSE", "NOTICE", "THIRD_PARTY_NOTICES.md", "README.md", "SECURITY.md", "Cargo.lock"):
        add(name, regular(ROOT / name))
    # Only tracked public documentation/examples/notices are eligible. Never walk
    # the workspace (which can contain operator state, credentials or local ADRs).
    tracked = subprocess.check_output(["git", "ls-files", "-z", "docs", "examples", "third-party"], cwd=ROOT).split(b"\0")
    verify_toolchain_notices({os.fsdecode(p) for p in tracked if p})
    for raw in sorted(p for p in tracked if p):
        name = os.fsdecode(raw)
        if name.startswith(("docs/adr/", "docs/specs/")):
            raise ValueError("internal documentation cannot enter a release")
        add(name, regular(ROOT / name))
    add("VERSION", (version + "\n").encode())
    add("install.sh", regular(ROOT / "scripts/install-release.sh"), 0o755)
    manifest = {"version": 1, "product_version": version, "target": args.target,
                "source_revision": revision, "source_date_epoch": epoch,
                "files": {name: {"sha256": hashlib.sha256(data).hexdigest(), "mode": oct(mode)}
                          for name, (data, mode) in sorted(files.items())}}
    add("manifest.json", (json.dumps(manifest, sort_keys=True, indent=2) + "\n").encode())
    sums = "".join(f"{hashlib.sha256(data).hexdigest()}  {name}\n" for name, (data, _) in sorted(files.items()))
    add("SHA256SUMS", sums.encode())
    stem = f"treeharbor-{version}-{args.target}"
    args.output_dir.mkdir(parents=True, exist_ok=True)
    destination = args.output_dir / (stem + ".tar.gz")
    # A fixed gzip timestamp and canonical tar headers make the same inputs yield
    # the same bytes. This does not claim reproducible compiler output.
    with destination.open("xb") as output:
        with gzip.GzipFile(fileobj=output, mode="wb", filename="", mtime=epoch) as gz:
            with tarfile.open(fileobj=gz, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                for name, (data, mode) in sorted(files.items()):
                    info = tarfile.TarInfo(stem + "/" + name)
                    info.size, info.mode, info.mtime = len(data), mode, epoch
                    archive.addfile(info, io.BytesIO(data))
    checksum = hashlib.sha256(destination.read_bytes()).hexdigest()
    destination.with_suffix(destination.suffix + ".sha256").write_text(f"{checksum}  {destination.name}\n")
    print(destination)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", required=True, choices=TARGETS)
    parser.add_argument("--bin-dir", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    try:
        package(parser.parse_args())
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"package failed: {error}\n")


if __name__ == "__main__":
    main()
