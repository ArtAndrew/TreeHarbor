# Accepted ChangeSet projection for GitHub

Development API: prepare an exact accepted ChangeSet version as a retained native
commit and download its complete source graph for optional Git export. This is
the source stage of Connected publication. It does **not** create a GitHub branch
or PR, report checks, enqueue a merge, update main or grant merge approval.

Use the [local service](local-service.md), a Connected repository with an active
[GitHub binding](github-connection.md), an imported base and an
[accepted ChangeSet](change-proposals.md). The owner needs `repository.write`.
The assigned Bridge needs an explicitly issued `host.publish` credential to
export. Existing `host.read` credentials are not upgraded. App keys and GitHub
tokens are not needed for this local preparation/export stage.

## Request, prepare and export

Save `projection.json`, replacing the example identifiers with the active binding,
accepted ChangeSet and a new projection UUID. `specification.version` selects the
current accepted ChangeSet version; `epoch` selects the repository authority epoch.

```json
{
  "action": "projection_request",
  "key": "project-change-v2",
  "epoch": 1,
  "projection": "ffeeddcc-bbaa-9988-7766-554433221100",
  "binding": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "specification": {
    "changeset": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
    "version": 2,
    "title": "Update selected files",
    "ttl_seconds": 600
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  github --request projection.json

treeharbor github prepare --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  --epoch 1 --projection ffeeddcc-bbaa-9988-7766-554433221100 \
  --key prepare-change-v2

treeharbor github export --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/publisher.json \
  --epoch 1 --projection ffeeddcc-bbaa-9988-7766-554433221100 \
  --output /private/treeharbor/projected-pack

treeharbor git-export --pack /private/treeharbor/projected-pack \
  --git /usr/bin/git --output /private/treeharbor/projected-git
```

Output parents must be existing private directories; outputs must not exist.
Export writes a verified [native pack v3](live-source-export.md). Optional Git export
creates a self-contained `history.bundle`. Its `refs/heads/main` is a label inside
the export artifact, not a write to the repository's authoritative main. Original
Git ancestor IDs are independently verified and preserved; the new projected
commit receives a new Git ID. Neither native preparation nor native download
executes Git or untrusted workspace code.

For owner status or cancellation, pass one of these requests to `client ... github`:

```json
{"action":"projection_status","epoch":1,"projection":"ffeeddcc-bbaa-9988-7766-554433221100"}
```

```json
{"action":"projection_cancel","key":"cancel-projection","epoch":1,"projection":"ffeeddcc-bbaa-9988-7766-554433221100"}
```

Status requires `read`; cancellation requires `repository.write`. The assigned
publisher can request `projection_context`, which additionally includes the
binding's pinned configuration. All requests use current credentials and scope.

## Immutable content and states

The server derives the commit from retained accepted data:

- one parent: the exact native base of that accepted version;
- snapshot and change operations: the accepted result and its delta;
- author: `TreeHarbor <projection@treeharbor.invalid>`;
- time: accepted time rounded down to whole Unix seconds, timezone zero;
- message: `TreeHarbor ChangeSet {UUID} version {version}\n\n{title}\n`.

No caller supplies a result snapshot, author, timestamp or Git identity.
This author identifies the projection process; it does not verify an agent's
identity or claim human authorship. Run/ChangeSet lineage remains in metadata.

An owner request starts `pending`. Preparation reserves a deterministic object,
writes and syncs it, then atomically records `ready`, a full-graph pin and a
replayable receipt/outbox event. Cancellation changes `pending` or `ready` to
`cancelled`. A newer accepted version leaves the old record historically intact
but makes it unusable for new preparation/export. Status exposes this separately
as `is_current_version`; `unexpired` reports the fixed deadline. Every response
still has `publication_ready: false` because this API grants no upstream write.

The current backend uses schema 17. Upgrade with the existing admin migration
command before starting the new server. Existing bindings, import sessions,
credentials and completed receipts are preserved. No permissions are added.
Migrations 1–16 remain unchanged.

## Retries, access and limits

Reuse the same key and inputs after an interrupted request. A completed prepare
replays its historical receipt, including after cancellation or version replacement,
subject to current credential/authority checks. This does not make the old result
current again. Read status/context before starting new work. A different payload
under an existing key is rejected. A ready projection cannot be prepared under a
new key; retain the original prepare key for recovery.

Preparation rechecks the current accepted version, binding, epoch, permission and
deadline after filesystem IO and after transactional writes. Cancellation or an
expired/revoked context produces no new ready pin/receipt. Bridge downloads check
assignment and `host.publish` on opening, between batches and before the completed
archive is handed to HTTP. Already delivered bytes cannot be revoked; a downloaded
pack is not permission to write upstream. Human credentials and read-only bridge
tokens cannot use the publisher export endpoint.

There are at most four unexpired pending projections per repository. The fixed
TTL is 1–3600 seconds; titles are trimmed, nonempty UTF-8 without control characters
and at most 256 bytes. These are operational bounds, not licensing limits.
Full graph downloads inherit the limits and private spool costs of
[live source export](live-source-export.md). Incremental export is not implemented.
Cancelled/expired/obsolete ready projections retain their pins and history;
automated archival/retention is not implemented.

`host_projection_state` (409) means the state, version or deadline is no longer
usable. Binding/epoch/permission failures retain their existing distinct errors.
Busy storage or pool/IO capacity errors may be retried within the same deadline.
After expiry, an owner can request a new projection of the current version.
An abandoned unpublished object may first need the existing catalog retirement
procedure before another projection can reuse its digest; automatic orphan cleanup
is still pending. Completed projection pins are retained separately.

## Wire and reservation identity

The prepare route is `POST /v1/github/projections/prepare`, with JSON fields
`version: 1`, `tenant`, `repository`, `key`, `epoch`, `projection`. Export uses
`POST /v1/github/projections/export`, with the same scope/version/epoch/projection
and no key. Scope must match the credential. Unknown fields are rejected.
Prepare returns the usual `{ "version": 1, "result": ... }` envelope. Export
returns the existing binary source archive v1; the CLI verifies its expected root
and publishes a complete native pack atomically without overwriting output.

The object reservation token is the first 16 bytes of SHA-256 over
`treeharbor-github-projection-object\0v1\0`, followed by the tenant UTF-8 byte
length (u64 big-endian), tenant bytes, repository UUID bytes, projection UUID bytes
and the 32-byte native object digest. UUIDs are canonical non-nil strings at the
wire boundary. An independent vector is in
[`github-projection-v1.json`](../crates/metadata/tests/fixtures/github-projection-v1.json),
reproduced by `python3 scripts/generate-github-projection-vector.py`.

The [branch transport library](github-publisher-transport.md) can send an exact
projection using a separate scoped App token and observe the ref after uncertain
outcomes. It is not yet connected to durable publication requests. Actual GitHub
PR/check/queue publication, persisted reconciliation and an installed-App acceptance
run remain release requirements.
