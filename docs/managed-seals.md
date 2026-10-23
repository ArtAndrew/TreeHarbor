# Save changes from a managed workspace

Experimental native workspace capture through the local service and Rust node.
It builds on [managed runs](managed-runs.md), configured object storage and the
external Linux gVisor runtime. It does not implement automatic workspace cleanup,
remote transport or a complete Connected release.

An owner selects paths and requests a **seal**. The request stops a managed run;
the assigned node verifies its exact runtime is removed, captures stable files
into a private immutable artifact, publishes objects and returns `ready`.
The owner then accepts that artifact as a ChangeSet version. Acceptance does not
approve a merge or produce a passing validation. Use the ordinary
[native integration](native-integration.md) or subsequent host workflow.

## CLI workflow

Build `cargo build -p treeharbor-service --locked`. Prepare a managed workspace and
run using the managed-run guide. In the following commands, substitute the real
paths and canonical UUIDs returned by that setup. The owner's credential needs
`workspace.write` to request/cancel, `read` to inspect and `change.write` to accept.
The node's separate credential needs `runtime.control`.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_AGENT" \
  seal-request --key task-42-seal --authority standalone --epoch 1 \
  --seal "$TH_SEAL_UUID" --run "$TH_RUN_UUID" --generation 1 \
  --replace src --exclude src/generated --delete obsolete.txt

# Let the existing run worker stop and remove the runtime first.
# Execute this on its original node, with the same storage and Engine identity:
treeharbor node seal-once --config "$TH_CONFIG" --credential-file "$TH_NODE" \
  --root "$TH_NODE_ROOT" --storage "$TH_STORAGE_UUID" \
  --engine-socket "$TH_ENGINE_SOCKET"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_AGENT" \
  seal-status --seal "$TH_SEAL_UUID"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_AGENT" \
  seal-accept --key task-42-accept --authority standalone --epoch 1 \
  --seal "$TH_SEAL_UUID" --changeset "$TH_CHANGESET_UUID" --previous-version 0
```

There is no implicit wildcard expansion or `.gitignore` interpretation. Replacement
roots select whole subtrees; exclusions preserve original baseline entries, and
new excluded entries are omitted. Unselected files remain at the original base.
Explicit deletions must be absent on disk. Sparse omissions never imply deletion;
the persisted profile must cover each complete selected subtree. See the exact
[selection and capture format](capture-format-v1.md). Select only task outputs
and exclude secrets/build products explicitly; arbitrary secrets are not detected.

`seal-once` handles one eligible request at its assigned node, or returns `idle`.
It cannot capture an uncontrolled external process. A database terminal status,
expired lease or missing heartbeat alone is not proof of stopped writers. The
node checks its existing runtime journal, actual Engine identity, absent saved
container ID/name and lack of successor volume assignment. It retains storage,
slot, volume and journal exclusion through actual capture IO. Host administration
is trusted; this does not turn a filesystem workspace into a sandbox.

## State, retries and errors

Request states are `waiting`, `capturing`, `ready`, `failed`, `cancelled`.
The current node adapter leaves retryable failures in `capturing` until lease
reclaim or explicit cancellation; it does not yet classify every failure into a
terminal `failed` reason. Ready is a durable source artifact, not a ChangeSet.

Request defaults are 100,000 objects, 256 MiB encoded bytes and a 3,600-second
deadline. These are resource budgets, not license limits. Capture also enforces
filesystem, per-object and traversal limits. Nodes lease claims for 60 seconds
and renew every 15 seconds; renewal never extends the original request deadline.
Only one waiting/capturing seal may exist for a run. Cancellation does not resume
a stopped run:

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_AGENT" \
  seal-cancel --key task-42-cancel --authority standalone --epoch 1 \
  --seal "$TH_SEAL_UUID"
```

Use the same request and idempotency key after an unknown outcome. Changed payloads
under an existing key fail. `seal-status` recovers a lost ready response. A worker
that dies after durable capture reopens identical artifact bytes; it never
recaptures a changed source under that completed identity. An artifact missing its
separate node completion marker requires renewed physical removal proof.

Acceptance has its own fixed deadline (`--ttl-seconds`, default 300, range 1–3600).
Computation reads only the original base ancestors and sealed replacements, with
a 4,096-object/128-MiB scratch budget. Derived object reservations are durable and
idempotent. After an interrupted computation, retry the same request within its
deadline. After expiration, inspect the operation first, then use a new key if
no acceptance committed. A ready capture can outlive its capture deadline; new
acceptance still requires the original workspace/run and current access context.

The result includes `changeset`, `version`, `run`, original `base_revision` and
`base_snapshot`, exact `operations`, `result_snapshot`, `seal` and
`manifest_digest`. Existing ChangeSets require the exact `previous_version`.
Concurrent updates have one winner. Acceptance atomically retains source/result
objects, creates the version, supersedes old pending candidates and records one
operation/outbox result. No earlier check or review becomes valid for the new
version. Current authorization, workspace state and authority epoch are checked
again before commit; the deadline is checked after receipt/outbox writes.

Completed acceptance replay requires current permission but can recover its exact
receipt after workspace drain. New acceptance after drain is rejected. Inspect
`operation-status --key task-42-accept` with the same owner's credential if needed.
Typical errors are `forbidden`, `not_found`, `execution_state_changed`,
`repository_fenced`, `version_conflict`, `idempotency_conflict`,
`upload_state_changed`, `upload_limit` and `lease_expired`. No error authorizes
deleting an ambiguous private artifact or overwriting original files.

## HTTP v1

Requests use the private Unix socket and bearer header described in
[local service](local-service.md). Credentials and principal identity never belong
in the body. Request/status/cancel are commands in `POST /v1/commands`:

```json
{
  "version": 1,
  "tenant": "example-team",
  "repository": "00000000-0000-0000-0000-000000000001",
  "command": {
    "method": "seal_request",
    "key": "task-42-seal",
    "authority": "standalone",
    "epoch": 1,
    "seal": "00000000-0000-0000-0000-000000000002",
    "run": "00000000-0000-0000-0000-000000000003",
    "generation": 1,
    "selection": {"replace_hex": ["737263"], "delete_hex": [], "exclude_hex": []},
    "max_objects": 100000,
    "max_bytes": 268435456,
    "ttl_seconds": 3600
  }
}
```

`seal_status` takes only `seal`. `seal_cancel` takes `seal`, `key`, `authority`
and `epoch`. Path hex is lowercase and preserves raw bytes. Unknown fields and
ambiguous selection paths are rejected. The general JSON body limit is 64 KiB.

`POST /v1/workspaces/seals/accept` takes a separate bounded JSON body:

```json
{
  "version": 1,
  "tenant": "example-team",
  "repository": "00000000-0000-0000-0000-000000000001",
  "key": "task-42-accept",
  "authority": "standalone",
  "epoch": 1,
  "seal": "00000000-0000-0000-0000-000000000002",
  "changeset": "00000000-0000-0000-0000-000000000004",
  "previous_version": 0,
  "ttl_seconds": 300
}
```

The caller cannot submit replacement operations or a claimed result digest here.
The server computes the result through the shared native change engine. Its
reservation token is the first 16 bytes of SHA-256 of
`treeharbor-seal-accept-object NUL v1 NUL`, followed by u64be tenant byte length,
tenant UTF-8, repository UUID bytes, u64be key byte length, key UTF-8 and the
32-byte object digest. UUID bits are not rewritten. An
[independent vector](../scripts/generate-seal-accept-vector.py) fixes this identity.

Accepted changes use persistent pins. This increment does not implement automatic
cleanup of private capture artifacts, expired requests or workspace volumes.
Keep private node state for recovery; do not remove it based on a timeout alone.
