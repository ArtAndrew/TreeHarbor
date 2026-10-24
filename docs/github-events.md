# Signed GitHub event intake

Development feature, not a complete merge-queue verifier. The dedicated receiver
accepts signed `merge_group` facts and App `ping` events. It stores exact raw bytes,
a SHA-256 digest, normalized group identity, local receive time and an outbox record
in one PostgreSQL transaction before acknowledging a group delivery.

A signed event does **not** prove PR membership, successful checks, approval or a
final main-tree result. `destroyed` with reason `merged` is retained as a provider
fact; it does not mark TreeHarbor's queue `merge_verified`. Creation and destruction
are separate records, even when delivered in reverse order. Sequence numbers and
receive times describe local arrival, not GitHub causality.

## Authorize a repository channel

First configure and successfully observe a [GitHub binding](github-connection.md).
The human owner's repository credential needs `repository.write` to create or
revoke a channel. Agents and bridge credentials cannot subscribe repositories.
A database upgrade does not create subscriptions or widen existing permissions.

Save this request as a private JSON file, replacing the UUIDs with your binding
and a newly generated channel UUID:

```json
{
  "action": "event_channel_create",
  "key": "subscribe-github-events-1",
  "epoch": 1,
  "channel": "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb",
  "specification": {
    "binding": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
    "max_events": 10000,
    "max_bytes": 1073741824,
    "max_aliases": 4
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/control.sock \
  --credential-file /private/treeharbor/owner.json \
  github --request /private/treeharbor/subscribe.json
```

One active route maps a configured App/installation/repository identity to exactly
one native tenant, repository, binding and authority epoch. A signed body's owner
ID must also match. Headers cannot choose a tenant. Revoked bindings/channels and
changed authority epochs fence intake. Old records remain retained.

`max_events` (1–1,000,000), `max_bytes` (1–10 GiB of raw payloads) and `max_aliases`
(1–16 delivery UUIDs per identical body) are immutable operational retention
budgets. They are not license or usage-pricing limits. Database/index/outbox space
is additional to `max_bytes`. Monitor all database disk usage. On exhaustion new
content is rejected, not silently discarded. Existing exact delivery retries still
succeed. Automatic retention/archiving and budget updates are not implemented;
plan capacity before production use. Revocation alone does not free disk space.

## Run the receiver

The existing Rust binary has a separate `webhook-serve` command. It needs a private
PostgreSQL connection and an App webhook secret, **not** an App signing key,
installation token or repository write token. Keep the general command server on
its separate private socket.

Create an owner-private configuration file and secret file. The secret must contain
32–256 UTF-8 bytes without control characters or a trailing newline. Use a randomly
generated high-entropy secret, configured identically in the GitHub App. Length
validation does not prove entropy. Use a distinct secret for each GitHub App:
merge-group bodies do not independently identify the App, so the configured
App-to-secret association is trusted operator configuration. All paths below are
operator configuration.

```json
{
  "database": {
    "database_url": "host=/private/postgres dbname=treeharbor",
    "schema": "public",
    "socket": "/private/treeharbor/github.sock",
    "pool_size": 4,
    "max_connections": 16,
    "request_timeout_ms": 10000
  },
  "app_id": 123456,
  "keys": [
    {
      "id": "cccccccc-cccc-4ccc-8ccc-cccccccccccc",
      "secret_file": "/private/treeharbor/github-secret"
    }
  ]
}
```

```sh
treeharbor webhook-serve --config /private/treeharbor/webhook.json
```

The socket and its parent are restricted to the current OS user. Run a trusted TLS
reverse proxy with appropriate socket access. Proxy only `POST /github/events`;
`GET /health` is process liveness, not evidence that deliveries can commit. Public
TLS ingress can use the [Caddy deployment recipe](webhook-https.md). Public DNS,
certificate provisioning and automatic App installation are not included in this
command. The receiver must not share credentials or its socket directory with
agent runtimes. Its database connection is trusted and currently has the same
schema privileges as the local control service.

The proxy must preserve original body bytes and the single-valued
`X-Hub-Signature-256`, `X-GitHub-Event`, and `X-GitHub-Delivery` headers. It must not
rewrite JSON or decompress payloads on behalf of this endpoint. Body limit: 1 MiB;
headers: 32 and 8 KiB. Concurrent connections, database pool and deadlines are
bounded. Oversized deliveries fail explicitly and need operational reconciliation.
Only the body is HMAC-authenticated; event/UUID headers are consistency and replay
inputs, not authorization. The normalized v1 group fields are documented in
[the golden fixture](../crates/github-webhook/tests/fixtures/checks-normalized.json).
Unknown provider extension fields are ignored; duplicate consumed fields,
unsupported actions/reasons and inconsistent identities are rejected.

Up to two configured keys support a deliberate rotation overlap. Use a new key
UUID for each new secret; UUIDs are operator audit labels, not cryptographic key
identities. Replace the private configuration and restart to change accepted keys.
Removing an old key from configuration makes its signatures fail after restart.
Existing accepted data is retained. No live reload or remote secret manager is
implemented. Ping requires signed `hook.app_id` to match the configured App.

## Delivery recovery and inspection

- `200 durably_received`: raw group body, normalized fact, delivery mapping and
  outbox committed. `200 ping_verified` does not create a group record.
- `401`: signature not accepted by configured keys.
- `409`: unavailable channel/binding/epoch or one delivery UUID reused for other bytes.
- `422`: unsupported or invalid authenticated event.
- `503` / `504` or a lost connection: not an acknowledgement; outcome may be unknown.
  Retry exactly the original body and delivery UUID after resolving the problem.

The same body and channel always return one logical event, even with a new delivery
UUID (up to `max_aliases`). A known UUID with different bytes is rejected. Secret
rotation does not duplicate content. Revocation fences even old delivery retries.

GitHub does not automatically redeliver failed webhooks. An operator must arrange
redelivery, inspect delivery failures and account for missing events. Automatic
App-delivery backfill/reconciliation is still pending; this inbox alone cannot
establish that all provider events were received. See
[GitHub's failed-delivery guidance](https://docs.github.com/en/webhooks/using-webhooks/handling-failed-webhook-deliveries).

Use the same authenticated client command with `read` permission to page retained
normalized facts (eight per page). Follow `next_after` when non-null:

```json
{"action":"event_page","epoch":1,"channel":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb","after":0}
```

Revoke using the human owner's `repository.write` credential:

```json
{"action":"event_channel_revoke","key":"unsubscribe-github-events-1","epoch":1,"channel":"bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb"}
```

The raw body is retained in PostgreSQL, not returned in these bounded status pages
or emitted in process logs. Treat database backups as confidential source metadata.
Include channels, events, aliases, operations and outbox consistently in any backup;
a complete tested product backup/restore workflow remains a release gate.

## Verified scope

Independent standard-library HMAC vectors and GitHub's published vector exercise
signature verification, raw Unicode bytes, malformed/duplicate fields and identity
checks. PostgreSQL tests cover atomicity, concurrency, quotas, replay, revocation,
tenant routing and populated schema upgrades. A process test kills the actual
receiver before and after commit and repeats the original delivery.
These use synthetic provider payloads. Installed-App callbacks, complete merge-group
membership/check/final-tree proof and public TLS deployment remain unverified.
