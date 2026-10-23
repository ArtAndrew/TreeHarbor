"""Independent stdlib branch-identity vector; contains no credentials."""
import hashlib
import json
import uuid

tenant = "fixture-tenant"
repository = uuid.UUID("11111111-1111-4111-8111-111111111111")
change = uuid.UUID("22222222-2222-4222-8222-222222222222")
body = (b"treeharbor-github-publication-branch\0v1\0"
        + len(tenant.encode()).to_bytes(8, "big") + tenant.encode()
        + repository.bytes + change.bytes)
raw = bytearray(hashlib.sha256(body).digest()[:16])
raw[6] = (raw[6] & 15) | 128
raw[8] = (raw[8] & 63) | 128
branch = uuid.UUID(bytes=bytes(raw))
print(json.dumps({"version": 1, "tenant": tenant, "repository": str(repository),
                  "changeset": str(change), "branch_id": str(branch),
                  "branch": "refs/heads/treeharbor/" + branch.hex}, indent=2))
