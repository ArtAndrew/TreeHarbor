#!/usr/bin/env python3
"""Independent managed capture intent v1; no Rust execution."""
import json
from pathlib import Path
raw = json.dumps(dict(version=1, seal='00000000-0000-0000-0000-000000000001',
                      binding='sha256:' + '2a' * 32), separators=(',', ':')).encode()
root = Path(__file__).resolve().parents[1] / 'crates/node/tests/fixtures'
(root / 'capture-intent-v1.json').write_bytes(raw)

(root / 'capture-complete-v1.json').write_bytes(json.dumps(dict(
    version=1, binding='sha256:'+'2a'*32, manifest='sha256:'+'2b'*32
), separators=(',', ':')).encode())
