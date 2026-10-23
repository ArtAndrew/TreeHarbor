#!/usr/bin/env python3
"""Independent projection wire and reservation vector (stdlib only)."""
import hashlib
import json
import pathlib
import uuid

tenant = 'projection-vector'
repository = '00112233-4455-6677-8899-aabbccddeeff'
projection = 'ffeeddcc-bbaa-9988-7766-554433221100'
object_bytes = bytes.fromhex('17' * 32)
payload = (b'treeharbor-github-projection-object\0v1\0'
           + len(tenant.encode()).to_bytes(8, 'big') + tenant.encode()
           + uuid.UUID(repository).bytes + uuid.UUID(projection).bytes + object_bytes)
value = dict(tenant=tenant, repository=repository, object='sha256:'+object_bytes.hex(),
             hold_token=str(uuid.UUID(bytes=hashlib.sha256(payload).digest()[:16])),
             request=dict(action='projection_request', key='project-version-2', epoch=1,
                          projection=projection, binding='aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
                          specification=dict(changeset='bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb',
                                             version=2, title='Update selected files', ttl_seconds=600)))
out = pathlib.Path(__file__).resolve().parents[1] / 'crates/metadata/tests/fixtures/github-projection-v1.json'
out.write_text(json.dumps(value, indent=2)+'\n')
