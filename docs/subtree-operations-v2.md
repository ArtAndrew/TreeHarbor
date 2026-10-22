# Exact subtree operations: canonical frame v2

`treeharbor-core` now represents a directory entry replacement with the same
exact-before predicate used for files. This is needed to describe file/directory
transitions and retain their complete before/after source graphs. It does not
provide process isolation, validate code, or authorize publication.

## Wire format and compatibility

The existing 22-byte frame is unchanged:

```text
"treeharbor\0" (11 bytes)
version        u16 big endian
kind           u8
payload_length u64 big endian
payload        exactly payload_length bytes
```

The object ID is SHA-256 of the complete frame. Kinds 1 (blob), 2 (directory),
3 (snapshot) remain version 1 with identical bytes/IDs. Native commits without
Git origins also keep v1; [origin-bearing commits](git-origin-v2.md) use kind5/v2.
Kind 4 (change operations) uses version 2 **only if at least one expected or
replacement entry is a directory**. Leaf-only operations, including an empty
list, still encode as version 1. A v2 frame for kinds 1–3, v1 operations with
a directory, or v2 operations without a directory are rejected. Old readers fail
explicitly on version 2; this is not an in-place reinterpretation of v1 bytes.

Payload layout remains:

```text
operation_count u32 big endian
repeat operation_count times, strictly sorted by raw path bytes:
  path_length   u32 big endian
  path          path_length bytes
  expected      optional entry
  replacement   optional entry

optional entry:
  0             one byte, absent
  1             one byte, followed by:
    mode        u32 big endian
    target      32 bytes, raw native object digest
```

Modes `0100644`, `0100755` and `0120000` reference a Blob (kind 1).
In v2, mode `0040000` references a Directory (kind 2). Directory references are
real typed graph edges: all descendant objects remain reachable through both the
expected and replacement entries. GC, upload membership and backup must follow
these edges exactly as for ordinary snapshot directories.

Existing limits and path rules apply: at most 100,000 operations, bounded frame
payload, unique paths, no path that is an ancestor of another operation, and at
least one of expected/replacement present. `a`, `a-1`, `a/b` is still rejected;
a sort-order neighbor must not hide the overlapping `a` and `a/b` edits.

[Independent Python vectors](../scripts/generate-subtree-vectors.py) produce the
[fixed frame/hash fixture](../crates/core/tests/fixtures/subtree-v2.txt), containing
two v1 blobs, a v1 directory and a v2 file-to-directory operation. The Rust tests
compare complete encoded bytes and digests against these independently computed
values, reject version aliases and retain all prior v1 golden vectors.

## Application and structural difference

`Changes::new` preserves the original leaf-only contract.
`Changes::with_subtrees` permits directory entries and selects the canonical
version from the content. The constructor does not grant access to the referenced
objects. In a service, every reference still needs scoped membership or a trusted,
protected source derivation.

`apply_changes` checks **all** expected entries against the original tree before
writing any result. A directory predicate matches its entire immutable digest,
not just its existence. It checks the replacement root's object kind. A file-to-
directory transition is one replacement at `a`; it is not an ordered sequence of
`delete a` followed by `add a/b`. Failed predicates and wrong replacement kinds
produce no result writes. Immutable intermediate writes can remain after a later
storage IO error, without creating a publication or successful ChangeSet.

`diff_snapshots(store, before, after, TreeDiffLimits)` is a mechanical Rust tree
difference, separate from three-way merge. It reads directories, skips equal
subtrees and never reads blob payloads or writes objects. It recursively compares
two directories, and emits exact entry replacements for additions, deletions and
type changes. Explicit empty directories are preserved, including a nonempty
directory becoming empty. The caller must replay and compare the result before
publication. It must also keep complete, validated input graphs pinned: this diff
is not a restore verifier or authorization check.

Defaults bound directory reads (20,000), visited entries (200,000), cumulative
path bytes (32 MiB), encoded read bytes (128 MiB) and operations (100,000).
One frame is still bounded by the core decoder's payload limit. Application
budgets do not replace runtime CPU/RAM/disk isolation.

## Current integration boundary and upgrade

The candidate builder now uses structural diff and replays its integration delta
against the exact current snapshot. The merge profile has changed to
`treeharbor-native-merge-v2/diffy-0.5.2/exact-sibling-renames/subtree-delta`.
It is included in candidate identity/context. A new candidate never inherits an
old candidate's passing checks. Old *preparing* candidates cannot be recomputed
under a different profile: cancel/requeue them after upgrade. Already computed
candidates remain immutable and subject to their existing exact-context policy.

Upgrade all readers, source-upload servers, nodes, exporters and backup tools
before allowing v2 writes. The current experimental service has no rolling-upgrade
capability negotiation or compatibility gate. Existing v1 data stays readable;
old binaries cannot safely resume writes to a graph containing v2 objects.
No PostgreSQL schema migration is needed: the object catalog already stores typed
edges and validates directory membership before publication.

Source graph uploads accept mixed v1/v2 graphs under their existing scoped
child-membership rules. Git export traverses these graphs and projects the source
history into an ordinary bundle. [Git bundle import](git-import.md) uses these
operations for verified first-parent deltas.
The public `changes/submit` wire v1 and its blob-only upload endpoint remain
leaf-only. This increment does not silently permit clients to reference arbitrary
server directories. A scoped subtree proposal transport is still required before
agents can submit directory replacements through that endpoint. This format
change alone does not complete either release mode.
