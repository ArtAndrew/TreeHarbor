#!/usr/bin/env python3
"""Independent v1 candidate-review identity/selector example; no Rust execution."""
import json
from pathlib import Path

uuid = lambda n: f"{n:08x}-1111-4111-8111-111111111111"
obj = lambda n: "sha256:" + f"{n:02x}" * 32
rev = lambda n: "native:" + obj(n)
vector = dict(
    request=dict(
        version=1, tenant="example", repository=uuid(1), authority="standalone",
        epoch=4, query=dict(kind="candidate", candidate=uuid(2)), limit=20,
        after=None, text=True,
    ),
    identity=dict(
        kind="candidate", candidate=uuid(2), entry=uuid(3), target="main",
        changeset=uuid(4), version=2, run=uuid(5), proposal_base_revision=rev(1),
        expected_head=rev(2), expected_generation=3, candidate_epoch=3,
        revision=rev(3), policy=obj(4), merge_profile="treeharbor-native-merge-v2/diffy-0.5.2/exact-sibling-renames/subtree-delta",
    ),
)
root = Path(__file__).resolve().parents[1]
(root / "crates/metadata/tests/fixtures/review-candidate-v1.json").write_text(
    json.dumps(vector, sort_keys=True, separators=(",", ":")) + "\n"
)
