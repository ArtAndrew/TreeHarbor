# Selected-file capture formats v1

This is a development library primitive in `treeharbor-core` and
`treeharbor-materializer`. It has no owner-facing seal command yet. Durable seal
requests, node/runtime proof, server object publication and ChangeSet acceptance
are still being integrated. A successful library capture is **not** proof that an
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
