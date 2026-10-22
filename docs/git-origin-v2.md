# Original Git commit metadata: native kind 5, version 2

Experimental immutable object format. This stores original source data; it grants
no merge permission, validation success, trusted authorship or signature approval.
Automatic Git repository import is not yet available. The bridge library and
`git-export` can validate and export an already complete origin-bearing native graph.

## Wire layout

The existing 22-byte header is unchanged: `treeharbor\0`, big-endian u16 version,
u8 kind, big-endian u64 payload length. Kind is 5 (`NativeCommit`).

A commit without `git_origin` remains version 1, with exactly the previous bytes
and ID. A commit with an origin is version 2. Its payload starts with the entire
v1 commit payload, followed by:

| Field | Encoding |
|---|---|
| Git hash algorithm | u8: 1 = SHA-1, 2 = SHA-256; others rejected |
| Claimed original Git OID | 20 or 32 raw bytes according to the algorithm |
| Original commit Blob | 32 raw bytes of its native SHA-256 object ID |

There is no absent-origin tag in v2. A v1 frame with a trailer, a v2 frame without
one, wrong algorithm, truncation and trailing bytes are rejected. Existing limits
apply to the complete payload. The raw Blob is the Git commit payload exactly,
without its `commit <length>\0` hash prefix. Its typed kind1 edge participates in
ordinary closure, upload membership, pinning, read leases and garbage collection.

[Independent vectors](../crates/core/tests/fixtures/git-origin-v2.txt) include v1
and both Git hash algorithms. Regenerate with
`python3 scripts/generate-git-origin-vectors.py`; it uses Python hashlib/struct,
not the Rust encoder or Git. Native object IDs always remain SHA-256.

## Verification at the Git boundary

A valid native digest and complete closure do **not** prove a Git origin claim.
The exporter recomputes the Git object hash, derives the complete native tree's
Git ID and recursively maps all ordered parents. All must equal the original
commit headers. The author, timestamp/offset and UTF-8 message must also match
the native fields. Extra structural tree/parent/author/committer headers are
rejected. Separate committer and optional extension/signature headers are retained
byte for byte. Git's strict object verification and an actual empty-repository
bundle restore must also succeed before publication.

Incorrect associations fail; the exporter does not replace them with a lossy
projection. A mismatch between original and requested Git hash format fails.
Non-UTF8 author/message, non-UTF8 encoding declarations and other unsupported
inputs follow the explicit [export compatibility limits](git-export.md).
Preserving a `gpgsig` field is not verification of the signature or its signer.

Newly generated native commits have no inherited origin. An original parent keeps
its exact Git ID, while a new descendant gets its deterministic native-v1 projection.
Different native change-operation metadata may map to the same Git object: source
export still does not preserve native deltas, checks, reviews or server authority.

## Upgrade

Upgrade every writer, server, worker, reader and export tool before enabling
kind5/v2 writes. Older readers reject these frames; rolling-upgrade negotiation is
not implemented. No database schema change is required because kinds and typed
edge storage are unchanged. Existing kind4/v2 subtree operations and kind1–5/v1
frames keep their exact canonical encoding. Retain the native source graph until
export has been verified; the raw metadata is not a disposable cache.
