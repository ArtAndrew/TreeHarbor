# ADR 012: Protected incremental reads from the local object catalog

Date: 2026-10-21. Status: implemented internal library contract. No external API,
workspace materializer, MCP server or performance result is implied.

## Problem

An object locator followed by a filesystem read leaves a race: the reader's hold
can expire, or its workspace pin can disappear, before the file is opened. Holding
a database transaction while downloading/hashing a whole tree would instead add
long locks and require loading history unrelated to the requested source files.

## Decision

Use incremental sessions rooted at an existing repository pin. The coordinator
must authenticate repository and holder access before calling `open_read_session`.
The returned session owns a generation-bound, expiring root hold. It starts with
one private `ReadNode`; subsequent nodes come only from references in verified
object bytes. Rust callers cannot construct nodes from arbitrary supplied hashes,
and nodes from another session are rejected. These types are internal and are not
wire-format bearer capabilities or substitutes for authentication.

For each requested frontier, `open_read_batch`:

1. Checks session identity, duplicate IDs, the 256-object cap and caller byte limit.
2. Locks and validates the root generation and active hold against retirement.
3. Resolves requested objects in one catalog query, checking LIVE state, backend,
   expected type and the generation-qualified parent edge that authorized each child.
4. Checks the aggregate encoded size against the caller limit and the hard 128 MiB
   cap before opening files. These are per-batch limits, not a process RSS guarantee.
5. Opens regular local object descriptors and compares their lengths with the catalog
   while the root is still protected. The transaction must commit before returning.
6. Reads and verifies frames, digests and types from those descriptors outside the
   transaction. The result is returned only if the entire requested batch verifies.

On the supported Linux/macOS local filesystems, an opened descriptor keeps its
inode accessible after GC removes the directory entry. The exact opened bytes can
therefore finish reading after session expiry, root unpinning and physical GC.
Reading a later frontier still requires a live session. Expired sessions cannot be
revived; generation reuse cannot make an old session valid. Callers renew before
expiry and explicitly close sessions when finished. A crash closes descriptors
and leaves only the expiring hold, without creating permanent reader pins.

The root uses a shared row lock, allowing concurrent readers while excluding
retirement and hold release. The lock ends before content IO/hashing. It covers only bounded local
open/stat work; a 5-second idle-in-transaction timeout prevents indefinitely parked
database sessions, and a terminated transaction cannot return a successful batch.
The service-owned storage tree must remain inaccessible to agents. Malicious local
writers, remapping a backend label to another directory and concurrent unfenced
restore remain outside this primitive's guarantees.

Reading a native commit yields its immediate references without traversing them.
The caller can select its snapshot, the required directories and selected blobs;
unrequested file contents and commit parents are not loaded. Durable history
retention still follows every published parent independently of this read strategy.
Repeated references must be deduplicated by a traversal before batching.

## Failure behavior and limits

- Missing/corrupt files, wrong catalog types or lengths, broken edges, cross-session
  nodes, stale generations and mismatched backends fail closed.
- An error or cancelled future drops opened descriptors. A batch already handed to
  a blocking worker may finish locally; the coordinator must not publish a result
  after external cancellation/revocation. No background worker is spawned here.
- This contract reads one configured local backend per batch. It does not promise
  the same open-descriptor behavior for S3, network filesystems or Windows. The S3
  reader needs a separate design for immutable generations and in-flight retention.
- Admission limits across clients, a bounded IO pool, deadlines, pagination, cached
  decoding, physical workspace creation and credential revocation are service-layer
  work still required before exposing the primitive remotely.
- Expired hold-row cleanup and disk accounting for unlinked but open inodes remain
  required operational work. Long-lived batches consume descriptors and storage;
  their cost must be included in the benchmark and reaper design.
- Database schema and canonical object format are unchanged. No new dependency.

## Evidence

Eight real PostgreSQL/local-file tests cover selected reads despite unavailable
unrequested data; opened batches after complete GC and generation reuse; scope and
backend separation; byte/object limits before IO; corrupted bytes and missing edges;
renewal/close retention; and current snapshot reads without loading either merge
parent; and concurrent readers sharing root protection. Existing catalog, intake,
storage and transaction tests remain applicable.
This is correctness evidence, not a speed or sandbox claim.
