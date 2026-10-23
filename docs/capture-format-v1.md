# Selected-file capture formats v1

This development format is implemented by `treeharbor-core` and
`treeharbor-materializer`. [Managed seals](managed-seals.md) add owner commands,
scoped object publication and separate ChangeSet acceptance. A
successful library capture is **not** proof that an
agent stopped, a server-accepted change, CI success or permission to merge.

## Selection

`CaptureSelection::new(replace, delete, exclude)` takes byte paths. Replacement
roots select an entire file, symlink or directory subtree, including additions
and deletions below that explicitly selected directory. Delete roots explicitly
confirm absence. Unselected source paths retain their original base state.

Exclusions must be strict descendants of replacement roots. Their original base
entries are preserved; new excluded files are omitted. Excluded contents are not
read. If a missing or non-directory ancestor would hide an excluded original
entry, capture fails instead of discarding it. Use explicit exclusions for build
outputs and secrets, or select only individual task files. There is no automatic
detection of arbitrary secrets or interpretation of `.gitignore`.

Roots cannot repeat or overlap, including across replace/delete. Exclusions
cannot repeat or overlap each other. Construction sorts each group by raw bytes;
decoding rejects noncanonical ordering instead of rewriting it. All groups together
contain at most 256 paths and encode to at most 64 KiB. Paths use the core limits:
4,096 bytes, 128 components, 255 bytes per component, no absolute/relative traversal,
NUL, backslash or reserved `.git`/`.treeharbor` components (case-insensitive).
Reserved metadata discovered inside a selected directory is skipped.

The binary selection is:

```text
UTF-8 literal "treeharbor-capture-selection" followed by one NUL
u16 big endian: 1
replace group
delete group
exclude group

Each group:
  u32 big endian path count
  repeated: u32 big endian byte length, exact path bytes
```

There is no padding or trailing data. The selection digest is SHA-256 of these
bytes, displayed as `sha256:` and 64 lowercase hex digits. This digest is a
selection identity, not a canonical source object address. The independent
[selection generator](../scripts/generate-capture-selection-vector.py) produces
checked-in binary/hash vectors.

## Capture and filesystem boundary

`MaterializedTree::capture_selected` requires a trusted loaded baseline and a
`Publication` whose lifetime guard owns storage and slot exclusion. The caller
must first prove that every managed runtime writer has stopped and cannot resume.
A scan plus timestamps cannot establish that property for uncontrolled writers.
Run blocking capture on the node's bounded IO executor and retain the guard until
actual IO stops, even if an awaiting request is cancelled.

Every selected root must be fully covered by the persisted source profile.
Selecting an ancestor of a sparse root fails; missing undelivered files never
become deletes. Expand the workspace safely before capturing a larger subtree.
The library uses exact baseline entries as before-state predicates and the shared
native change-operation format, including directory replacements. Empty directory
and file/directory transitions are retained. Existing core operation semantics,
including pruning empty parents during nested leaf deletion, still apply.

Traversal uses directory-relative descriptors without following symlink ancestors.
It rejects device changes, special files, hardlinked regular files and special
permission bits. Symlink payloads must satisfy the relative, nonescaping link
contract. Source stat identity and content lengths are checked around reads;
root identity is checked before publication. These checks detect changes but do
not replace runtime quiescence. Filename support is limited by the host filesystem;
Linux tests include non-UTF8 bytes, macOS tests use supported Unicode names.

`Limits` bounds visits, path bytes, per-file bytes, total copied bytes and encoded
object bytes. Core object payloads remain capped at 64 MiB. Capture publishes
nothing on a pre-publication error. Partial private staging is not a receipt.

## Artifact manifest and objects

A completed private artifact contains the existing local-store layout and a
`manifest.json`, followed by the existing [publication receipt](local-node.md).
The destination must be private, outside agent files and supplied by the trusted
controller. Publication never overwrites an existing destination. Object frames,
manifest and publication names are synced before returning. A post-rename error
has an uncertain outcome: inspect the existing artifact on retry.

The manifest is compact UTF-8 JSON, with no trailing newline and fields in this
exact order:

| Field | Meaning |
|---|---|
| `version` | Integer `1` |
| `tenant`, `repository` | Original scope; canonical repository UUID |
| `base_snapshot` | Original canonical snapshot ID |
| `selection_hex` | Lowercase hex of the complete selection bytes |
| `operations` | Canonical ChangeOperations object ID |
| `objects` | Unique new object IDs in deterministic child-first order; operations last |
| `baseline_dependencies` | Pairs `[object ID, kind]`, sorted by ID; kinds 1 (blob) or 2 (directory) |
| `encoded_bytes` | Sum of listed canonical frame lengths |
| `filesystem_entries` | Number of visited selected entries, including excluded entries and explicit deletes |
| `filesystem_bytes` | Source file/symlink payload bytes copied, including duplicate contents |
| `path_bytes` | Sum of visited path byte lengths |

All object IDs use canonical `sha256:` spelling. JSON strings follow the normal
Rust JSON encoding, including escapes. The manifest is capped at 16 MiB; its exact
SHA-256 is bound by the publication receipt to the controller's expected request
binding. The [independent manifest generator](../scripts/generate-capture-manifest-vector.py)
constructs the expected source graph, change frames, selection and manifest hash
without executing Rust.

Baseline dependencies are intentionally not copied into the artifact. The server
must prove each dependency is reachable from this repository's pinned original
base and keep it protected while publishing new objects. A guessed digest or a
local manifest is not authorization to borrow shared storage. Artifacts cannot be
imported as complete repository backups.

`CaptureArtifact::open` requires the expected scope, base snapshot, selection and
request binding from trusted state. It verifies canonical manifest bytes, exact
hashes, source-object types, child-first membership, complete reachability and
resource limits. It rejects extra/unreferenced objects and dependencies. Reopening
never reads the mutable source directory and does not create missing store
subdirectories. The immutable bytes can therefore be retried after source files
change or disappear. Deletion, garbage collection and server acceptance still
need their separate lifecycle/retention protocols.

## Managed request and local node records (development)

Schema 13 adds durable owner-selected requests, owner-only read/cancel, a retained
base pin and a fixed deadline. A request asks an active managed run to stop; a node
can claim it only after recorded runtime removal. Claims bind the registered node,
storage, placement, run and source context. Renewals cannot extend the original
deadline, and newer claims fence older workers. Current owner grants, node
credentials, repository authority/epoch and workspace drain are checked again.
These metadata methods have PostgreSQL coverage, including a populated v12 upgrade.
They do not prove physical runtime removal.

The in-development node adapter independently reopens the existing exact runtime
journal using the current Engine identity and original specification, checks
absence by saved container ID and managed name, and rejects a successor volume
assignment. Its removed guard retains journal/volume locks through actual blocking
capture IO together with the node/slot guard. It does not detect arbitrary writers
introduced by a privileged host administrator. Actual Linux acceptance of this
new capture path is a separate required CI gate; library/metadata tests alone
do not prove it.

Private records live at `NODE_ROOT/seals/SEAL_UUID/`, outside agent mounts:

- `intent.json`: compact JSON in field order `version`, `seal`, `binding`.
- `artifact/`: the immutable artifact described above.
- `complete.json`: compact JSON in field order `version`, `binding`, `manifest`.

Both record versions are integer `1`; `seal` is a canonical UUID. `binding` and
`manifest` use `sha256:` spelling. `manifest` is the exact artifact manifest hash.
The [independent record generator](../scripts/generate-capture-intent-vector.py)
produces both vectors. Records are private, written without replacement and
fsynced. A missing intent in an existing directory requires reconciliation.

The node completion record is written only after capture and a final guard check.
It is distinct from the generic artifact publication receipt. With both artifact
and matching completion record, recovery does not inspect mutable agent files.
An artifact without the node completion record requires renewed physical runtime
and placement proof before the node may finish that record; it never recaptures
changed source bytes over the existing artifact. A completion record with no
artifact is an error. No database `ready` state or accepted version follows merely
from the existence of these files. The server publication methods below are now
implemented in development. Owner acceptance, transport commands, actual Linux
capture acceptance and physical workspace cleanup remain unfinished.

### Seal binding v1

The digest is SHA-256 of `treeharbor-seal-binding`, NUL, `v1`, NUL, followed by a
compact UTF-8 JSON array with these elements in order:

```text
1, tenant, repository UUID, seal UUID, run UUID, runtime attempt UUID,
run binding hex, workspace UUID, workspace generation, authority epoch,
authority name, node name, storage UUID, slot UUID, volume identity hex,
preparation UUID, baseline manifest hex, base revision, snapshot ID,
source profile hex, selection digest, maximum objects, maximum encoded bytes
```

UUIDs and revision/object IDs use their canonical string forms. Fields named
`hex` contain 64 lowercase hex digits without `sha256:`; the selection digest
uses `sha256:` spelling. Integers are JSON integers. The original run binding
already includes owner, runtime profile and argv. Worker lease/generation is
intentionally outside this content binding, so a new authorized claimant reopens
the same immutable artifact. Fixed deadline and current authorization are separate
metadata fences. The [independent binding generator](../scripts/generate-seal-binding-vector.py)
encodes the source profile and selection independently before hashing the context.


## Scoped server publication (development)

The assigned node binds the first verified artifact's complete declaration and
storage backend to its durable request. Retrying with a different manifest,
object order, dependencies, byte count or backend fails. The operations must cover
exactly the requested replacement and deletion roots. Node execution permission
does not grant owner ChangeSet acceptance or validation permission.

Preserved dependencies require verified read objects from a session anchored at
the run's exact original snapshot. A proof from a different session, repository or
history root is rejected, even when its bytes already exist in the same tenant's
object store. The node follows selected paths and exclusion ancestors to obtain
these proofs; it does not enumerate the full commit history. All reads are bounded
by configured entry, path and byte budgets.

New objects are reserved and published child first. References must name completed
objects in this declaration or proved baseline dependencies. Actual fsynced
receipts must match the reserved digest, backend and physical generation. Object
count and encoded-byte limits include reused new-object contents. Reservation
holds end at the original request deadline; a retry never extends it. Reclaimed
workers reuse the same reservations, while the current claim fences every write.

Finishing rejects incomplete or extraneous objects. The operations pin, immutable
`ready` receipt, operation record and outbox event commit atomically. A final
clock/authorization check after the writes rolls everything back if the lease or
request expired during the transaction. Permanent pins retain the operations and
all referenced contents after temporary holds expire. Only the exact completing
worker claim can replay its receipt; replay does not authorize new work.

Reservation tokens are UUIDs formed from the first 16 SHA-256 bytes of:

```text
treeharbor-seal-object NUL v1 NUL
u64be(tenant UTF-8 length), tenant UTF-8,
repository UUID bytes, seal UUID bytes, object digest bytes
```

No UUID version bits are rewritten. The [independent generator](../scripts/generate-seal-reservation-vector.py)
checks this stable identity across retries. PostgreSQL tests exercise physical
write/metadata gaps, reclaim, scoped dependencies, budget enforcement, current
access fences, rollback, replay and GC retention. These tests use trusted runtime
fixtures and do not establish physical stopped-file safety; the actual Linux
capture-to-acceptance scenario remains a release gate.
