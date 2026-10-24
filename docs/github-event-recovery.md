# GitHub event recovery

Experimental recovery for retained `merge_group` callbacks. An owner authorizes
bounded work; an assigned bridge scans GitHub deliveries, retains content
observations and may request one redelivery per GUID in that recovery. Only the
signed receiver can insert an event. This does not establish that a change passed
checks or merged. Real installed-App acceptance remains a release gate.

## Prerequisites and permissions

Upgrade all command/receiver processes to metadata schema 24 using the existing
[administrative migration](local-service.md). Stop old processes first. The
populated schema-23 upgrade preserves existing events, grants and tokens; it does
not grant recovery access. Downgrade requires restoration of a compatible backup.

Create and verify a [GitHub binding](github-connection.md), then create a
[signed event channel](github-events.md) and configure its
[HTTPS receiver](webhook-https.md). Recovery never creates channels, changes
webhook configuration or expands their event/byte limits.

The Human owner needs `repository.write` for request/cancel and `read` for status.
The assigned Bridge needs the separately granted `host.recover` permission in
both repository access and its token. `host.read`, `host.publish` and `host.merge`
do not imply it. Agents cannot request or execute recovery. The bridge's App key
stays outside workspaces and sandbox runtimes.

Provider recovery uses App JWT authentication, with App-wide privileges. Local
repository filtering is not provider-enforced repository scoping. Each provider
operation verifies the App ID/client ID, installation/owner and exact HTTPS
callback, with JSON delivery and SSL verification enabled. Only GitHub.com is
supported; redirects and arbitrary URLs from pagination links are not followed.

## Request, run, inspect, cancel

Use the existing `treeharbor github --request request.json` command with the
appropriate `--socket` and `--credential-file`. UUIDs below are examples; choose
new IDs/keys and a real UTC Unix-millisecond start time for your deployment.

```json
{
  "action": "recovery_request",
  "key": "recover-outage-1",
  "epoch": 1,
  "recovery": "c8433e5c-0603-4c6a-9f73-0b853d6ff780",
  "specification": {
    "channel": "78821abf-a49d-4125-95f1-2a48020c2c00",
    "callback_url": "https://hooks.example.com/github/events",
    "since_unix_ms": 1792796400000,
    "ttl_seconds": 900,
    "max_pages": 10,
    "max_deliveries": 20
  }
}
```

The server fixes the end of the observation window at request creation. It binds
channel, active binding, repository epoch and assigned bridge. At most eight
unexpired, uncancelled recoveries are allowed per repository. TTL is 1–3600
seconds; each page/delivery budget is 1–100. These are safety bounds, not license
quotas. `max_deliveries` limits detail inspections, including callbacks already
present, and therefore also bounds dispatches. A page contains at most 100
provider entries, including entries filtered out of the selected repository.

Run the bridge with its App configuration from the connection guide:

```sh
mkdir -m 700 recovery-state
treeharbor github recover-events \
  --config github-app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file recovery-bridge.json \
  --recovery c8433e5c-0603-4c6a-9f73-0b853d6ff780 \
  --epoch 1 --state recovery-state
```

Reuse the same private state directory on restart. The worker locks it, binds its
immutable identity and syncs each observation before reporting it to the service.
Pending reports are flushed before reading an App key. A retained dispatch is
never sent again, even if its response was lost. After scanning is exhausted and
all details are prepared, a restart can inspect receipts without the App config.
The command returns 0 only when all observed attempts match retained signed
content, scanning reached its observed end and no retention gap is reported.
Otherwise it returns 1; inspect JSON output or the diagnostic. Exit 0 still does
not assert complete historical coverage or successful integration.

Owner status is paginated (eight attempts per page); use `next_after` as `after`
until null:

```json
{"action":"recovery_status","epoch":1,"recovery":"c8433e5c-0603-4c6a-9f73-0b853d6ff780","after":0}
```

```json
{"action":"recovery_cancel","key":"cancel-outage-1","epoch":1,"recovery":"c8433e5c-0603-4c6a-9f73-0b853d6ff780"}
```

Cancellation/expiry stop new scan, inspection and dispatch work. A report for an
already granted dispatch may still be retained while the bridge/channel/binding
remain authorized. Cancellation cannot undo a request already sent to GitHub;
there is no cross-system transaction. Channel, binding, credential revocation or
epoch changes fence bridge access. Retain its private journal for operator
reconciliation; do not invent an unsent outcome after losing access.

## States and failure behavior

| Evidence | Meaning |
|---|---|
| `page_count`, `next_cursor`, `scan_exhausted` | Durable scan progress and observed pagination end; not a coverage watermark |
| `details` | App-authenticated identity and parsed-payload digest; not a signed event |
| `dispatch` without `report` | One send grant was committed; outcome unknown, including a lost grant response |
| Report `accepted` | GitHub returned HTTP 202; callback receipt is still independent |
| Report `not_sent` | Worker completed failure before the send boundary; no automatic retry |
| Report `unknown` | No definitive response after the send boundary; no automatic retry |
| Inbox `missing` | No signed event retained under this scoped GUID |
| Inbox `needs_content_check` | A GUID alias exists, but no prepared provider-content digest |
| Inbox `payload_conflict` | Signed content differs from the selected provider payload; never accepted as recovered |
| Inbox `comparison_unavailable` | Content cannot be normalized within comparison bounds |
| Inbox `received` | GUID and parsed content match independently authenticated, durable inbox content |

Duplicate page reports are idempotent; overlapping pages preserve one attempt ID
and reject immutable identity substitution or cursor cycles. Provider statuses may
change and are retained in page observations. A GUID can have several numeric
attempt IDs; only one dispatch is allowed for it within a recovery. Another
explicit owner recovery is required to authorize a further attempt. No background
scheduler or automatic repeat policy is supplied.

A fresh detail read must match retained identity, callback and payload digest
before POST; the service rechecks current eligibility immediately before send.
The CLI refreshes each attempt because another entry may have dispatched the same
GUID or a callback may have arrived. HTTP 202 and provider success/null statuses
never write the signed inbox or set `merge_verified`.

GitHub lists a moving, retention-limited history. Exhausted page/delivery budgets,
concurrent changes, cancellation and errors leave gaps. `retention_gap` indicates
that the requested start is older than the documented three-day delivery window.
`coverage_complete` and `merge_verified` remain false for every recovery status.
No retained changes, refs, validations or integration decisions are modified.

## Version 1 wire and content comparison

Worker commands use the existing authenticated GitHub command envelope:

| Action | Additional fields | Required permission |
|---|---|---|
| `recovery_context` | `epoch`, `recovery`, `after` | `host.recover` |
| `recovery_page` | `key`, `epoch`, `recovery`, `report` | `host.recover` |
| `recovery_details` | `key`, `epoch`, `recovery`, `details` | `host.recover` |
| `redelivery_begin` | `key`, `epoch`, `recovery`, `provider_id`, `dispatch` | `host.recover` |
| `redelivery_context` | `epoch`, `recovery`, `provider_id`, `dispatch` | `host.recover` |
| `redelivery_report` | `key`, `epoch`, `recovery`, `provider_id`, `dispatch`, `report` | `host.recover` |

`DeliveryPageReport` is version 1 with `observed_at_unix_ms`, nullable `cursor` and
`next_cursor`, `scanned` and `attempts`. Each `DeliveryAttempt` has the versioned
[serialized fixture](../crates/github-contract/tests/fixtures/delivery-attempt-v1.json).
`DeliveryDetails` is `{version:1, attempt, observed_at_unix_ms, payload_sha256}`.
Page and detail shapes have an independent
[version-1 fixture](../crates/github-contract/tests/fixtures/delivery-recovery-v1.json).
A redelivery report is `{version:1, observed_at_unix_ms, outcome}`, where outcome
is `accepted`, `unknown` or `not_sent`. Unknown fields are rejected. These reports
are trusted-bridge observations, not agent evidence.

GitHub HMAC covers the raw body, **not** the delivery GUID header. Matching only a
GUID could attribute an old authentic body to another delivery. The provider's
parsed payload is therefore compared with the independently HMAC-verified inbox
body. No provider JSON or copied signature can create a `VerifiedDelivery`.

The comparison format is `payload-json-v1`: parse into `serde_json::Value`, sort
object keys by UTF-8 lexical order recursively, preserve array order, and serialize
compact UTF-8 JSON with the pinned serde_json serializer. Numbers retain that
parser's integer/floating representation; this is not RFC 8785 or raw-byte
identity. Hash `treeharbor.github.payload-json.v1` followed by a NUL byte and the
serialized bytes using SHA-256; encode lowercase hex. The top level must be an
object, nesting at most 64 and normalized bytes at most 8 MiB. Its independent
[Python-produced vector](../crates/github-contract/tests/fixtures/payload-json-v1.json)
fixes Unicode, escaping, object order, arrays and a representable fractional value.
Any incompatible normalization change requires a new format version. Original
raw-body SHA-256, signature verification and receiver provenance remain separate.

The provider detail response is bounded to 8 MiB; payload, headers and signatures
are discarded after comparison. Other provider responses are bounded to 128 KiB.
Calls share four transport slots and a 45-second outer deadline. The private
journal caps records/bytes and refuses insecure directories, symlinks, holes and
identity changes. Schema 24 retains append-only pages, details and dispatch
receipts, with transactional request deduplication and outbox entries.

## Validation and remaining gates

Tests exercise local HTTP/JWT transport, malformed and substituted provider data,
content mismatch, lost responses and no mutation retries. PostgreSQL tests cover
permission and epoch fencing, overlapping pages, cancellation, expiry during
commit, concurrent dispatch, signed-content correlation and populated upgrades.
An actual CLI/server kill test loses committed page/detail/grant/report responses,
restarts without an App key and confirms receipt through the real HMAC receiver.
Provider observations in that test are explicit fixtures, not a live App.

Remote callback URL/key changes, production privileges, provider retention and
complete delivery recovery still need real installed-App acceptance. Full queue
group/check/final-tree verification is a separate unfinished gate.

Provider references: [App webhooks](https://docs.github.com/en/rest/apps/webhooks?apiVersion=2026-03-10)
and [authenticated App](https://docs.github.com/en/rest/apps/apps?apiVersion=2026-03-10#get-the-authenticated-app),
checked 2026-10-24.
