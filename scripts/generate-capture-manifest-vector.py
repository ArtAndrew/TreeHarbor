#!/usr/bin/env python3
"""Independent immutable capture manifest v1; no Rust execution."""
from hashlib import sha256
from pathlib import Path
import json
import struct

def frame(kind, payload):
    return b'treeharbor\0' + struct.pack('>HBQ', 1, kind, len(payload)) + payload

def ident(raw):
    return sha256(raw).digest()

def text(raw):
    return 'sha256:' + raw.hex()

def blob(raw):
    return ident(frame(1, raw))

def directory(entries):
    payload = len(entries).to_bytes(4, 'big')
    for name, mode, obj in sorted(entries):
        payload += len(name).to_bytes(4, 'big') + name + mode.to_bytes(4, 'big') + obj
    return ident(frame(2, payload))

file_mode, dir_mode = 0o100644, 0o40000
before = blob(b'before\n')
build = directory([(b'cache', file_mode, blob(b'old cache\n'))])
src = directory([(b'file', file_mode, before), (b'.env', file_mode, blob(b'old secret\n')),
                 (b'build', dir_mode, build), (b'empty', dir_mode, directory([]))])
root = directory([(b'src', dir_mode, src), (b'outside', file_mode, blob(b'outside\n')),
                  (b'obsolete', file_mode, blob(b'obsolete\n'))])
base = ident(frame(3, root))
path = b'src/file'
selection = (b'treeharbor-capture-selection\0' + (1).to_bytes(2, 'big')
             + (1).to_bytes(4, 'big') + len(path).to_bytes(4, 'big') + path + bytes(8))
after_frame = frame(1, b'after\n')
after = ident(after_frame)
ops_frame = frame(4, (1).to_bytes(4, 'big') + len(path).to_bytes(4, 'big') + path
                  + b'\1' + file_mode.to_bytes(4, 'big') + before
                  + b'\1' + file_mode.to_bytes(4, 'big') + after)
manifest = dict(version=1, tenant='test', repository='11111111-1111-4111-8111-111111111111',
                base_snapshot=text(base), selection_hex=selection.hex(), operations=text(ident(ops_frame)),
                objects=[text(after), text(ident(ops_frame))], baseline_dependencies=[[text(before), 1]],
                encoded_bytes=len(after_frame)+len(ops_frame), filesystem_entries=1,
                filesystem_bytes=6, path_bytes=len(path))
raw = json.dumps(manifest, ensure_ascii=False, separators=(',', ':')).encode()
output = Path(__file__).resolve().parents[1] / 'crates/materializer/tests/fixtures'
output.mkdir(exist_ok=True)
(output/'capture-manifest-v1.json').write_bytes(raw)
(output/'capture-manifest-v1.sha256').write_text('sha256:' + sha256(raw).hexdigest() + '\n')
