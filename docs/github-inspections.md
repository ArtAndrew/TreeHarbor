# Exact GitHub check observations

Development API and CLI. After [PR delivery](github-pulls.md), capture selected
GitHub Check Runs for the exact published commit and compare the PR head, base
and active rulesets before and after capture. This records evidence; it does not
authorize queue admission or merging. [Actual installed-App captures](evidence/connected-live-20261024.json)
passed for two exact synthetic PR heads, selecting the observed GitHub Actions
App ID and retaining consistent before/after PR/ruleset observations. Both
completed captures recovered identically after service restart without the App
configuration. Merge-group validation and complete GitHub queue integration
remain release gates.

## Request a capture

Use a Human credential with `repository.write`. Save this as `inspection.json`,
substituting the observed pull-intent UUID, a new inspection UUID and the actual
check names and GitHub App IDs trusted by your organization:

```json
{
  "action": "inspection_request",
  "key": "inspect-selected-pr",
  "epoch": 1,
  "inspection": "55555555-5555-4555-8555-555555555555",
  "specification": {
    "pull": "44444444-4444-4444-8444-444444444444",
    "required": [{"name": "CI / build", "app_id": 7}],
    "ttl_seconds": 600
  }
}
```

App ID `7` is an example, not an identity you should trust by default. Match the
actual check-run issuer; the check name alone does not establish it.

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  github --request inspection.json
```

The service derives the exact repository, PR number/node ID, generated head ref,
base ref and Git revision from the retained publication. Select 1–16 distinct
name/App pairs; names have at most 256 bytes. The request lasts at most 900 seconds.
An operational limit allows four unexpired pending inspections per repository.
There is no license or usage-count restriction.

An inspection has its own deadline, so expiration of the earlier branch-delivery
request does not prevent checking an already published PR. The source version must
still be current and its projection ready, with an active binding and authority
epoch. A cancelled projection or changed ChangeSet version fences new collection.

## Collect with the assigned bridge

Use the private [App configuration](github-connection.md) and the binding's Bridge
credential with `host.read`. A credential containing only `host.publish` cannot
collect this evidence. The App installation needs **Checks**, **Pull requests**,
**Contents** and **Metadata**, all **read-only**. Tokens are narrowed to the pinned
numeric repository. The command does not request Administration or write scopes.

```sh
install -d -m 700 /private/treeharbor/inspections/pr-1
treeharbor github inspect \
  --config /private/treeharbor/app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/reader.json \
  --inspection 55555555-5555-4555-8555-555555555555 --epoch 1 \
  --state /private/treeharbor/inspections/pr-1
```

The command works without a Git executable on supported Linux/macOS hosts. It
uses GitHub.com directly, with bounded requests and no redirects or environment
proxy. It reads the PR, active branch rulesets, exact SHA/name/App Check Runs,
then the rulesets and PR again. Returned URLs are never followed. Current local
authorization is checked before token creation and after the capture.

The capture selects GitHub's `filter=latest` view. It retains at most two matching
runs per selected name/App pair and the provider's total count. Only exactly one
`completed` run with `success` satisfies a selected check. Missing, ambiguous,
pending, failed, neutral, skipped or unknown results do not. Every retained run
must have the exact requested SHA, name and App ID. The server recomputes these
decisions from a shared Rust contract; a reporter cannot submit a `passed` flag.

## Read the result correctly

| Field | Meaning |
|---|---|
| `state: observed` | An immutable capture was stored; it may contain failing checks |
| `capture_consistent` | The observed PR state/head/base and ruleset digest matched before/after, and the head matched the target |
| `selected_checks_passed_at_capture` | The capture was consistent and every explicitly selected Check Run passed |
| `is_current_version` | The native proposal version still matches the inspection's source |
| `host_policy_verified: false` | This is not a complete evaluation of GitHub admission policies |
| `legacy_branch_protection_included: false` | The ruleset snapshot does not contain legacy branch protection |
| `merge_authorized: false` | This result cannot authorize a merge |

Checks can rerun and provider state can change after the last read. No multi-endpoint
GitHub snapshot is atomic. Even identical before/after reads cannot prove that no
intermediate change occurred. Idempotent mutation replies omit dynamic
current-version/deadline flags; read `inspection_status` for those current local
facts. A completed inspection remains a historical capture; request a new inspection
for fresh evidence. A new ChangeSet version marks the old
source stale and does not inherit its validation. Closed/draft PR state is retained,
but a passing selected check does not imply that the PR can be merged.

Active rulesets are captured as a bounded canonical digest, count and presence of
a merge-queue rule. The ruleset read is repeated to detect drift. This does not
prove legacy protection, reviews, deployments or every host requirement is met.
Legacy commit statuses are not selected Check Runs. The eventual GitHub queue must
enforce its own policies without bypass and validate each exact merge-group SHA;
these head checks cannot stand in for those group checks.

## Recovery, status and cancellation

Repeat the same command and state directory after interruption. Reads may repeat
until a report is saved; once saved, the private journal retries that exact report
and receipt. An OS lock prevents competing drivers in the same directory. The
server accepts one immutable report per inspection. A terminal result or pending
report can recover without the App configuration. No token or key is stored in
the journal or database; `token_revoked` describes this capture's token cleanup,
not every earlier interrupted attempt. A lost token-creation response can leave
a temporary read token alive until GitHub expires it.

Exit zero means the capture was stored, including captures with failing checks.
Inspect the result fields rather than interpreting process success as passing CI.
An expired/cancelled request cannot start a fresh capture. A report captured within
its request window can still be retained after expiry/source replacement/binding
revocation, using the original bridge's current credential and authority epoch.
It does not gain new validity. Reports captured after the deadline are rejected;
create a new owner request and state directory for a new capture.

Use the same `client ... github --request FILE` interface for these commands:

```json
{"action":"inspection_status","epoch":1,"inspection":"55555555-5555-4555-8555-555555555555"}
```

```json
{"action":"inspection_cancel","key":"cancel-inspection","epoch":1,"inspection":"55555555-5555-4555-8555-555555555555"}
```

Status requires `read`; cancellation requires `repository.write`. Cancellation
stops acceptance of a pending capture. It cannot erase an already observed record.
Bridge actions `inspection_context`, `inspection_collect_context` and
`inspection_report` require `host.read`; the last includes a mutation `key` and
the typed capture report. Fresh collection context is distinct from historical
status. The service trusts the assigned bridge, not an agent's claim of CI success.

Schema 20 preserves PR/publication/source data and existing permissions. Tests
exercise source/issuer mismatch, head/base/policy drift, latest reruns, scope and
deadline fences, populated upgrades and real CLI/server process loss. Provider
snapshots in these tests are explicitly fixtures, not installed-App evidence.
