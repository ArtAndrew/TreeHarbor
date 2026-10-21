#!/usr/bin/env python3
"""Independent Python reference for docs/managed-runs.md's canonical formats."""
import hashlib
import json
from pathlib import Path
from uuid import UUID

ROOT = Path(__file__).resolve().parents[1]


def canonical(value):
    return json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode()


def write(directory, name, value, prefix=b""):
    directory.mkdir(parents=True, exist_ok=True)
    data = canonical(value)
    digest = hashlib.sha256(prefix + data).hexdigest()
    (directory / f"{name}.json").write_bytes(data)
    (directory / f"{name}.sha256").write_text(digest + "\n")
    return digest


node = ROOT / "crates/node/tests/fixtures"
volume = {
    "version": 1,
    "storage": str(UUID(int=2)),
    "slot": str(UUID(int=3)),
    "volume": {
        "mount": "/srv/volume",
        "files": "/srv/volume/placement",
        "device": 1234,
        "mount_inode": 2,
        "files_inode": 14,
        "mount_id": 31,
        "capacity_bytes": 120000000,
        "limit_bytes": 134217728,
    },
}
volume_digest = write(node, "volume-v1", volume, b"treeharbor-node-volume\0v1\0")
intent = json.loads((node / "intent-v1.json").read_bytes())
intent["version"] = 2
intent["volume"] = "sha256:" + volume_digest
write(node, "intent-v2", intent)
run = [1, "tenant", str(UUID(int=1)), str(UUID(int=7)), str(UUID(int=8)),
       str(UUID(int=4)), "owner-é", "standalone", 3, 2, [17] * 32,
       ["/bin/tool", 'literal "quoted" é'], [71] * 32]
write(ROOT / "crates/metadata/tests/fixtures", "run-binding-v1", run,
      b"treeharbor-run-binding\0v1\0")
