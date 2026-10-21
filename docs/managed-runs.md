# Managed AgentRun development contract

Schema 8 connects owner requests, prepared source identities and the local runtime
controller. The metadata/API path has PostgreSQL contracts. The node/CLI execution
adapter is under integration testing: a complete service-to-gVisor demonstration
is still required before treating this increment as usable execution infrastructure.
The earlier controller harness proves its own runtime boundary, not this wiring.

## Permissions and placement

An owner with `workspace.write` requests or stops a run. Reading its status needs
`read` and the same owner. The request supplies a prepared workspace generation,
an approved runtime profile digest, bounded UTF-8 argv and agent/task labels.
Repository authority and epoch come with the mutation. Client JSON cannot select
a host mount, Engine socket, image, node, identity or unrestricted runtime flags.
The profile is distinct from the workspace's full/sparse source-delivery profile.

A trusted operator approves a canonical [runtime profile](runtime-controller.md)
and registers an existing independent bounded ext4 volume to a node storage UUID
and one slot UUID. Registration requires an empty private placement directory
below the mount; the mount root and its `lost+found` are not a workspace.
Node journals and credentials must be on another filesystem. Registration never
creates a mount, adopts old files, or converts an already reserved ordinary slot.
The database serializes registration against preparation reservations. Repeating
an identical registration is allowed; changing its physical identity is refused.

Use `node volume-register --config ... --credential-file ... --root ... --storage ...
--slot ... --mount ... --placement ... --limit-bytes ...` before
`node prepare-once --config ... --credential-file ... --root ... --storage ... --slot ...`.
These are trusted operator commands, not agent tools. A legacy directory-only
preparation remains useful for source delivery and cannot launch a sandbox.

Approve or revoke a runtime profile with trusted administration:

```text
treeharbor admin --config CONFIG runtime-profile-register --tenant TENANT --repository UUID --file PROFILE_JSON
treeharbor admin --config CONFIG runtime-profile-revoke --tenant TENANT --repository UUID --profile-digest sha256:DIGEST
```

The input profile is a private JSON file with the runtime `Definition` fields.
Human formatting is accepted; the database stores its canonical version-1 encoding.
No mutable image tags, injected mounts, environment variables or fallback runtimes.

## Owner API and CLI

The existing authenticated Unix-socket `/v1/command` envelope is unchanged:
`version`, `tenant`, `repository`, `command`; the credential is outside JSON.
The `run_request` command has:

```json
{
  "method": "run_request",
  "key": "agent-task-42",
  "authority": "standalone",
  "epoch": 1,
  "run": "00000000-0000-4000-8000-000000000042",
  "workspace": "00000000-0000-4000-8000-000000000001",
  "generation": 1,
  "profile_digest": "sha256:REPLACE_WITH_APPROVED_DIGEST",
  "argv": ["/bin/tool", "task-42"],
  "agent": "example-agent",
  "task": "task-42"
}
```

The digest placeholder must be replaced by the actual registration result. This
request stores the immutable run/attempt and queue event before any runtime call;
`requested` does not mean execution has begun. Preserve the request key and UUID
on retry. A changed body under the same key is an idempotency conflict.

```text
treeharbor client --socket SOCKET --credential-file OWNER run-request --key KEY --authority standalone --epoch 1 --run RUN_UUID --workspace WORKSPACE_UUID --generation 1 --profile-digest sha256:DIGEST --agent example-agent --task task-42 -- /bin/tool task-42
treeharbor client --socket SOCKET --credential-file OWNER run-status --run RUN_UUID
treeharbor client --socket SOCKET --credential-file OWNER run-stop --key STOP_KEY --authority standalone --epoch 1 --run RUN_UUID
treeharbor node run-once --config CONFIG --credential-file NODE --root NODE_ROOT --storage STORAGE_UUID --engine-socket ENGINE_SOCKET
```

`run_status` JSON contains only `method` and `run`. `run_stop` adds `key`, `authority`
and `epoch`. Status returns run/workspace/generation/epoch, lifecycle state,
`stop_requested`, optional observed `exit_code`/`oom_killed`, and `runtime_removed`.
It does not disclose a host path or grant Validation/admission authority.

## Lifecycle and failure boundaries

One managed attempt is allowed per prepared workspace generation in this increment.
A new execution needs a new workspace; slots and source pins are retained after
completion. Automatic recycling, stopped-file capture and safe deletion are later
boundaries. Finished runs cannot masquerade as running to bypass legacy intake.

`requested -> launching -> running -> stopping -> succeeded/failed/cancelled/lost`
describes the common path. Fast tasks can be observed already exited, and a request
can be cancelled before starting. `launching` records logical start permission;
`running` requires a verified runtime observation. Exit observations are immutable.
Only confirmed removal of the exact owned runtime permits a terminal result.
An unobserved outcome is `lost`, never inferred successful. A policy-driven stop
is `cancelled`. Task exit, even zero, is not source Validation or merge permission.

The node holds an exclusive local placement lock and a renewable database job
lease. Reclaim keeps the same immutable attempt and binding. An expired worker
cannot admit, record or finish execution. Completion stores the exact completing
worker and lease generation atomically with the result and completed queue event;
the same worker can recover a lost response after reconnect. An older lease cannot
claim its successor's completion. Credential revocation is checked on replay too.

Owner account/grant changes, profile revocation, workspace drain, and repository
authority/epoch changes fence subsequent launch admission and request removal.
Owner-policy locks and final lease/authorization checks cover the commit boundary.
The original request token expiring or being revoked does not cancel an already
accepted request: stop the run or revoke the owner's grant/account/profile policy.
The executing node credential must remain valid.

PostgreSQL and the runtime have no shared transaction. A previously admitted
Engine RPC may complete after drain or worker death. The journal prevents a second
uncertain start; cleanup targets the exact verified container ID. If exact removal
is confirmed after an uncertain start, the managed path may finish `lost` or
`cancelled`; it does not fabricate an exit result. Unknown creation without an
identifiable container retains ownership for recovery. Neither a timeout nor an
absent heartbeat proves removal. Quiescent workspaces, pins and physical volumes
remain retained. Host/engine replacement needs a future fenced recovery procedure.

## Persistent formats

All encodings below use compact UTF-8 JSON, lowercase hyphenated UUID strings,
integer numbers and no trailing newline. Strings use JSON escaping without
ASCII-escaping other Unicode characters. Unknown or noncanonical local records
are refused. These digests are identity bindings, not authentication signatures.

* Node volume record v1 has fields in order `version`, `storage`, `slot`, `volume`.
  The nested identity fields are `mount`, `files`, `device`, `mount_inode`,
  `files_inode`, `mount_id`, `capacity_bytes`, `limit_bytes`. Its digest is SHA-256
  of `treeharbor-node-volume\0v1\0` followed by these JSON bytes (`\0` is a NUL byte).
* Volume-backed preparation intent v2 retains every ordered field of
  [intent v1](local-node.md), changes `version` to 2 and appends `volume` as
  `sha256:` plus the lowercase volume-record digest. Its binding is SHA-256 of
  these JSON bytes, as for intent v1. Directory-only v1 bytes remain unchanged.
* Run binding v1 is SHA-256 of `treeharbor-run-binding\0v1\0` followed by the JSON
  array `[1, tenant, repository_uuid, run_uuid, attempt_uuid, preparation_uuid,
  owner, authority, epoch, generation, profile_digest_bytes, argv, volume_digest_bytes]`.
  Each digest is an array of 32 unsigned decimal byte values. The preparation UUID
  binds the immutable exact source revision, snapshot, profile, node and slot.

`scripts/generate-managed-run-vectors.py` is an independent Python reference.
The Rust tests consume its checked-in bytes/hashes, including Unicode/quoted argv.
Metadata tests exercise real PostgreSQL, concurrent duplicate delivery, stale
claims, terminal replay, mid-transaction lease expiry, owner revocation locks,
policy fences and populated schema upgrade. Their synthetic trusted preparation/
removal inputs do not substitute for a service-to-real-runtime integration test.
