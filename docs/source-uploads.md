# Native source uploads

This development API imports a complete canonical object graph and registers a
native commit as a workspace base. It does not move a branch, accept an agent
ChangeSet, approve a merge or prepare files. For ordinary files, use the
[native directory import commands](native-import.md), [Git bundle import](git-import.md)
or a pack from [live source export](live-source-export.md). Automatic
node/runtime preparation are still pending. The low-level commands below are
useful to clients that already encode TreeHarbor objects.

## Enable local storage

Stop old processes and run the explicit metadata migration with the new binary.
Migration 006 preserves existing profiles, credentials, pins and receipts. Add
`storage` to the private server configuration from [the local guide](local-service.md):

```json
"storage": {"root": "/absolute/private/source-store", "backend": "local-1"}
```

Create the root beforehand, owned by the service user with mode 0700. The first
start requires an empty directory, creates a durable private `backend-id`, and
binds its UUID to the configured label in PostgreSQL. Subsequent starts require
that identity. Changing the label's directory to a new empty store is refused.
Existing unbound catalog data needs an explicit adoption/migration procedure,
which is not provided yet. Do not delete/copy the marker as a shortcut around a
missing store. A complete backup/restore procedure remains a release requirement.
Omitting storage retains control-plane-only operation; it does not enable uploads.

Provision a **human** principal with `repository.write` for source ingestion;
agent credentials cannot acquire that permission. Session ownership is the
authenticated principal, never a JSON field. The following commands assume its
private credential file and a running private Unix-socket server. Workspace
creation additionally requires `workspace.write` and a registered source profile.

## Client sequence

Choose one UUID and retain it across retries. The object input is a complete
[canonical frame produced by Object::encode](../crates/core/src/codec.rs), not raw file content or a Git object.
The CLI requires each input frame to be a private regular file in a private
directory. The SHA-256 ID includes the frame header and must match its bytes.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-begin --key import-1 --authority standalone --epoch 1 \
  --upload "$UPLOAD_UUID" --max-objects 100000 --max-bytes 4294967296

# Repeat in child-before-parent order for every reachable object.
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-put --upload "$UPLOAD_UUID" --object "$OBJECT_ID" --file "$FRAME_FILE"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-finish --key finish-1 --authority standalone --epoch 1 \
  --upload "$UPLOAD_UUID" --commit "$NATIVE_COMMIT_ID"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-status --upload "$UPLOAD_UUID"
```

Upload blobs before their directories, subdirectories before parents, then
snapshots/operations before commits, and every parent commit before its child.
All typed references must belong to this session. The normal PUT path requires
complete verified bytes. `source_reuse_base` can enroll the complete graph of a
previously accepted native source base in the same repository. It uses the base's
retained pin and exact live object generations to establish membership. The
caller needs both `read` and `repository.write`. Each graph object counts toward
the session's existing object and byte budgets, and the new holds use the
session's original deadline. Use the complete PUT path for an initial import.

`client source-import` uses the first parent in a verified full-history pack
when that parent is available as a retained base. It continues with a full
upload when the server lacks the command, the caller has a write-only token,
the base is unavailable, or reuse cannot fit the upload budget. The pack and
replay journal formats stay at their existing versions. The [GitHub bridge](github-import.md)
has its own admission rule for a prior import. Archive transfers still send the
complete supplied artifact.

Finish returns `revision: native:sha256:…` and `snapshot`. Use that revision as
the workspace base. Completion commits its persistent graph pin, base binding,
session state and replay receipt together after durable storage publication.
All reachable parents are retained. Imported author/message/operation data is
not review evidence or proof that the commit was approved by anyone.

For cancellation, use `source-abort --key abort-1 --authority standalone --epoch 1
--upload "$UPLOAD_UUID"` with the same common client arguments. Abort fences late
publication and retains temporary protection until the original deadline. It
does not synchronously delete files or release an accepted base.

## HTTP and retries

Control operations use the existing `/v1/commands` envelope, version 1:

| Method | Command fields |
|---|---|
| `source_begin` | `key`, `authority`, `epoch`, `upload`, `max_objects`, `max_bytes`, `ttl_seconds` |
| `source_reuse_base` | `upload`, `base` as a canonical `native:sha256:…` revision |
| `source_finish` | `key`, `authority`, `epoch`, `upload`, `commit` |
| `source_abort` | `key`, `authority`, `epoch`, `upload` |
| `source_status` | `upload` |

The object route is
`PUT /v1/source/{tenant}/{repository}/uploads/{upload}/objects/{sha256:id}`
with `Content-Type: application/octet-stream`, the usual Bearer credential and
fixed `Host: treeharbor.local`. Repository/upload UUIDs and hex digests must use
canonical lowercase spelling. Query strings are not accepted. The successful
response is `{"version":1,"result":{"object":"sha256:…","state":"stored"}}`.

The reuse response contains the exact `upload` and `base`, with `objects` and
`bytes` for the complete enrolled graph. The command repeats safely during
the active session, returning the same graph counts without reserving its
members again. Use at most 100,000 objects per reuse. See the
[v1 request fixture](../crates/metadata/tests/fixtures/source-reuse-base-v1.json).

A timeout, broken connection or lost response may have an unknown outcome.
Retry the exact UUID/digest/bytes, or the exact control command and key. Object
retries reuse their reservation and do not consume the budget twice. Committed
control receipts replay even after later state changes; check source-status for
current state. All operations, including replay, recheck current permissions.
Credential rotation can resume the same principal's upload; a different principal
cannot take it over. Authority/epoch changes fence uncommitted upload work.

## Limits and errors

An upload has a fixed server deadline of 1–3,600 seconds; retries never extend
it. Begin and status expose `expires_at_unix_ms`; status also returns `expired`,
`reserved_objects` and `reserved_bytes`. Up to four unexpired active sessions
per repository are allowed. Each session permits at most 1,000,000 unique objects
and 64 GiB of encoded bytes; the CLI defaults to 100,000 objects and 4 GiB.
An object is at most 64 MiB of payload plus its 22-byte frame header.

The current server admits two binary upload operations at a time. Each retains
its capacity permit during blocking decoding/storage work, including after
request cancellation; excess requests return 503 `upload_busy`. Bodies have a
five-second deadline and the configured overall request deadline still applies.
Frames are bounded but buffered; streaming/chunking and throughput tuning remain
pending. These resource limits are operational controls, not license limits.

| Result | Meaning |
|---|---|
| 400 `invalid_object` | Digest, frame or canonical object is invalid |
| 401/403 | Credential invalid/revoked or missing the required permission |
| 404 | Upload not found in this repository and principal scope |
| 404 on `source_reuse_base` | Base not retained in the current repository |
| 409 `upload_membership` | Missing child, wrong child kind or incomplete commit |
| 409 `upload_state_changed` | Expired, aborted or already completed upload |
| 409 `repository_fenced` | Authority/epoch or active repository state changed |
| 409 `object_busy` | Another upload or GC owns the physical transition |
| 429 `upload_limit` | Session count, object or byte budget exhausted |
| 5xx / connection loss | Retry exact operation; do not infer rollback |

Bytes can remain after interrupted/aborted/expired uploads. Holds expire but
automatic cleanup/reconciliation is not implemented yet. Incomplete objects may
remain busy until the existing manual catalog retirement protocol runs. Completion
keeps its base pinned independently of upload expiry. This is a development
interface, not a claim of production lifecycle readiness or performance superiority.
