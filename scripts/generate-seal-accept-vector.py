#!/usr/bin/env python3
"""Independent acceptance reservation identity; no Rust or service calls."""
from hashlib import sha256
from pathlib import Path
from uuid import UUID

tenant, key = b"test-tenant", b"accept-1"
data = (b"treeharbor-seal-accept-object\0v1\0" + len(tenant).to_bytes(8, "big") + tenant
        + UUID(int=1).bytes + len(key).to_bytes(8, "big") + key + bytes([17]) * 32)
target = Path(__file__).resolve().parents[1] / "crates/metadata/tests/fixtures/seal-accept-reservation-v1.txt"
target.write_text(str(UUID(bytes=sha256(data).digest()[:16])) + "\n")
