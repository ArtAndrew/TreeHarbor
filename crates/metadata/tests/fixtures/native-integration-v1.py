#!/usr/bin/env python3
"""Independent policy/hash/reservation vector, using only Python stdlib."""
import hashlib
import json
import uuid

policy = {
    'version': 1,
    'checks': [dict(name='tests', definition='sha256:'+'07'*32,
                    environment='sha256:'+'08'*32, toolchain='sha256:'+'09'*32,
                    dependencies='sha256:'+'0a'*32, validators=['ci-a', 'ci-b'])],
    'reviewers': ['reviewer-a', 'reviewer-b'], 'approvals': 1,
}
canonical = json.dumps(policy, ensure_ascii=False, separators=(',', ':'))
tenant = 'native-vector'
repository = '00112233-4455-6677-8899-aabbccddeeff'
candidate = 'ffeeddcc-bbaa-9988-7766-554433221100'
object_hex = '11'*32
body = (b'treeharbor-candidate-object\0v1\0'
        + len(tenant.encode()).to_bytes(8, 'big') + tenant.encode()
        + uuid.UUID(repository).bytes + uuid.UUID(candidate).bytes
        + bytes.fromhex(object_hex))
value = dict(version=1, policy=policy, canonical=canonical,
             policy_digest=hashlib.sha256(b'treeharbor-native-policy\0v1\0'+canonical.encode()).hexdigest(),
             tenant=tenant, repository=repository, candidate=candidate,
             object='sha256:'+object_hex,
             hold_token=str(uuid.UUID(bytes=hashlib.sha256(body).digest()[:16])))
print(json.dumps(value, indent=2))
