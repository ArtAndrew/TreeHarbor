# Workspace cleanup (development)

Cleanup connects PostgreSQL release records to a guarded Rust node operation.
Owners request drain and inspect progress through the local HTTP service/CLI; the
assigned node performs physical removal. Local filesystem and PostgreSQL tests
include actual worker process loss. Real Linux/gVisor acceptance is a separate
required CI gate. This is development functionality, not a complete mode release.

An owner drain is a durable request to discard mutable workspace files. It blocks
new runs and change acceptance for that workspace generation. Later revocation of
the requesting owner does not undo the drain. Cleanup still requires the assigned
node's current authorization and the original active repository authority/epoch.
Epoch changes and unavailable original nodes leave resources quarantined.

## Commands and status

Use the existing owner credential and exact current generation. Capture and accept
any wanted edits before requesting deletion; drain discards mutable workspace files.

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  workspace-delete --key discard-work-42 --authority standalone --epoch 1 \
  --workspace WORKSPACE_UUID --generation 1
```

`draining` acknowledges the request, not physical deletion. Continue or recover
`node run-once` until managed execution reports removal. Then, on the assigned
node, with its node credential and original registered storage:

```sh
treeharbor node cleanup-once --config /private/treeharbor/config.json \
  --credential-file /private/treeharbor/node.json \
  --root /private/treeharbor/node --storage STORAGE_UUID \
  --workspace WORKSPACE_UUID --engine-socket /private/engine.sock

treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  workspace-status --workspace WORKSPACE_UUID
```

The Engine socket is required for a managed execution, including a removed one.
File-only preparations need no Engine, but must never host unmanaged code.
The returned `cleanup` field is null until claimed, then contains an immutable `id`
and state `claimed`, `deleting` or `deleted`. Workspace state remains `draining`
until metadata commits the physical completion. No host path or credential is
exposed to the owner. Only that workspace's owner can read its status.

Repeat `cleanup-once` with the same workspace. A completed response is returned
under current assigned-node authorization before opening the node root or reusable
slot, including while another process owns the root. `busy` means another live
cleanup claim remains; it does not mean completion. If a worker fails, its successor
waits for lease expiry and resumes the original intent. Errors retain unreleased
resources; do not manually delete metadata to bypass them. Automatic cleanup
scheduling, journal retention and node replacement recovery remain unfinished.

For native checks, the assigned validator explicitly releases its **collected**
check workspace using the existing `client native --request REQUEST.json` command:

```json
{"action":"check_release","key":"discard-check-42","epoch":1,"run":"CHECK_RUN_UUID"}
```

Use a validator credential with `validation.report`; ordinary agents cannot invoke
this action. It derives the workspace from the check, rejects uncollected runs and
preserves immutable check evidence. It does not accept a path or choose another
owner's workspace. The node then performs the same `cleanup-once` protocol.
A new release key can return the current `deleted` state; replaying an old key
returns its original result. Publication still independently checks exact candidate,
policy, assignment and current trust. Deleting files does not revoke evidence or
make a stale check valid. CI logs/build artifacts need separate retention before
operators choose to discard them; automated artifact retention is not implemented.

## Removal and recovery

The assigned node claims the exact original placement. Managed runs must already
have a terminal removal observation. The node separately opens the existing runtime
journal, matches the original Engine, profile, task and volume, and queries both
the saved container ID and attempt name. Any existing container or uncertain Engine
response blocks deletion. A missing journal is not recreated. A database flag,
expired lease or missing directory is not evidence of stopped writers.

The node retains its storage lock, slot guard, volume lock and runtime journal
through actual filesystem IO, including when an async caller disappears. The
cleanup guard verifies the stable operator placement rather than requiring the
already deleted child files. Capture retains its separate, stricter guard.

The cleanup journal is stored outside the agent mount, under
`NODE_ROOT/cleanup/CLEANUP_UUID/`. Its exact intent is admitted by the server before
unlink begins. Only the owned `tree`, `staging`, `intent.json` and valid private
record-write temporaries are removed. The registered mount and placement inode
remain in place. Unknown top-level contents or substituted identities are retained
for reconciliation.

Removal uses held directory descriptors, no-follow lookup and same-device checks.
Symlinks and special files are unlinked without opening their targets. Regular
files are never chmod'ed, including when hard-linked elsewhere. Owned directories
may have their permissions repaired to allow cleanup after an agent used mode 000.
On Linux this uses an exact O_PATH descriptor through the host's verified procfs;
the task's procfs is not used. Managed execution is supported only on Linux.

One traversal is bounded by four million entries, 64 MiB of accumulated name
bytes and 256 levels below the placement. Exceeding a budget leaves cleanup
incomplete: some entries may already be gone, but no completion or resource-release
receipt is issued. These are operational safeguards, not license limits.

Child directories and the placement are synced before the completion record. After
its durable publication, one metadata transaction marks the workspace deleted,
releases only its source pin and makes the placement reusable. Historical runs,
preparations, accepted ChangeSets, seal artifacts and published history remain.
Those retained records and artifacts have separate retention work still pending.

A retry reuses the immutable journal, never changes its deletion target, and
continues partial removal. A saved completion requires an empty original placement.
If metadata already records completion, the node returns it without opening the old
slot; it may belong to a new workspace. Runtime and file-only preparation are
distinct: the latter is not permission to run unmanaged code. Host administration
must preserve the node's exclusive control over private storage paths.

## Journal format v1

Files use compact UTF-8 JSON with no trailing newline, the field order below and
no additional fields. Hashes are SHA-256 of the exact bytes, displayed as
`sha256:` followed by 64 lowercase hexadecimal characters. UUIDs use lowercase
hyphenated text. Operator paths in these records must be representable in UTF-8;
repository entry names remain raw bytes.

`intent.json` fields, in order:

1. `version`: integer 1.
2. `binding`: the cleanup context digest below.
3. `root`: null for no physical directory, otherwise an object with `path`,
   `device`, `inode`, in that order. Paths are canonical absolute operator paths.
4. `initialized`: whether the original preparation's exact durable intent existed.

The binding is SHA-256 of `treeharbor-cleanup-binding\0v1\0` (literal NUL bytes)
followed by compact JSON for this ordered array:

```text
[1, cleanup_uuid, tenant, repository_uuid, workspace_uuid, drained_generation,
 epoch, authority, node, storage_uuid, original_preparation_intent_sha256_or_null,
 manifest_sha256_or_null, runtime_or_null]
```

The runtime value is an array of run UUID, attempt UUID, run binding digest, volume
registration digest, optional Engine ID and optional container ID. The original
preparation intent uses its existing v1/v2 bytes; its hash uses no extra prefix.

`complete.json` contains `version: 1`, `intent: SHA256(intent.json)`, and
`state: "deleted"`, in that order. Metadata binds both the intent hash and the hash
of the completion bytes. Neither record carries validation or merge authority.
Atomic record-write temporaries do not prove completion.

Independent vectors are generated by
[generate-cleanup-vectors.py](../scripts/generate-cleanup-vectors.py) and checked by
Rust tests. The format has not yet been released as a stable compatibility contract.
