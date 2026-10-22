#!/usr/bin/env python3
"""Independent Git and native v1/v2 framing and IDs using only hashlib/struct."""
import hashlib
import pathlib
import struct


def frame(kind, payload, version=1):
    return b'treeharbor\0' + struct.pack('>HBQ', version, kind, len(payload)) + payload


def digest(raw):
    return hashlib.sha256(raw).digest()


def sized(raw):
    return struct.pack('>I', len(raw)) + raw


def git(kind, raw, algorithm):
    return hashlib.new(algorithm, kind + b' ' + str(len(raw)).encode() + b'\0' + raw).digest()


root = frame(2, struct.pack('>I', 0))
snapshot = frame(3, digest(root))
changes = frame(4, struct.pack('>I', 0))
payload = struct.pack('>I', 0) + digest(snapshot) + digest(changes) + sized(b'Alice') + sized(b'a@example.invalid') + struct.pack('>qh', 1234567890, -330) + sized(b'message\n')
values = [root, snapshot, changes, frame(5, payload)]
for tag, algorithm in [(1, 'sha1'), (2, 'sha256')]:
    raw = b'tree ' + git(b'tree', b'', algorithm).hex().encode() + b'\nauthor Alice <a@example.invalid> 1234567890 -0530\ncommitter Bob <b@example.invalid> 1234567999 +0200\nencoding UTF-8\nx-test preserved\n continuation\n\nmessage\n'
    blob = frame(1, raw)
    values.extend([blob, frame(5, payload + bytes([tag]) + git(b'commit', raw, algorithm) + digest(blob), 2)])
out = pathlib.Path(__file__).resolve().parents[1] / 'crates/core/tests/fixtures/git-origin-v2.txt'
out.write_text(''.join(digest(v).hex() + ' ' + v.hex() + '\n' for v in values))
