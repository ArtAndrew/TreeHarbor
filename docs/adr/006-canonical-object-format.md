# ADR 006 Canonical object format version 1

Status: implementation decision, 2026-10-21. Implements the domain-separated
format required by architecture sections 7–8 and the native history in ADR-004.
No product capability or release gate is removed by this decision.

All object integers are big endian. A frame is the ten ASCII bytes
`treeharbor` followed by NUL, a u16 version (1), a u8 kind, a u64 payload
length, then exactly that many payload bytes. The object identifier is SHA-256
of this entire frame. Text form is `sha256:` and 64 lowercase hexadecimal digits.
The decoder rejects trailing bytes, unknown kinds/versions, and noncanonical data.

Kinds: 1 Blob, 2 Directory, 3 Snapshot, 4 ChangeOperations, 5 NativeCommit.
A byte string is a u32 byte count and the raw bytes. A collection starts with
a u32 count. A digest is exactly 32 bytes, with no embedded string encoding.

- Blob payload is the unmodified file content, including arbitrary binary bytes.
- Directory payload is a collection of entries sorted by raw name bytes, strictly
  increasing, with no duplicate names. Each entry is name bytes, u32 Git-style
  mode, and digest. Modes are directory 040000, file 100644, executable 100755,
  symlink 120000. Gitlinks are rejected until a separate compatibility profile.
- Snapshot payload is the root directory digest.
- ChangeOperations payload is a collection sorted by raw repository path.
  Each operation is path bytes, expected-before (u8 0 absent, 1 followed by
  u32 mode and digest), then replacement (same encoding). None means deletion.
  Both absent is invalid. Duplicate and ancestor/descendant paths in one batch
  are rejected initially, so application order cannot change the result.
  Expected-before compares both content and mode against the original snapshot.
- NativeCommit payload is an ordered collection of parent digests, snapshot
  digest, changes digest, UTF-8 author name, UTF-8 author email, i64 Unix seconds,
  i16 timezone offset in minutes, and UTF-8 message. These bytes freeze before
  validation. Server authentication identity is separate metadata, never inferred
  from author text. The snapshot and changes digests identify typed objects.

Path and entry names preserve raw bytes. Paths must be relative, nonempty,
contain no empty/dot/dot-dot components, NUL or backslash. Components matching
`.git` or `.treeharbor` ignoring ASCII case are reserved. Case/normalization
collisions and symlink containment are checked again by the materializer for
the destination filesystem; canonical storage alone does not promise safe IO.

Version 1 limits: payload 64 MiB, path 4096 bytes, component 255 bytes, path depth
128, collection 100,000 entries, commit parents 1,024. These are decoder safety
limits, not license quotas. Large-file chunking is a later explicit format
extension; v1 rejects larger objects rather than silently truncating them.

Object identity does not assert graph validity or durable publication. Storage
verifies digests. Metadata acceptance checks typed graph edges, reservations
and live generations before atomically pinning an accepted root. A read-only
in-memory object implementation is only a unit-test oracle, never a server.
