#!/usr/bin/env python3
"""Full-source manifest from independently encoded core vectors; no Rust/Git."""
import hashlib
import json
import pathlib
import struct

root = pathlib.Path(__file__).resolve().parents[1]
vectors = [line.split() for line in (root / 'crates/core/tests/fixtures/git-origin-v2.txt').read_text().splitlines()]
raw = bytes.fromhex(vectors[4][1])[len(b'treeharbor\0') + struct.calcsize('>HBQ'):]
oid = hashlib.sha1(b'commit ' + str(len(raw)).encode() + b'\0' + raw).hexdigest()
# DFS in reverse reference order, children before their parents.
indices = [4, 2, 0, 1, 5]
manifest = dict(version=2, selection='full', source_revision='git:sha1:' + oid,
                commit='sha256:' + vectors[5][0], snapshot='sha256:' + vectors[1][0],
                objects=['sha256:' + vectors[i][0] for i in indices],
                encoded_bytes=sum(len(bytes.fromhex(vectors[i][1])) for i in indices),
                filesystem_entries=0)
encoded = json.dumps(manifest, separators=(',', ':')).encode()
out = root / 'crates/service/tests/fixtures/source-pack-v2.json'
out.write_bytes(encoded + b'\n')
print('fingerprint: sha256:' + hashlib.sha256(b'treeharbor-source-pack\0v2\0' + encoded).hexdigest())
