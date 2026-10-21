# ADR 009: Object publication, roots and physical generations

Date: 2026-10-21. Status: accepted implementation refinement; manual local-backend
primitives implemented, not an autonomous production GC service.

## Storage and upload contract

Canonical object format v1 is unchanged. `PreparedObject` encodes once and binds
the digest, kind, length and outgoing typed references to those exact bytes.
`ManagedStore` uses a separate layout:

```text
<configured root>/managed-v1/<positive generation>/objects/<tenant>/<hash prefix>/<hash suffix>
```

The original loose `LocalStore` remains the reference backend. Its existing
layout is not silently rewritten. A deployment's backend label must identify one
stable configured root. Neither store nor metadata library is an agent-facing
authorization boundary; the service and external runtime must restrict access.

Reserve an object slot/hold in PostgreSQL before upload. A storage-minted receipt
is returned only after no-clobber write, file fsync and directory fsync. Publication
locks parent and child catalog rows in digest order, verifies the receipt and
current hold, requires every child to be LIVE with the expected type, inserts
deduplicated edges in a batch and marks the parent LIVE atomically. An expired
upload cannot publish. Typed edges include **all** native commit parents.

Root pins are durable and have no automatic TTL. The owning workspace/ref/backup
lifecycle releases them explicitly under its retention/drain policy. Pin creation
is part of the same transaction as the service effect, operation receipt and
outbox. Acquiring a read lease from an authorized root locks/checks the LIVE root
and creates its hold before returning its locator. The graph's edges retain all
descendants; a workspace does not copy every file row into PostgreSQL.

## Retirement protocol

Authoritative edges, pins and unexpired holds determine reachability; there is no
unchecked cached refcount. GC locks the same object row as writers. A grace timer
starts only when unreferenced state is observed, and acquiring a new reference
clears it. Age, LIST results and workspace lease expiry alone cannot authorize GC.

An eligible object moves to RETIRING and gets a durable tombstone before deletion.
New writers cannot reference that generation. The deletion worker revalidates the
catalog/tombstone before destructive IO, deletes only the generation-qualified
location, then removes outgoing edges and marks DELETED in a metadata transaction.
This releases descendants for later collection. Pending tombstones resume after a
crash; completed tombstones allow an old physical-generation cleanup to be retried.

Reusing the same logical digest increments its generation. An old worker may
leave orphan bytes in an old generation, but cannot publish them or delete the new
generation. Tombstones are retained for later cleanup of those late writes; a
bounded automatic scan/reaper and its operating costs remain to implement.

Restore must fence and stop old writers/deleters before replacing metadata and
objects. Pre-deletion validation does not make concurrent unfenced restore safe.
Backend labels cannot be casually reassigned, and shared backend configuration
validation is still a deployment task. Full backup/restore drills remain required.

## Schema and validation

Schema migration 002 adds catalog slots, typed-reference storage, root pins,
temporary holds and tombstones. Migration checksums form a chain: v1 retains its
published checksum; subsequent checksums hash the previous digest and next SQL.
Unsupported versions or changed migration history fail closed. Upgrading the
published v1 schema and retrying migration are tested against real PostgreSQL.

Tests include an independent mark/sweep reachability model, native second-parent
retention, pin-vs-GC concurrency, missing/wrong-type children, expired uploads,
crash after physical deletion, old-generation retry and a restored catalog that
invalidates an in-memory retirement permit. These are bounded tests, not proof
against every failure or a full application recovery campaign.

Remaining gates: transactionally accepted ChangeSets, application authorization,
automatic cleanup/reconciliation/dry-run, S3 conditional writes, resource limits,
kill-process tests and full restore/export. No performance advantage is claimed.
