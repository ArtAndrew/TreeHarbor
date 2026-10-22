#!/usr/bin/env python3
"""Independent seal object reservation vector; no Rust/service calls."""
from hashlib import sha256
from pathlib import Path
from uuid import UUID

scope = b"test-tenant"
data = (b"treeharbor-seal-object\0v1\0" + len(scope).to_bytes(8, "big") + scope
        + UUID(int=1).bytes + UUID(int=2).bytes + bytes([17]) * 32)
target = Path(__file__).resolve().parents[1] / "crates/metadata/tests/fixtures/seal-reservation-v1.txt"
target.write_text(str(UUID(bytes=sha256(data).digest()[:16])) + "\n")
