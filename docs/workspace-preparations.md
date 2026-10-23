# Workspace preparation reservations

The metadata library can now reserve a workspace's physical preparation before a
node touches its files. This is an internal, authenticated Rust control-plane
interface. The [local node CLI](local-node.md) now drives this interface through
real file preparation and restart recovery. It does not allocate bounded physical
disks, launch runtimes, schedule continuously or expose remote node operations.

## Identity and ownership

An operator registers an existing `Node` principal with a persistent storage UUID
using `Metadata::register_node_storage`. Retrying the same registration succeeds;
changing its UUID or assigning that UUID to another node fails. A storage UUID is
an identifier, not proof of filesystem ownership or isolation. The local node also
verifies its private durable identity, physical root and exclusive OS lock.

Each reservation binds:

- tenant, repository, workspace, workspace generation and authority epoch;
- immutable base revision, snapshot and source profile;
- node principal, registered storage UUID and slot UUID;
- preparation event and a stable attempt UUID.

One workspace generation has one attempt. One storage slot cannot belong to two
active attempts, including attempts in different repositories. Slot ownership and
source pins remain retained during drain. The separate [cleanup protocol](workspace-cleanup.md)
releases them only after durable physical deletion and preserves historical
preparation rows. Lease expiry is never a reason to reuse a directory.

## Node operations

All methods below require a current repository-scoped `RuntimeControl`
authorization issued to a `Node`. They recheck principal status, repository grant,
token revocation/expiry and relevant fences inside their transactions.

| Rust method | Effect |
|---|---|
| `claim_preparation(auth, storage, worker, ttl)` | Claim a due native preparation event in this permitted repository; skip unrelated events and work assigned to another node |
| `reserve_preparation(auth, job, slot)` | Persist ownership before filesystem work and return the exact source context |
| `renew_preparation(auth, plan, ttl)` | Extend only a current reservation whose workspace has not begun draining |
| `preparation_status(auth, workspace)` | Inspect this node's durable assignment for recovery planning; does not issue a live lease |
| `prepared_workspace(auth, workspace)` | Inspect a current file-ready placement with the exact source/profile/manifest and no runtime binding; does not authorize execution |
| `complete_preparation(auth, plan, files)` | Record a durable manifest observation and atomically complete the event |

Claims use the actual stored event, including worker, generation, payload and
database-time lease. An expired or modified caller-side `Job` is insufficient.
Git-compatible workspaces, unresolved profiles and old repository epochs are not
claimed by this native worker interface. Their events remain available for their
appropriate future handler or explicit reconciliation.

A worker reconnecting after an unknown reservation result inspects the stored
assignment and reserves the **same slot** under its current claim. It receives the
same attempt UUID. Changing the slot fails. A different node cannot adopt the
workspace simply because the first worker stopped renewing. A node controller
still needs exclusive local ownership of its filesystem/runtime operations; these
database checks cannot stop an already executing process.

## File readiness and execution

The node supplies the exact snapshot and profile plus SHA-256 of the durable
[manifest bytes](workspace-manifest-v1.md). A mismatch with the reservation fails.
The manifest hash is an observation of a local file, not a canonical source object
in the object store. The metadata layer does not read or validate that file itself.
Only the trusted materializer/controller may supply this observation.

Completion moves the reservation from `reserved` to `prepared`, records the
completing worker generation, marks the workspace `ready` and completes the outbox
event in one transaction. Here **ready means prepared files**. The workspace's
runtime binding remains empty. It is not a running sandbox or an accepted check.
The legacy running-agent callback refuses this state; a separate persisted run
attempt and verified external runtime binding are still required.

An identical completion can be retried while that exact workspace is still ready.
A different manifest, superseded worker, revoked credential, changed authority
epoch or draining workspace cannot reuse the earlier successful observation.
Prepared observations and source/ownership bindings are immutable in the database.

Drain increments the workspace generation first and changes its reservation to
`draining` in the same transaction. Preparation completion and renewal then fail.
No files, reservations or source pins are deleted. Inspection remains possible
for an authorized node, including after an epoch change, so a controller can plan
reconciliation without treating that read as permission to act.

## Migration and evidence

Migration 007 adds the registry and reservations. Existing workspaces and receipts
are preserved; no synthetic assignment or manifest is backfilled. Updated service
startup requires the current schema and rejects an old schema until the explicit
administrative migration has run.

The real PostgreSQL suite covers duplicate and concurrent reservations, reconnect
and reclaim, cross-node/tenant denial, source and manifest mismatch, revocation,
authority changes, drain, retained pins/slots, immutable records and populated-v6
upgrade. A database lock pauses insertion after initial lease validation; expiry
before publication rolls back the entire reservation. These are metadata tests,
not evidence of a complete physical node or runtime recovery path.

The local node separately tests materializer publication and physical recovery.
Remaining integration work includes actual volume capacity enforcement, separate
runtime launch attempts, process stop observations, safe drain/removal and release
of retained resources. See the
[runtime supervisor](runtime-supervisor.md) for the independently exercised
external process-lifetime primitive.
