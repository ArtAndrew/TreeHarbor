#!/usr/bin/env python3
"""Independent canonical v1/v2 frames; no Rust encoder or Git invocation."""
import hashlib
import pathlib
import struct


def frame(kind, payload, version=1):
    return b"treeharbor\0" + struct.pack(">HBQ", version, kind, len(payload)) + payload


def raw_id(value):
    return hashlib.sha256(value).digest()


def sized(value):
    return struct.pack(">I", len(value)) + value


before = frame(1, b"before\n")
after = frame(1, b"after\n")
directory = frame(2, struct.pack(">I", 1) + sized(b"file") + struct.pack(">I", 0o100755) + raw_id(after))
entry_before = struct.pack(">I", 0o100644) + raw_id(before)
entry_after = struct.pack(">I", 0o040000) + raw_id(directory)
operations = frame(4, struct.pack(">I", 1) + sized(b"a") + b"\1" + entry_before + b"\1" + entry_after, 2)
values = [before, after, directory, operations]
out = pathlib.Path(__file__).resolve().parents[1] / "crates/core/tests/fixtures/subtree-v2.txt"
out.write_text("".join(raw_id(value).hex() + " " + value.hex() + "\n" for value in values))
