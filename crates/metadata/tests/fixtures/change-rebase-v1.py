#!/usr/bin/env python3
"""Independent rebase reservation vector (Python stdlib; no Rust encoder)."""
import hashlib
import json
import uuid

tenant = 'rebase-vector'
repository = '00112233-4455-6677-8899-aabbccddeeff'
rebase = 'ffeeddcc-bbaa-9988-7766-554433221100'
object_hex = '11' * 32
profile = 'treeharbor-change-rebase-v1/treeharbor-native-merge-v2/diffy-0.5.2/exact-sibling-renames/subtree-delta'
body = (b'treeharbor-rebase-object\0v1\0'
        + len(tenant.encode()).to_bytes(8, 'big') + tenant.encode()
        + uuid.UUID(repository).bytes + uuid.UUID(rebase).bytes
        + bytes.fromhex(object_hex))
print(json.dumps(dict(version=1, tenant=tenant, repository=repository, rebase=rebase,
                     object='sha256:' + object_hex, profile=profile,
                     hold_token=str(uuid.UUID(bytes=hashlib.sha256(body).digest()[:16]))), indent=2))
