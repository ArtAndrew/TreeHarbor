#!/usr/bin/env python3
"""Independent capture selection v1 encoding. No Rust execution."""
from hashlib import sha256
from pathlib import Path

wire = bytearray(b'treeharbor-capture-selection\0' + (1).to_bytes(2, 'big'))
for paths in ([b'src', b'readme\n".md'], [b'obsolete'], [b'src/build', b'src/.env']):
    wire += len(paths).to_bytes(4, 'big')
    for path in sorted(paths):
        wire += len(path).to_bytes(4, 'big') + path
root = Path(__file__).resolve().parents[1] / 'crates/core/tests/fixtures'
(root / 'capture-selection-v1.bin').write_bytes(wire)
(root / 'capture-selection-v1.sha256').write_text('sha256:' + sha256(wire).hexdigest() + '\n')
