# Import selected native source files

The development CLI can capture selected files, keep a durable local pack and
resume its upload to the local service. Completion registers a native workspace
base. It does **not** create a branch, import Git history, accept an agent's
ChangeSet or run a sandbox. For complete commit ancestry use the separate
[Git bundle import](git-import.md). Both end-to-end product modes remain unreleased.

## Capture and send

Build the binary and provision the server, storage and a human ingestion credential
as described in [local service](local-service.md) and [source uploads](source-uploads.md).
The credential needs `repository.write`. Stop writers in the input directory for
the entire capture. The service needs no access to that directory.

```sh
# Use an absolute private directory outside the source tree. Keep it through retries.
mkdir -m 700 "$TH_PRIVATE/imports"
treeharbor source-pack --directory "$SOURCE_DIR" \
  --output "$TH_PRIVATE/imports/pack-1" \
  --path src --path Cargo.toml --path Cargo.lock \
  --author-name 'Example Developer' --author-email 'developer@example.invalid' \
  --message 'Initial native source'

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-import --pack "$TH_PRIVATE/imports/pack-1" \
  --state "$TH_PRIVATE/imports/import-1.json" --authority standalone --epoch 1
```

Use the repository's actual authority and epoch. The example needs the listed paths
to exist. Each `--path` names a file or an entire directory, relative to the source
root. Paths are normalized, sorted and collapsed under selected ancestors. At
least one explicit path is required. No ignore rules, Git configuration, hooks,
filters or subprocesses are executed. Anything within a selected directory,
including untracked files and secrets, is included unless rejected as unsupported.
Inspect the selection before uploading it.

The result is a **new tree containing just that selection**. It is not a sparse
view of a different complete revision and does not imply deletions from a previous
revision. The parentless native commit uses the supplied author and message,
timezone zero, and `--timestamp-seconds` (default zero for reproducibility).
It has an empty change-operations object. These metadata are not proof of authorship
or review. The capture command prints its native commit, snapshot, unique object
count, encoded bytes and filesystem entry count. `pack_path_hex` preserves the
absolute output path bytes; `pack` is the UTF-8 string or null if unrepresentable.

After capture, upload reads only the pack. Source edits or removal no longer
change the uploaded revision. Finish returns `native:sha256:…`, usable with
`workspace-create` and a registered [source profile](source-profile-v1.md).
This creates a logical workspace; [node preparation](local-node.md) can then be
requested. Automatic scheduling is pending.
Its owner can then [list directories and download baseline files](source-reads.md)
with the workspace generation and repository epoch.

## Recovery and lifecycle

- Repeat the exact `source-import` invocation with the same pack and state file
  after a connection failure or process loss. The journal is made durable before
  any request. A retry first asks for the exact completion receipt, then resumes
  child-before-parent uploads if necessary. Completed receipts survive session
  expiry, subject to current authorization.
- The immutable state file binds repository scope, pack bytes, authority, epoch,
  budgets, TTL and an upload UUID. Changed arguments fail before a new request;
  the CLI does not overwrite the journal or silently generate a replacement ID.
  Rotated credentials for the same principal may resume; another principal cannot.
- `--ttl-seconds` defaults to 3,600 and cannot exceed it. Retries do not extend the
  server deadline. An expired or aborted incomplete session needs a deliberately
  new state filename and session. Keep the old journal for diagnosis. Changing the
  repository epoch requires a new explicitly fenced invocation as well.
- Capture requires a new output directory. Failure leaves a partial pack without
  a complete manifest; it cannot be sent or resumed as a capture. Stop writers,
  fix the cause and select a new output path. Remove only your known partial pack
  after confirming no process is still using it.
- There is no automatic spool cleanup. Once completion is confirmed and no retry
  needs it, the operator may remove the local pack and journal. Keep both if the
  outcome is unknown. Server retention is independent: an accepted base remains
  pinned; deleting a local pack is not repository deletion or server GC.

## Filesystem and resource boundaries

Linux and macOS use descriptor-relative traversal of selected descendants, with
no-follow opens and before/after identity/metadata checks. Source changes that are
detected abort capture. These checks are **not an atomic snapshot or a writer lock**;
they cannot guarantee consistency against concurrent or privileged writers. Input
ancestors, the private spool and the OS identity performing capture are trusted.
No sandbox, transaction rollback or process isolation is provided here.

Regular files and executable bits are preserved. Symlinks are stored as bytes,
never read through. Absolute and lexically escaping links are rejected. Link chains
and cycles are not resolved by capture; materialization performs its own safety
checks and can reject such a tree. Missing selected paths, traversal through a
symlink, devices/FIFOs/sockets, reserved metadata names (including nested `.git`),
and recognized Git LFS pointer headers are rejected. Submodule and LFS resolution,
extended attributes, ownership and filesystem timestamps are not imported. Name
bytes are retained where the source filesystem permits them; APFS cannot represent
all byte names supported by Linux. Destination filesystem restrictions still apply.

Defaults: 100,000 unique objects and 4 GiB encoded bytes. `--max-objects` may be
1–1,000,000; `--max-bytes` may be 22 bytes–64 GiB. The same byte budget also bounds
source data read before deduplication. Files are limited to 64 MiB, links to 4,096
bytes, each directory to 100,000 entries, the entire capture to 1,000,000 filesystem
entries and 64 MiB of accumulated path bytes. Paths use the core's 4,096-byte,
128-component and 255-byte name bounds. Pack manifests are limited to 128 MiB,
and graph validation to 2,000,000 references. These are operational limits, not
license restrictions.

The local spool contains a complete copy of the selected source and framing.
Capture and reopen verify the whole pack, and upload sends each complete frame
sequentially. Existing server objects still require session membership. Budget
disk space, repeated reads, server storage/fsync and cleanup costs accordingly;
incremental reuse, batching and performance measurements remain future work.

## Local pack and journal format, version 1

A new private directory contains a `LocalStore` under tenant `native-pack` and a
last-published private `manifest.json`. The manifest has exactly these fields:

| Field | Meaning |
|---|---|
| `version` | Integer 1 |
| `commit`, `snapshot` | Canonical `sha256:` native object IDs |
| `selection_hex` | Canonical lowercase hex paths, sorted with redundant descendants removed |
| `objects` | Unique child-before-parent IDs, all and only reachable objects; commit last |
| `encoded_bytes` | Sum of canonical frame lengths for that list |
| `filesystem_entries` | Number of selected filesystem entries, including ancestor directories |

Every frame is digest-verified, every edge has the expected type, and the commit's
snapshot must match. Extra fields, duplicate/missing objects, parents before their
children, and unrelated
objects fail validation. Formatting and field order of a read JSON manifest need
not be canonical, but its **exact bytes** identify the pack for retry. Do not edit
or reformat it after starting an upload. The fingerprint is SHA-256 of the bytes
`treeharbor-source-pack\0v1\0` followed by the raw manifest. It is a client identity,
not a source object kind. The [independent vector](../crates/service/tests/fixtures/source-pack-v1.json)
contains one `src/main.rs` file with `fn main() {}\n`, author `Test`, email
`test@example.invalid`, message `capture` and timestamp zero. Ignore the fixture's
terminal newline: the writer emits compact JSON without it. Its fingerprint is
`sha256:cb4a7d0f8f09dab7215cf4c3624c59cda21a61308b103fa275f73e9f0008f1d2`.

The private journal is JSON with exactly `version`, `tenant`, `repository`,
`authority`, `epoch`, `pack_fingerprint`, `commit`, `upload`, `ttl_seconds`,
`max_objects`, `max_bytes`. Version is 1; upload is a canonical UUID. Credentials
and the original source path are not stored. The CLI derives idempotency keys
`import-{upload}-begin` and `import-{upload}-finish`. Files use atomic no-clobber
publication and file/parent-directory fsync. Unsupported versions fail closed.
