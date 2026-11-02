# Reconnect after an operator-assisted restore

Development functionality after 0.1.11; not included in the published package.
This protocol creates **one new GitHub binding** after a quarantined restore. It
never reactivates the old binding or replays its PR, queue or check operations.
It is not automatic disaster recovery or a production certification.

Use [local restore configuration](restore-configure.md), retain the old effects,
[recover source](restore-acquire-source.md) and complete the
[repository review](restore-review.md) first. Missing source or an unresolved
external operation must be investigated before proceeding.

## Who supplies each guarantee?

The owner must actually stop/fence old servers and runtimes, prevent old App keys
or credentials from minting more tokens, revoke or wait out already issued tokens,
and settle previously sent or unknown GitHub mutations. Control relevant host
writes during final capture/activation and review the host's branch policy. Do not
check these boxes simply because a request is absent from the restored database.
A backup cannot contain operations that happened after it was taken.

TreeHarbor records the owner's assertions and evidence digest; it **cannot verify
these external facts**. `external_fencing_verified` remains false. Keep the actual
incident/evidence record privately; its hash does not upload it or prove its truth.
Do not put App keys, tokens or private source in the plan or evidence field.

The server verifies current repository/epoch/quarantine identity, revoked old
bindings, complete fresh captures, retained operation decisions, independent source
pins and exact Git mappings. The assigned trusted bridge reports a fresh read-only
probe; the database authenticates that bridge, not GitHub's HTTP exchange itself.
Agent credentials cannot record a plan or activate it.

## Prepare a plan

Requires an owner credential with `repository.write`. Prepare a mode-0600 JSON
file in a mode-0700 directory, with your actual identifiers. Copy the App/client
and upstream fields from the selected old binding. Use a never-used binding UUID;
the new bridge principal must already exist, be enabled and have `host.read` for
this repository. These commands issue no new credentials or App grants.

```json
{
  "version": 1,
  "manifest_sha256": "<64 lowercase hex digits from the backup>",
  "source_binding": "<old binding UUID>",
  "new_binding": "<fresh UUID>",
  "binding": {
    "version": 1,
    "app_id": 7,
    "client_id": "<existing App client ID>",
    "bridge": "<prospective bridge principal>",
    "upstream": {
      "installation_id": 9, "repository_id": 11, "owner_id": 13,
      "owner": "<repository owner>", "name": "<repository name>"
    }
  },
  "ttl_seconds": 300,
  "evidence_sha256": "<SHA-256 of the private operator evidence record>",
  "old_writers_and_runtimes_fenced": true,
  "old_credentials_cannot_mint": true,
  "old_tokens_revoked_or_expired": true,
  "prior_host_mutations_settled": true,
  "host_writes_controlled": true,
  "host_policy_reviewed": true
}
```

The IDs above are examples, not a working installation. Every assertion is required
and must be true. Lifetime is 30–3600 seconds. Preserve the exact request and key
for retry; a new key is a new operation, not a retry.

```sh
treeharbor github restore-reconnect plan \
  --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" --epoch 1 \
  --key restore-plan-001 --specification /absolute/private/reconnect-plan.json
```

The result binds all retained contexts and all quarantine journals to the current
database/schema/epoch. `observe_after_unix` is the earliest permitted start of
final observations. A repeated plan request returns its original receipt and does
not extend its lifetime or undo cancellation.

## Probe and capture the final state

Use the prospective bridge's scoped credential and private configuration with the
replacement App key. The App/client/upstream identity must match the plan. The
probe waits at most a few seconds for `observe_after_unix`, reads identity using a
new read-only repository-scoped token, and revokes that token. It writes no PR or
branch. Retain a separate private journal directory across interruptions.

```sh
treeharbor github restore-reconnect probe \
  --socket "$TH_SOCKET" --credential-file "$TH_BRIDGE_CREDENTIAL" --epoch 1 \
  --plan-key restore-plan-001 --config /absolute/private/replacement-app.json \
  --state /absolute/private/reconnect-probe
```

Save the returned `key`. A recorded probe retries before reading App files or
contacting GitHub. A receipt recovered after expiry is still historical evidence,
not fresh permission; use a new plan/journal and fresh observations when required.

After `observe_after_unix`, run [inventory capture](restore-inventory-coverage.md)
for **every retained binding**, including a registered recovery target, with each old binding's assigned freshly provisioned
recovery bridge credential. Capture does not un-revoke those bindings. Complete
source preservation and current `do_not_replay` decisions, then run
`github restore-review` with every selected sealed inventory key. If preserving
newly discovered source takes too long, recapture before the plan expires. A
changed retained context requires a new plan. `do_not_replay` alone does not settle
an unknown remote outcome.

## Activate once

Create a second private JSON file:

```json
{
  "version": 1,
  "plan_key": "restore-plan-001",
  "probe_key": "<key returned by the probe>",
  "review": {
    "version": 1,
    "manifest_sha256": "<same backup SHA-256>",
    "inventory_keys": ["<one selected sealed inventory per old binding>"]
  },
  "assessment_sha256": "<exact hash returned by the latest repository review>",
  "external_assertions_still_valid": true
}
```

```sh
treeharbor github restore-reconnect activate \
  --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" --epoch 1 \
  --key restore-activate-001 --specification /absolute/private/activation.json

treeharbor github restore-reconnect status \
  --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" --epoch 1 \
  --plan-key restore-plan-001
```

Activation recomputes the entire review under the same exclusive repository
transaction that inserts the new binding. It rejects changed contexts, missing
pins, cancelled/expired plans, archived/disabled repositories, wrong probe identity,
a changed assessment, or captures/probes that started before the plan's cutoff.
Both probe and each capture must finish within 120 seconds of admission, with at
most five seconds future clock tolerance. Probe and source-binding inventory must
agree on repository node and default branch. Omitted bindings remain blockers.
Concurrent activation has one winner; a lost response is retried with the same key
and unchanged file. No extra binding or configured event is created on replay.

The result is `configured_after_operator_assisted_restore`, not approval to merge.
Normal connection, import, PR, queue, policy and exact-check guards still apply to
new work. Old validation/check/policy/event-channel rows do not transfer to the new
binding. No former operation or outbox item is bulk-replayed. Recreate only the
new work you explicitly intend, using normal workflows and fresh request IDs.

A successful historical activation receipt can still be returned after its new
binding has subsequently been revoked. `status.current_binding_state` and
`status.reconciliation_required` describe current local state; the old receipt
must not be interpreted as current readiness. Another quarantine or a new staged
database/schema invalidates the prior admission. Keep all original journals.

To abandon an unactivated plan:

```sh
treeharbor github restore-reconnect cancel \
  --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" --epoch 1 \
  --key restore-cancel-001 --plan-key restore-plan-001
```

Cancellation does not alter host state. After activation use ordinary binding
revocation instead. Failed requests commit no fresh binding; inspect the cause,
retain the same key for uncertain retries, and create a new plan only for a new
reviewed intent. Expiry does not refresh on replay.

## Limits and evidence

This increment supports 1–256 retained bindings and the existing review/inventory
bounds. It requires the same App/client/installation/upstream identity as a selected
old binding; key rotation and a replacement bridge principal are supported.
When no binding survived the backup, first register a permanently disabled
[discovery target](restore-first-target.md); inventory/source review still applies.
Migration of an existing retained binding to another App remains unsupported.
Unavailable post-backup/deleted history and external mutations must be investigated
outside this protocol. Live installed full-disaster acceptance remains unfinished.

PostgreSQL tests cover nonempty inventory with actual Rust Git mapping, source-pin
loss after passing review, concurrent admission, cancellation, wrong probe identity,
changed quarantine, and replay after revocation. A real CLI/service test loses the
activation reply after commit, kills both processes, restarts and retries without
App files or duplicate binding. Its host inventory/probe/quarantine are explicit
fixtures; it does not prove the external assertions or live recovery.
