#!/usr/bin/env python3
"""Independent archive v1 and native full-pack v3, from core framing vectors."""
import hashlib
import json
import pathlib
import struct

root = pathlib.Path(__file__).resolve().parents[1]
values = [line.split() for line in (root / 'crates/core/tests/fixtures/git-origin-v2.txt').read_text().splitlines()]
frames = [bytes.fromhex(v[1]) for v in values]
ids = [bytes.fromhex(v[0]) for v in values]
transfer = [5, 1, 0, 2, 4]
order = [4, 2, 0, 1, 5]
size = sum(len(frames[i]) for i in transfer)
wire = b'treeharbor-source-archive\0' + struct.pack('>H', 1) + ids[5] + ids[1] + struct.pack('>IQ', len(transfer), size)
for i in transfer:
    wire += ids[i] + struct.pack('>Q', len(frames[i])) + frames[i]
fixtures = root / 'crates/service/tests/fixtures'
(fixtures / 'source-archive-v1.hex').write_text(wire.hex() + '\n')
manifest = dict(version=3, selection='full', source_revision='native:sha256:' + ids[5].hex(),
                commit='sha256:' + ids[5].hex(), snapshot='sha256:' + ids[1].hex(),
                objects=['sha256:' + ids[i].hex() for i in order], encoded_bytes=size, filesystem_entries=0)
raw = json.dumps(manifest, separators=(',', ':')).encode()
(fixtures / 'source-pack-v3.json').write_bytes(raw + b'\n')
print('archive sha256:', hashlib.sha256(wire).hexdigest())
print('pack fingerprint: sha256:' + hashlib.sha256(b'treeharbor-source-pack\0v3\0' + raw).hexdigest())
