# Exact GitHub source import

Development support for importing one exact commit and its complete reachable
history into a Connected repository. An owner requests the import; the assigned
trusted bridge downloads, verifies and uploads it. Completion pins a native source
base with the original Git identity. It does not publish a branch, approve checks,
move main or establish that GitHub's main still points to the imported commit.
Connected is not yet released.

## Prerequisites

- A running [local service](local-service.md) with object storage and schema 16.
  Migration preserves existing bindings, credentials, observations and Human uploads.
- An active [GitHub App binding](github-connection.md), a Human owner credential
  with `repository.write`, and its assigned Bridge credential with `host.read`.
- An App installed on that repository with Contents read-only, plus the existing
  private App configuration and key. A prior connection observation is not access
  authorization for a new download.
- A Linux bridge host, trusted Git and the compiled `treeharbor` binary. This
  network adapter currently supports GitHub.com SHA-1 repositories. The separate
  offline [bundle importer](git-import.md) supports SHA-1/SHA-256 on Linux/macOS.
- A private operator directory, resource-bounded storage and an external process
  supervisor. Never run this credential-bearing bridge inside an agent workspace.

No extra upstream write permission or TreeHarbor `host.publish` is needed.
TreeHarbor does not use GitHub CLI credentials, environment tokens or Git helpers.

## Request and run

Write a private request file using an actual commit SHA, binding UUID and new import
UUID. Branch names and abbreviated SHAs are not accepted. Example field names and
types are also fixed in the [v1 request fixture](../crates/metadata/tests/fixtures/github-import-v1.json).

```json
{
  "action": "import_request",
  "key": "import-source-1",
  "epoch": 1,
  "import": "11111111-1111-4111-8111-111111111111",
  "binding": "22222222-2222-4222-8222-222222222222",
  "specification": {
    "git_revision": "git:sha1:7777777777777777777777777777777777777777",
    "max_objects": 100000,
    "max_bytes": 1073741824,
    "ttl_seconds": 3600
  }
}
```

The example SHA is synthetic; substitute a commit reachable on the selected remote.
The object/byte bounds apply to the full native source graph, including history.
They are resource controls, not license limits. At most four unexpired imports and
four active source uploads per repository are admitted. A retry cannot extend the
fixed request deadline or replace its Git identity, binding, bridge or budgets.

```sh
treeharbor client --socket /absolute/private/service.sock \
  --credential-file /absolute/private/owner.json \
  github --request /absolute/private/import-request.json

mkdir -m 700 /absolute/private/import-1
treeharbor github import --config /absolute/private/github-app.json \
  --socket /absolute/private/service.sock \
  --credential-file /absolute/private/bridge.json \
  --import 11111111-1111-4111-8111-111111111111 --epoch 1 \
  --state /absolute/private/import-1 --git /usr/bin/git
```

Use one private state directory per import; keep it for retries. It contains an
immutable request intent, an exclusive process lock, a verified full Git bundle
and the converted native pack. It contains source code and must be protected like
the repository. No App key or token is persisted there. After successful completion
the server holds the accepted source graph independently of these local files.

The bridge compares its App pins with the server binding, requests fresh access
scoped to one repository and independently verifies that scope. It fetches into a
new private bare directory, checks Git objects, creates a self-contained bundle,
and converts all reachable commits/trees into a native pack. Git hooks, checkout,
submodule execution, credential helpers, redirects, environment proxies and
automatic maintenance are disabled. TLS verification stays enabled. A failed
fetch directory is never accepted as a completed download.

Fresh retries may mint another token. After a known fetch outcome the client
attempts revocation, and a revocation failure prevents success. Lost token responses,
caller cancellation or process death can leave a temporary token until upstream
expiry. Server revocation cannot physically stop an already-running GitHub request.
Use App/installation revocation and process supervision for emergency isolation.

## State, retries and cancellation

States are `pending` → `receiving` → `completed`; pending/receiving imports may
become `cancelled`. Expiry is reported independently as `unexpired: false` and
blocks new effects. Completed and cancelled identities cannot be reused.

```json
{"action":"import_status","epoch":1,"import":"11111111-1111-4111-8111-111111111111"}
```

```json
{"action":"import_cancel","key":"cancel-import-1","epoch":1,"import":"11111111-1111-4111-8111-111111111111"}
```

Submit either with `client ... github --request FILE`. Status requires `read`;
cancellation requires the Human owner's `repository.write`. Cancellation does not
delete already accepted source history or stop an in-flight network operation.

Repeat `github import` with the same ID, epoch and directory after interruption.
The current server binding, assignment, epoch and credentials must still permit
the operation. An incomplete fetch starts fresh; a completed local bundle requires
fresh App access before reuse. Object uploads and completion resume idempotently.
A completed server import can be read without the App key or local state directory.
Both a fresh completion and a completed retry print the same
[version 1 completion result](../crates/metadata/tests/fixtures/github-import-completion-v1.json):
`version`, `import`, `outcome: "completed"`, `git_revision`, native `revision`,
`snapshot` and `publication_ready: false`. A retry reads the already registered
source; it does not extend the deadline or repeat a GitHub download. This is a
historical result, not a claim about the current remote branch.
For an incomplete expired/cancelled import use a new UUID and state directory.
Do not remove an active import directory; stop its supervisor before clearing failed local data.
Automatic removal of abandoned private staging directories is not implemented.

Same-key mutation retries return retained receipts, which are historical effects,
not fresh authorization. Current context reads still enforce binding/credential
revocation. `publication_ready` always remains false. Token revocation, changed
grants, epoch changes, binding revocation and cancellation are rechecked when object
bytes are published and before source acceptance commits. Expiry after receipt or
outbox writes rolls back the transaction.

## Adapter protocol

The command API is the existing authenticated `github` method. Unknown fields are
rejected; responses use the `version`/`result` envelope. Assigned-Bridge actions:

| Action | Fields in addition to `action` | Effect |
|---|---|---|
| `import_context` | `epoch`, `import` | Current binding, exact request/state and `completed_result` |
| `import_begin` | `key`, `epoch`, `import`, `native_commit` | Fix native root and begin bounded upload |
| `import_finish` | `key`, `epoch`, `import` | Verify full membership, pin base, retain receipt |

`completed_result` is null before completion. Once completed, it contains the
same v1 result as `import_finish`, derived from the retained import and its
registered source snapshot. Current binding, bridge assignment, credential and
authority checks still apply. No migration is needed; upgrade the service and
CLI together. A new CLI refuses a completed response from an older server that
does not supply this field, rather than printing a different success shape.

Upload canonical native frames child-first with authenticated binary PUT requests
to `/v1/github/TENANT/REPOSITORY/uploads/IMPORT/objects/OBJECT` using the normal
source framing. The import ID is also the upload ID. `native_commit` and `OBJECT`
use native `sha256:` object IDs. Human source-upload routes remain separate and
cannot be used by a Bridge. A stored-object acknowledgement is not source acceptance.

Finalization derives the root's GitOrigin from the verified native frame and
compares it with the owner's requested Git revision. It requires the complete
registered graph before pinning. The assigned converter is trusted to verify raw
Git hashes, parents, trees and replayed changes; GitOrigin comparison alone is not
cryptographic proof of a GitHub exchange.

An import result includes `git_revision`, native `revision` and `snapshot`. The
source can then be prepared as workspace files or downloaded through
[live source export](live-source-export.md) and restored with [Git export](git-export.md).
Common failures include HTTP 409 `upload_state_changed` for cancelled/expired work,
`host_binding_state` for inactive bindings, membership/limit errors and idempotency
conflicts for changed inputs. Retry the same request after an unknown transport
outcome; inspect status before creating a replacement.

## Resource and verification boundaries

The download has a 300-second maximum wall deadline, monitored aggregate disk
usage and Linux per-process CPU, address-space (8 GiB), file-size and descriptor
limits; stricter inherited limits are retained. Token/API checks have their own
bounded timeouts. The supervisor kills the Git process group on timeout and
detects loss of its parent. This does not replace a cgroup or sandbox: aggregate
RAM/CPU, hard volume quota, network policy, and teardown after arbitrary supervisor
failure belong to the external runtime. Disk polling may overshoot. Bundle and
native conversion temporarily duplicate data; budget for both and for server data.
Offline conversion has its own logical limits and deadline; the download's Linux
limits do not apply to every later bridge process.

Tests include real Git smart HTTP with synthetic credentials, redirect rejection,
process loss, exact history conversion, partial uploads, server restart, revocation,
wrong Git identity, concurrent completion, expiry during writes and a populated
schema upgrade. A local transport fixture is not an installed-App acceptance test.

On 2026-10-24, the actual App adapter imported the pinned root commit of the public
[synthetic acceptance repository](https://github.com/TreeHarbor/TreeHarbor-acceptance).
A trusted Linux aarch64 bridge used the selected installation; the service retained
15 native objects. Native export and ordinary Git restoration reproduced the exact
original commit, tree and all seven files; `git fsck --strict` passed. The
[retained result](evidence/github-app-import-20261024.json) contains the exact
identities and scope of this check. This small history contains one commit.
The operator connected the local bridge VM to the service with an SSH Unix-socket
forward. This is acceptance infrastructure, not a remote product API or an agent
sandbox acceptance run. Linux x86-64 remains the tested agent runtime platform.

SIGKILL/restart preserved the completed source. A retry with absent App config,
Git and local import state recovered the same identity. This test exposed differing
CLI output shapes; an upgraded Linux client and server returned the exact initial
v1 completion result when the same live import was retried without those inputs.
PR publication, checks, signed callbacks, native GitHub queue and final merge
confirmation remain release gates. No performance advantage is claimed.
