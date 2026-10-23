#!/usr/bin/env python3
"""Independent node cleanup journal v1 bytes and hashes; no Rust execution."""
import hashlib
import json
from pathlib import Path

root = Path(__file__).resolve().parents[1] / "crates/node/tests/fixtures"
identifier = lambda n: f"00000000-0000-0000-0000-{n:012x}"
payload = [1, identifier(1), "tenant-one", identifier(2), identifier(3), 2, 1,
           "standalone", "node-one", identifier(4), "sha256:" + "2b" * 32,
           "sha256:" + "2c" * 32,
           [identifier(5), identifier(6), "sha256:" + "2d" * 32,
            "sha256:" + "2e" * 32, "engine-one", "11" * 32]]
encoded = json.dumps(payload, separators=(",", ":")).encode()
(root / "cleanup-binding-v1.json").write_bytes(encoded)
(root / "cleanup-binding-v1.sha256").write_text(
    hashlib.sha256(b"treeharbor-cleanup-binding\0v1\0" + encoded).hexdigest() + "\n")
intent = dict(version=1, binding="sha256:" + "2a" * 32,
              root=dict(path="/private/node/slots/slot", device=1795, inode=12),
              initialized=True)
raw = json.dumps(intent, separators=(",", ":")).encode()
digest = hashlib.sha256(raw).hexdigest()
(root / "cleanup-intent-v1.json").write_bytes(raw)
(root / "cleanup-intent-v1.sha256").write_text(digest + "\n")
complete = json.dumps(dict(version=1, intent="sha256:" + digest, state="deleted"),
                      separators=(",", ":")).encode()
(root / "cleanup-complete-v1.json").write_bytes(complete)
(root / "cleanup-complete-v1.sha256").write_text(hashlib.sha256(complete).hexdigest() + "\n")
