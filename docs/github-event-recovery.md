# GitHub event recovery transport

Development library only. There is no recovery command, durable scan ledger or
automatic missed-event reconciliation yet. The existing signed receiver remains
the only path into the event inbox. This component is not an installed-App
acceptance result or proof that a change was checked or merged.

The trusted Rust bridge exposes `Connection::list_webhook_deliveries` and
`Connection::redeliver_webhook`. They use the configured App's JWT and fixed
GitHub.com API. An App JWT has App-wide provider access; local repository filters
do not make it a repository-scoped installation credential. Keep the private key
outside agent workspaces and runtimes.

Each invocation checks the authenticated App ID and client ID, installation and
owner, and exact operator-pinned HTTPS callback URL. JSON delivery and SSL
verification must be enabled. It never changes webhook configuration. The
callback must use `/github/events`, with no credentials, query or fragment.

Listing returns one page of at most 100 attempts, across all statuses. Only
`merge_group` entries for the selected installation/repository are returned.
`scanned` includes other entries so callers can bound total work. A continuation
is extracted from a validated GitHub delivery Link and encoded as an opaque query
parameter; provider URLs are never followed. Repeated current cursors, ambiguous
links and limit/parse errors fail the call. A missing next cursor does not prove
time-range completeness, inbox receipt or availability beyond provider retention.

Numeric delivery IDs identify attempts; delivery GUIDs correlate callbacks.
The shared `DeliveryAttempt` contract preserves scope, action, delivery time,
redelivery flag, nullable status and throttle time. Its serialized shape is
covered by a [fixture](../crates/github-contract/tests/fixtures/delivery-attempt-v1.json).
Provider success and pending statuses are observations, not inbox acknowledgments.

Redelivery requires an exact previously selected attempt. It rereads that attempt,
compares identity and callback URL, then calls the caller's final eligibility
check before a single POST. The caller must retain durable intent before invoking
it. An empty HTTP 202 produces a receipt with `callback_confirmed: false`.
Cancellation, process death or a lost response must leave an unknown outcome in
the future ledger; the transport does not retry the mutation. Only failures that
complete before the send boundary can report `mutation_not_sent: true`.

Detail responses can contain parsed request payloads and signatures. They are
bounded to 8 MiB and discarded, never reserialized into a `VerifiedDelivery` or
returned in a receipt. Other responses remain bounded to 128 KiB. Calls share a
four-slot limit and a 45-second outer deadline, with shorter connection/request
deadlines. These are operational limits, not license restrictions.

Remote configuration can change after observation; no local transaction fences
GitHub settings. The behavior of redelivery after URL/key changes and complete
callback recovery still require real installed-App acceptance. Durable owner
intent, recovery-specific bridge permission, scan resumption, dispatch records,
inbox correlation and recovery CLI remain to be connected before operational use.

Provider references: [App webhooks](https://docs.github.com/en/rest/apps/webhooks?apiVersion=2026-03-10)
and [authenticated App](https://docs.github.com/en/rest/apps/apps?apiVersion=2026-03-10#get-the-authenticated-app),
checked 2026-10-24. Local HTTP fixtures exercise the actual Rust transport and
JWT generation, but are not evidence of a live provider installation.
