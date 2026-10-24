# Durable GitHub publication requests

Development coordinator API. An owner can retain a publication request for an
exact [ready projection](github-projection.md); an assigned bridge can record its
verified mapping, claim one attempt and report observations after a restart.
The coordinator commands do not themselves contact GitHub. The trusted publisher
command below drives the actual branch transport. A [live synthetic acceptance](evidence/connected-live-20261024.json)
passed two fresh branch publications, PR delivery and exact head-check capture
through an installed App. Both publications recovered identically after service
SIGKILL without the App configuration or Git. Complete merge-queue acceptance and
final-tree verification remain pending; this small test does not cover every live
update, cancellation or uncertain transport outcome.

Upgrade the [local service](local-service.md) to schema 18 with its admin migration
command. Existing sources, bindings, credentials and ready projections are preserved.
No token gains new permissions. The owner requires `repository.write`; status
requires `read`. Bridge operations require the binding's assigned Bridge identity
and an explicit `host.publish` credential. An agent or `host.read` credential
cannot prepare a mapping, claim dispatch or report a publication result.

## Owner request and status

Save this as `publication.json`, replacing the projection and publication UUIDs.
The projection must be ready, unexpired, uncancelled and still select the current
accepted ChangeSet version. `expected_head: null` requires an absent remote branch;
an update instead supplies an exact `git:sha1:<40 lowercase hex>` old commit.

```json
{
  "action": "publication_request",
  "key": "publish-change-v1",
  "epoch": 1,
  "publication": "33333333-3333-4333-8333-333333333333",
  "specification": {
    "projection": "ffeeddcc-bbaa-9988-7766-554433221100",
    "expected_head": null,
    "ttl_seconds": 600
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  github --request publication.json
```

The server returns the immutable native revision, expected old Git head,
projection/binding/epoch, derived `branch_id` and service branch, state and deadline.
The deadline is at most the projection's deadline; TTL is 1–3600 seconds. These
are operational bounds. The request creates a separate persistent graph pin.
Neither this pin nor a successful branch observation grants merge authorization.

Pass these requests through the same command for status and cancellation:

```json
{"action":"publication_status","epoch":1,"publication":"33333333-3333-4333-8333-333333333333"}
```

```json
{"action":"publication_cancel","key":"cancel-publication","epoch":1,"publication":"33333333-3333-4333-8333-333333333333"}
```

One unresolved publication is allowed per derived branch. An expired request
before dispatch can be cancelled before requesting another. Expiry of an attempt
does not establish that its remote effect was absent and does not unlock it.

## Bridge protocol

Use the generic `client ... github --request` command with the assigned publisher
credential. Every action has `epoch` and `publication`; mutations also have `key`.
The wire route remains `POST /v1/commands`, `method: "github"`, with the normal
version/tenant/repository/authentication envelope.

| Action | Additional fields | Result |
|---|---|---|
| `publication_context` | None | Retained intent plus pinned binding configuration; no write permission |
| `publication_prepare` | `mapping: {branch_id, native_revision, git_revision}` | Persisted exact mapping, `prepared` |
| `publication_begin` | Fresh non-nil `attempt` UUID | Durable `receipt` plus one-shot `dispatch_granted` flag |
| `publication_dispatch_context` | Original `attempt` UUID | Current eligibility check for the active dispatcher; never a dispatch grant |
| `publication_report` | `report` described below | Persisted observation and resulting state |

Preparation must use the trusted [branch transport](github-publisher-transport.md)
converter. `branch_id` and native revision must equal the retained intent. The
metadata service authenticates the assigned bridge's mapping; it does not
independently run Git to verify a reported Git hash. The worker must not treat an
agent-supplied mapping or JSON manifest as a verified artifact.

Only the fresh successful `publication_begin` commit returns
`dispatch_granted: true`. Every idempotent replay returns the same durable receipt
with `dispatch_granted: false`. A different key cannot create another attempt.
This flag is separate from the historical receipt: a receipt is never reusable
permission to send. A driver must journal before claiming and dispatch only from
its fresh response, while checking current context. Losing that response requires
reconciliation. Outbox events are retained facts, not instructions to repeat a push.

A report contains:

- `attempt`, `branch_id`, `binding`, `native_revision`, `prepared_git`: exact values
  from the recorded attempt and mapping;
- `outcome`: `observed`, `unknown`, or `not_sent`;
- `remote_revision`: observed Git SHA-1 or null; must be null for `unknown` and
  `not_sent`;
- `observed_at_unix_ms`: positive observation time; reports cannot precede the
  attempt beyond the 15-second clock allowance, replace a newer observation with
  an older one, or claim a time more than 15 seconds in the future;
- `token_revoked`: actual cleanup result, independent of the branch observation.

`observed` with the exact prepared commit changes state to `observed`. An absent,
different or unreadable head leaves state `uncertain`. `not_sent` is only for a
trusted driver that finished without invoking push; an error or missing remote ref
is not that evidence. It cannot resolve an already uncertain attempt. The driver
must validate the upstream binding and branch before translating a transport result
into a report. Reports are trusted bridge evidence, not direct provider attestations.

## Publisher command

On a trusted Linux bridge host, use the private App configuration described in
[GitHub connection](github-connection.md). Its pinned repository and App identity
must match the owner binding. The App needs Contents-write for branch publication;
read-only source credentials are unchanged. The service credential must belong to
the assigned Bridge and grant `host.publish`.

```sh
install -d -m 700 /private/treeharbor/publications/change-1
treeharbor github publish \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/publisher.json \
  --config /private/treeharbor/github-app.json \
  --publication 33333333-3333-4333-8333-333333333333 --epoch 1 \
  --state /private/treeharbor/publications/change-1 --git /usr/bin/git
```

The command downloads the exact projection, verifies the native pack, derives
the Git bundle and records its mapping before claiming the sole attempt. A private
OS lock prevents concurrent drivers sharing that directory. Other directories
cannot claim another attempt on the server. New dispatch requires a fresh grant;
current service eligibility is checked before token creation and again before push.
These checks cannot make cancellation atomic with GitHub's external operation.

Run the same command after interruption. An existing attempt is reconciled by
reading its exact branch; the command does not retry the push. This recovery path
works on Linux and macOS without Git or the source pack. It still needs valid App
configuration for a new read, and current service authorization. Projection expiry,
version replacement or revoked binding do not erase historical recovery context.
A retained terminal result or pending local observation can be recovered without
App configuration. Terminal status alone does not prove the branch is unchanged now.

The state directory retains immutable intent, mapping, attempt, report and report
acknowledgement files. Keep it outside agent workspaces and backup it with bridge
state. Packs and temporary projection artifacts may remain after process death;
automatic cleanup is not implemented. Reports are capped at 1024 per directory;
this is a local safety bound, not a license limit. Do not modify journal records to
force a retry. No key or temporary access token is saved there.

Output is the current publication context. Exit zero means `observed`; uncertain,
cancelled or not-sent outcomes return nonzero. An uncertain result may reflect a
completed remote effect. Inspect status and rerun for observation, preserving the
original publication identity. Diagnostic errors do not contain upstream bodies
or tokens. A different observed head is retained as a fact, not overwritten.

Failed or unknown token cleanup remains separate from branch success. Revoking a
new read token cannot prove the previous write token was revoked; the driver keeps
`token_revoked: false` in that case. Known token expiry is retained locally, but
an interrupted token creation may leave expiry unknown. Operator/provider cleanup
and expiry reconciliation remain required; the command does not claim that an
unknown token has been revoked.

## Cancellation, recovery and retention

States are `requested → prepared → attempting → observed / uncertain / not_sent`.
An uncertain attempt may later become observed after reading the exact target.
Cancellation before dispatch produces `cancelled`. Cancellation after dispatch
sets `cancel_requested: true` and preserves the attempt; it does not claim rollback.
Terminal facts are immutable. All mutation keys reject a different payload.

Fresh prepare/begin checks current permission, binding, authority epoch, accepted
version and both deadlines after receipt/outbox writes before committing. Historical
replays cannot refresh those facts. Status `unexpired` is only a deadline fact,
not proof that a new write is eligible.

The same assigned bridge, with current credentials, can report an existing attempt
after projection expiry/version replacement or binding revocation. This preserves
knowledge of an external effect without permitting a new write. Credential
revocation and a changed repository authority epoch still reject access. Future
authority migration must drain and reconcile outstanding attempts first.

Failed token revocation remains visible even when the exact remote commit was
observed. A cancelled or expired request retains its source pin and audit history.
Automated archival, operator resolution of a permanently absent uncertain target,
provider-token cleanup and a complete authority-cutover protocol remain lifecycle
requirements. There is no timeout-based automatic resend.

`host_publication_state` is HTTP 409 for an unusable state, mapping or attempt.
Projection, binding, permission and epoch failures retain their existing error codes.
After an interrupted begin, inspect/replay its receipt and reconcile; do not invent
a new operation key to obtain another send.

The branch UUID is derived from SHA-256 over
`treeharbor-github-publication-branch\0v1\0`, tenant UTF-8 byte length (u64 big-endian),
tenant bytes, repository UUID bytes and ChangeSet UUID bytes. Its first 16 bytes
use the UUID version-8 and RFC variant bits. This is stable across ChangeSet versions
and separate across logical repositories/tenants. An independent vector is in
[`github-publication-v1.json`](../crates/metadata/tests/fixtures/github-publication-v1.json),
reproduced with `python3 scripts/generate-github-publication-vector.py`.

Tests exercise actual PostgreSQL and Unix-socket CLI/service restarts, including
SIGKILL before/after begin commit and loss of the original source pack. Observation
fixtures do not establish actual GitHub App publication or queue acceptance.

After the branch is observed, an owner can request [PR delivery](github-pulls.md).
This is a separate operation; branch publication does not create or merge a PR.
