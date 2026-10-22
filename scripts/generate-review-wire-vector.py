#!/usr/bin/env python3
"""Independent stdlib JSON/framing/hash/unified-patch vector for review v1."""
import difflib
import hashlib
import json
from pathlib import Path
import struct


def blob(value):
    frame = b"treeharbor\0" + struct.pack(">HBQ", 1, 1, len(value)) + value
    return "sha256:" + hashlib.sha256(frame).hexdigest()


before = "one\ntwo\nthree\n"
after = "one\nTWO\nthree\n"
request = dict(
    version=1,
    tenant="example",
    repository="11111111-1111-4111-8111-111111111111",
    authority="standalone",
    epoch=3,
    query=dict(kind="change", changeset="22222222-2222-4222-8222-222222222222", version=2),
    limit=20,
    after=None,
    text=True,
)
patch = "".join(difflib.unified_diff(
    before.splitlines(keepends=True), after.splitlines(keepends=True),
    fromfile="before", tofile="after", n=3,
))
vector = dict(
    request=request,
    profile="treeharbor-review-diff-v1/diffy-0.5.2/context-3",
    before=before,
    after=after,
    entry=dict(
        path_hex=b"file.txt".hex(), kind="modify",
        before=dict(mode=33188, object=blob(before.encode())),
        after=dict(mode=33188, object=blob(after.encode())),
        text=dict(status="patch", patch=patch),
    ),
)
root = Path(__file__).resolve().parents[1]
(root / "crates/service/tests/fixtures/review-diff-v1.json").write_text(
    json.dumps(vector, sort_keys=True, separators=(",", ":")) + "\n"
)
