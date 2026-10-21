# Immutable change proposals

Development API over the local Unix-socket service. Requires configured object
storage, schema 9 and the workspace owner's `change.write` permission. `read` is
separately required for operation-status lookup. This does not import Git, publish
a branch, run checks or grant merge rights.

The server computes the result from the run's original snapshot and explicit file
operations. The caller cannot supply a result snapshot. Unmentioned files remain
unchanged, including sparse-omitted files. Deletes are explicit.

An eligible run is running with the exact workspace/runtime binding, or terminal
with runtime removal confirmed by its managed node. Failed/cancelled/lost runs can
also propose edits. Requested/launching/stopping/drained contexts are rejected.
Owner, workspace generation, repository authority and epoch must still match.
Task exit is never Validation. This API accepts immutable bytes; it does **not**
scan or seal a mutable workspace. Stopped-file capture remains a release gate.

## CLI

Use the socket/credential from [local service](local-service.md) and the run from
[managed execution](managed-runs.md). Replace `SOCKET`, `CREDENTIAL` and `RUN` with
existing values; the request IDs below are examples.

```sh
th() { treeharbor client --socket "$SOCKET" --credential-file "$CREDENTIAL" "$@"; }
UPLOAD=00000000-0000-4000-8000-000000000001
CHANGE=00000000-0000-4000-8000-000000000002
th change-begin --key upload-one --authority standalone --epoch 1 \
  --upload "$UPLOAD" --run "$RUN" --generation 1
th change-put --upload "$UPLOAD" --file replacement.bin
```

The input is a private regular file (0600) containing raw replacement bytes. Put
the returned `object` ID into a private `operations.json`. An addition of `new.txt`:

```json
[
  {
    "path_hex": "6e65772e747874",
    "expected": null,
    "replacement": {"mode": 33188, "object": "sha256:<ID from change-put>"}
  }
]
```

Replacement/deletion requires an `expected` original mode and blob ID from a
[baseline source read](source-reads.md). `replacement: null` deletes a leaf. Modes:
regular file 33188, executable 33261, symlink 40960. Symlink content is bytes and
is never followed during submission. Directory edits and overlapping paths are
rejected. Paths are lowercase hex of repository-relative bytes, including non-UTF-8.

```sh
th change-submit --key submit-one --authority standalone --epoch 1 \
  --upload "$UPLOAD" --changeset "$CHANGE" --previous-version 0 \
  --operations operations.json
th change-status --upload "$UPLOAD"
th operation-status --key submit-one
th change-abort --key abort-one --authority standalone --epoch 1 --upload "$UPLOAD"
```

Abort applies to active sessions only. Acceptance returns version, AgentRun, original
base revision/snapshot, operations ID and computed result snapshot. A new version
uses a new upload/key, the same ChangeSet ID and its current `previous-version`.
Operations remain relative to the original run base, not the previous proposal.
Concurrent versions cannot overwrite each other.

## HTTP and recovery

The `/v1/commands` envelope supports `change_begin`, `change_status`, `change_abort`.
Begin fields match CLI flags. Binary frames use
`PUT /v1/changes/{tenant}/{repository}/uploads/{upload}/objects/{object}`. Only full,
verified canonical blob frames are accepted; `change-put` builds them. A guessed
CAS ID cannot borrow another upload/repository's bytes. Source-import endpoints
retain their separate human-only permission.

`POST /v1/changes/submit` accepts this JSON envelope; UUID placeholders must be
replaced and operations use the format above:

```json
{
  "version": 1, "tenant": "example", "repository": "<repository UUID>",
  "key": "submit-one", "authority": "standalone", "epoch": 1,
  "upload": "<upload UUID>", "changeset": "<ChangeSet UUID>",
  "previous_version": 0, "operations": []
}
```

The same request recovers its committed receipt after session closure, workspace
drain or restart, using current authorization for the same owner. Changed input
under the same key conflicts. Unaccepted requests must satisfy all current fences.
Blob/generated-object reservations survive process/response loss; repeated writes
consume no second budget entry. Durable bytes precede publication. Version, graph
pins, session completion, receipt and outbox commit together. A timeout is an unknown
outcome: retry exactly or inspect the operation key. Stored blobs are not ChangeSets.

Current bounds: 64 KiB submit JSON, 256 operations, 64 MiB per blob, 128 MiB/4,096
objects of computation scratch, two concurrent write/proposal workers per service.
Session budgets include generated directories/operations/snapshots and have a fixed
deadline of at most one hour. Four active uploads per owner/repository are allowed.
These are operational bounds, **not ELv2 license limits**. Bulk proposals, remote
streaming and adaptive quotas remain future work. Temporary holds expire at the
session deadline; accepted pins remain. Automatic orphan cleanup is not implemented.

Internal generated-object reservation identity: first 16 bytes of SHA-256 over
`treeharbor-proposal-object\0v1\0`, big-endian u64 tenant-byte length, tenant UTF-8
bytes, repository UUID bytes, upload UUID bytes and object digest bytes. This is not
an authentication capability. Independent vector: `scripts/generate-proposal-vector.py`.
