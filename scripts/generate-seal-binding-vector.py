#!/usr/bin/env python3
"""Independent managed seal binding v1, including selection/profile encodings."""
import json
from hashlib import sha256
from pathlib import Path
from uuid import UUID
u = lambda i: str(UUID(int=i))
profile = b'treeharbor-profile\0' + (1).to_bytes(2, 'big') + bytes(5)
selection = bytearray(b'treeharbor-capture-selection\0' + (1).to_bytes(2, 'big'))
for group in ([b'src'], [b'old'], [b'src/cache']):
    selection += len(group).to_bytes(4, 'big')
    for path in group:
        selection += len(path).to_bytes(4, 'big') + path
values = [1,'tenant',u(1),u(8),u(2),u(3),'03'*32,u(5),2,3,'standalone','node-1',
          u(6),u(7),'04'*32,u(4),'05'*32,'native:sha256:'+'01'*32,'sha256:'+'02'*32,
          sha256(profile).hexdigest(),'sha256:'+sha256(selection).hexdigest(),100,1048576]
raw = json.dumps(values, ensure_ascii=False, separators=(',', ':')).encode()
root = Path(__file__).resolve().parents[1] / 'crates/metadata/tests/fixtures'
(root/'seal-binding-v1.json').write_bytes(raw)
(root/'seal-binding-v1.sha256').write_text('sha256:'+sha256(b'treeharbor-seal-binding\0v1\0'+raw).hexdigest()+'\n')
