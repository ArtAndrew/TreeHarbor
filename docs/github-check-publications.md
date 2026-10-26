# Publish a managed GitHub check

Experimental Connected service/CLI support. This connects a collected
[managed execution](github-managed-checks.md) to an exact required App check.
Full live execution, queue admission and final-merge acceptance remain release
gates. An observed check delivery is not a complete merge authorization.

New Report version 2 receipts preserve numeric and opaque node check identities
for [candidate check-set evaluation](github-admission-evidence.md). Version 1
receipts and journal replay remain supported. Explicit publication-owner or
reporting-bridge trust revocation invalidates current aggregate eligibility while
the immutable receipt remains historical; dispatch expiry alone does not erase it.

## Prerequisites and authority

Upgrade server and CLI together and apply the current additive migration29
(including the migration28 publisher). Existing data,
repository grants and tokens are preserved; none silently gains `host.check`.
Use a private operator directory for credentials, requests and publisher state.

The policy owner first approves an offline program for an exact imported PR head
or retained merge-group candidate. Its assigned validator completes and collects
that execution. A separate Human `host.queue` credential requests publication of
the result. The assigned Bridge requires an explicitly granted `host.check`
credential. Agents, validators and ordinary readers cannot publish checks.

The server derives source, program, evidence and conclusion from protected
execution records. The request cannot supply a success, head, program or App ID.
Before authorizing dispatch, the server recomputes that evidence and checks the
original policy owner, validator, runtime profile/node, source, binding, authority
epoch and publication owner's original token. Revocation or a changed source
blocks new dispatch. A failed exit produces `failure`; unknown/error outcomes
produce `action_required`, never success, neutral or skipped.

The App needs Checks:write and Administration:read in addition to the scoped read
permissions described in [the transport contract](github-required-check-transport.md).
The publisher narrows each temporary installation token to one numeric repository;
it does not receive branch, PR or queue write permission. It attempts revocation
after each operation and records failure to revoke without inventing a rollback.

**Supported host policy:** legacy branch protection must require the exact context
`TreeHarbor/e<epoch>/<full program SHA-256 hex>` from this App, enforce admins,
disable force pushes/deletion and have no legacy review bypass allowances. An
effective merge-queue rule must also be present. Ruleset-only protection is not
supported by this initial verifier. Program/image/epoch changes require explicit
host-rule rotation; TreeHarbor never silently changes branch protection.
After [branch admission-policy adoption](github-admission-policy.md), target
version2 instead uses the returned `TreeHarbor/e<epoch>/a<context SHA256>` name,
which also binds binding/base/policy generation. Both host preflight reads must
match the policy's entire required set and reviewed rules fingerprint. Policy
rotation requires explicit host-context rotation even for an unchanged program.

## Request and run

Save this request as `request.json`, replacing both IDs with actual values:

```json
{
  "action": "check_publication_request",
  "key": "publish-pr-check-1",
  "epoch": 1,
  "publication": "11111111-1111-4111-8111-111111111111",
  "specification": {
    "check_run": "22222222-2222-4222-8222-222222222222",
    "ttl_seconds": 600
  }
}
```

The deadline is 1–900 seconds and bounds new dispatch, not retention of historical
facts. `check_run` identifies the collected managed execution, not a GitHub run ID.
Use the same shape for a PR or a group; the server derives which source was tested.

```sh
treeharbor client --socket "$SOCKET" --credential-file "$OWNER" \
  github --request request.json

mkdir -m 700 "$PRIVATE_STATE/check-1"
treeharbor github check-publish --config "$APP_CONFIG" \
  --socket "$SOCKET" --credential-file "$BRIDGE" \
  --publication 11111111-1111-4111-8111-111111111111 --epoch 1 \
  --state "$PRIVATE_STATE/check-1"
```

`APP_CONFIG` uses the existing [private App configuration](github-connection.md).
`OWNER` needs `host.queue`; `BRIDGE` needs `host.check`. Requesting status separately
needs `read`. As with other administration commands, `access-set` replaces a grant:
retain any other intended permissions explicitly when adding `host.check`, then
issue a new narrowly scoped token. Never give the App key or bridge token to a task.

The CLI reserves an attempt, reads GitHub source/policy twice, then asks the server
for one final dispatch grant. Only its fresh response permits a single create
request. Replays return a historical receipt with `dispatch_granted:false`.
After creation, the bridge rereads the exact returned check ID. A group's combined
candidate SHA is distinct from its member PR head, including when trees match.

Exit zero means the intended delivery was observed. Read the returned
`target.conclusion` to distinguish a successful check from delivery of a failure.
The `admission_verified`, `host_policy_verified` and `merge_verified` flags remain
false: a required-context preflight does not certify the complete host policy.

## Retained dispatch policy record

For a successful **queue-candidate** result with an owner-reviewed complete
AdmissionPolicy v2, dispatch now requires the latest attributable host-policy
capture after this check's committed collection receipt. The capture must match
its candidate, current membership, owner configuration and immediate publication
preflight. Existing report freshness and credential-revocation bounds apply.
The server checks those facts again before committing the dispatch transaction.
After collecting the managed check, run the [host-policy collector](github-host-policy.md#collect-and-read)
for its retained membership before `check-publish`. An absent or mismatched full
capture refuses dispatch; it never falls back to the legacy required-context test.

An immutable record binds the selected execution, exact publication target,
collection operation, owner pin and capture/configuration/preflight digests. It
commits with the one-shot grant, operation receipt and outbox event. No future
GitHub check receipt or success from another program is required to publish this
individual result. GitHub still enforces its complete required set and authorizes
merging. Administrator changes on GitHub cannot be locked by this transaction.

Read the retained record using a `read` credential:

```json
{"action":"check_dispatch_policy","epoch":1,"publication":"11111111-1111-4111-8111-111111111111"}
```

Submit this JSON through `treeharbor client ... github --request request.json`.
The response has `recorded`, `proof` and `digest`; `merge_verified` remains false.
`recorded:true` describes the former dispatch decision, not present permission to
send or merge. Recovery retains the original record after cancellation or expiry.
PR-head checks, non-success conclusions, legacy policy profiles and dispatches
predating schema 31 have no such record. `recorded:false` is unknown historical
policy certification, not a fabricated failure or success. Migration does not
backfill proofs. Existing publication target/report encodings remain unchanged.

## State and recovery

| State | Meaning | Next action |
| --- | --- | --- |
| `requested` | Owner intent retained, no attempt | Run publisher or cancel |
| `attempting` | One attempt reserved; `preflight` records final dispatch if granted | Recover this attempt |
| `uncertain` | A write might have reached GitHub | Read-only reconciliation; never automatically create again |
| `observed` | One exact check identity/conclusion observed | Historical result; no new authorization |
| `not_sent` | Trusted no-send evidence retained | A new owner request may start a separate intent |
| `cancelled` | Cancelled before an attempt | A new owner request is required |

Only one unresolved publication may exist per repository/binding/epoch/exact Git
head/program/host context. Legacy version1 keeps its original context bucket;
version2 separates policy generations and base branches. An unresolved old context
cannot authorize another send, but does not block a distinct new context.
Expiry or cancellation after an attempt does not release that context's fence.
An `uncertain` attempt cannot later become `not_sent`. Observed and not-sent
terminal records and all reports are immutable.

Run the same command with `--recover-only` to avoid reserving a new attempt. An
existing attempt with no committed preflight can be atomically marked not-sent;
that transition fences a late dispatch callback. If dispatch won the race, recovery
instead reads GitHub. An admitted attempt uses complete bounded check scans with
exact App, context, SHA, external ID and conclusion matching. Absence, duplicates,
truncation or drift do not prove the write never happened. See the transport limits.

The private journal locks out another local publisher. It retains immutable intent,
attempt, preflight, reports and acknowledgments. Pending reports are submitted
before loading the App key; losing the server's commit response cannot duplicate
the report or reissue a dispatch. Preserve this directory across process restarts.
If local evidence is lost after dispatch, remote absence can remain unresolved and
require investigation; the CLI does not trade that safety for automatic retries.

GitHub and PostgreSQL cannot share an atomic transaction. A write already admitted
and in flight cannot be recalled by subsequent local cancellation or revocation.
Host policy/source reads are observations, not a lock on GitHub. Historical report
recovery remains possible after deadline/cancellation/binding revocation within the
same active authority epoch, using current assigned-bridge credentials. It does not
refresh old approvals or make an old check current.

## Version 1 wire boundary

All commands use the standard authenticated `github` request envelope and `epoch`.
Every mutation has an idempotency `key`. Reusing a key with another payload fails.

| Action | Additional fields | Required permission |
| --- | --- | --- |
| `check_publication_request` | `publication`, `specification: {check_run, ttl_seconds}` | Human `host.queue` |
| `check_publication_begin` | `publication`, `attempt` | Assigned Bridge `host.check` |
| `check_publication_dispatch` | `publication`, `attempt`, `preflight` | Same attempt token, `host.check` |
| `check_publication_report` | `publication`, `attempt`, `report` | Current assigned Bridge `host.check` |
| `check_publication_context` | `publication` | Assigned Bridge `host.check` |
| `check_publication_status` | `publication` | `read` |
| `check_dispatch_policy` | `publication` | `read` |
| `check_publication_cancel` | `publication` | Human `host.queue` |

Ordinary callers should use the publisher command for begin/dispatch/report.
`preflight` and `report` follow the [v1 vector](../crates/github-contract/tests/fixtures/check-publication-v1.json).
The [owner command vector](../crates/metadata/tests/fixtures/check-publication-request-v1.json)
contains no caller outcome. Status/cancel requests, for example:

```json
{"action":"check_publication_status","epoch":1,"publication":"11111111-1111-4111-8111-111111111111"}
```

```json
{"action":"check_publication_cancel","key":"cancel-check-1","epoch":1,"publication":"11111111-1111-4111-8111-111111111111"}
```

Schema28 stores immutable target/owner/bridge identity, one-way attempt and
preflight, monotonic bounded report sequence (maximum 1,024), and an immutable
report ledger. Receipt/outbox writes and permission/deadline rechecks commit
together. PostgreSQL migration/failure tests and killed client/server tests cover
these boundaries. Their provider and terminal-runtime fixtures are labelled; they
are not evidence of a live installed-App write or full release readiness.
