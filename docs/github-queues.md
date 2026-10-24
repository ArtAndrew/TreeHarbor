# GitHub queue delivery (development)

This development command requests the native GitHub merge queue, retains the
provider's asynchronous request identity and recovers observations after process
loss. It does **not** yet verify final integration. An explicit
[removal command](github-dequeue.md) adds remote dequeue; durable
merge-group events, exact group/final-tree proof and actual installed-App acceptance
remain unfinished. Do not use this increment as a production merge controller.

## Authorize a request

First complete [PR delivery](github-pulls.md) and a successful, consistent
[selected-check inspection](github-inspections.md). Issue the Human a separate
`host.queue` permission and the assigned Bridge a separate `host.merge` permission.
Existing `repository.write`, `host.read` and `host.publish` grants do not acquire
these powers on upgrade. Agent credentials cannot receive either permission.

Save this request as `queue.json`, replacing both example UUIDs:

```json
{
  "action": "queue_request",
  "key": "queue-selected-change",
  "epoch": 1,
  "queue": "66666666-6666-4666-8666-666666666666",
  "specification": {
    "inspection": "55555555-5555-4555-8555-555555555555",
    "ttl_seconds": 600
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/queue-owner.json \
  github --request queue.json
```

The service pins the native proposal version, projection, exact Git head, PR
identity, expected base, selected checks, ruleset observation, binding and authority
epoch. Its dispatch deadline is independent of the earlier publication/inspection
deadline, and is at most 900 seconds. A new source version or cancelled projection,
revoked binding, credential or changed authority fences new effects. Queue records
retain the source graph and an append-only observation history.

## Run the assigned bridge

```sh
install -d -m 700 /private/treeharbor/queues/change-1
treeharbor github queue \
  --config /private/treeharbor/app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/queue-bridge.json \
  --queue 66666666-6666-4666-8666-666666666666 --epoch 1 \
  --state /private/treeharbor/queues/change-1
```

Each invocation performs one delivery or recovery observation. Repeat with the
same state directory to advance observation. Automatic scheduling is not yet
implemented. The journal must be private; an OS lock prevents two drivers from
using the same directory concurrently. The command needs no Git executable.

Before delivery, the bridge recollects the selected head checks and compares the
PR/base/rules and queue configuration. It checks current local permission again
immediately before the host write. Exactly one durable dispatch grant permits one
async PUT with the exact head SHA, `merge_action: merge_queue` and
`bypass_rules: false`. There is no direct-merge/default fallback. Two workers or
replayed/lost grant replies cannot obtain another dispatch for the same intent.
These cross-service reads are not atomic with GitHub changes; GitHub enforces its
own current admission rules. Selected head checks are not merge-group checks.

App tokens are narrowed to the pinned numeric repository:

| Operation | Requested repository permissions |
|---|---|
| Head inspection | Contents, Metadata, Pull requests, Checks: read |
| Queue/PR observation | Above plus Merge queues: read |
| Queue dispatch | Contents: write; Metadata, Pull requests, Checks, Merge queues: read |
| Async result GET | Contents: write; Metadata: read |

Although it performs a GET, GitHub's async-result endpoint requires Contents-write.
That token is separate from the read-scoped observation token and is not used to
send a merge. Every minted token is revoked on the normal completion path.
`token_revoked` describes only the token for that capture, not earlier interrupted
attempts or the preceding inspection. Keys and token values are not journaled.

## Interpret recovery and status

The provider response is journaled before database reporting. A retained UUID is
bound to its original PR, exact head and queue action. Result reads use that UUID;
unknown or mismatched pending responses remain uncertain. Once a terminal
`enqueued` result is saved, the command observes the PR/queue instead of polling
that result for final merge. Pending/unavailable result reads alternate with PR
observations. A 404 can mean expiry or unavailable data; it does not prove that
the request failed and does not permit a new PUT.

Late journal reports remain in history even when a newer observation already
exists. This preserves the only returned UUID without replacing the newer live
view. Terminal async outcomes and observed final commit/tree facts are retained;
a later network error or expired provider record cannot erase them. Contradictory
terminal outcomes for the same UUID are rejected.

| State | Meaning |
|---|---|
| `requested` | Owner intent exists; no dispatch has been granted |
| `uncertain` | No conclusive observation of the effect |
| `host_pending` | The exact async request is pending |
| `host_enqueued` | A capture recorded enqueue; not a verified final merge |
| `validating` | A queue entry has base/head commit observations; checks are not yet certified |
| `result_unavailable` | The async result could not be found; not proof of failure |
| `request_failed` | An immediate rejection or exact-UUID failure was retained |
| `not_sent` | The sole invocation positively established that it sent no mutation |
| `stale` / `closed` | A capture found a changed head or an unmerged closed PR |
| `host_merged_unverified` | A host merge fact exists; required group/final-tree proof is incomplete |
| `cancelled` | Cancellation happened before dispatch |

`provider_result` is retained request history; `observation` is the latest capture
by completion time. Neither establishes continuous membership of a mutable queue.
`recorded_observation` in a report receipt acknowledges exactly the submitted
record, including a late one. `merge_verified` and `host_policy_verified` remain
false in this increment. Exit zero means an observation was stored, not that CI
passed or the change merged. A pending report can recover without App configuration
or credentials for GitHub, but still needs the original bridge's current TreeHarbor
credential and authority epoch.

Use `client ... github --request FILE` for status and cancellation:

```json
{"action":"queue_status","epoch":1,"queue":"66666666-6666-4666-8666-666666666666"}
```

```json
{"action":"queue_cancel","key":"cancel-queue","epoch":1,"queue":"66666666-6666-4666-8666-666666666666"}
```

Status requires `read`; cancellation requires `host.queue`. After dispatch,
cancellation records `cancel_requested` and stops future enqueue effects, but
**does not remove the PR from GitHub's queue**. Use a separate explicit
[dequeue request](github-dequeue.md) for host removal. A concurrent host merge remains
recorded. An unknown, expired or merely locally cancelled dispatch blocks a new
intent for that PR. A confirmed request failure or positive no-send report permits a new explicit owner
request with a new UUID/key and fresh checks; replaying the original intent never
resends it. Source pins are preserved across these transitions.

The shared Rust contract rejects extra/unknown fields and malformed
identities. [Async-result vectors](../crates/github-contract/tests/fixtures/queue-result-v1.json)
cover pending, enqueue, failure, merge and unavailable outcomes. Version2 queue
reports add positive no-send evidence while preserving version1 reads. Schema22
adds explicit removal records without converting old cancellations into host writes.
Provider transport/database/CLI tests use labelled fixtures and local process-loss
injection; they do not prove installed-App or real merge-group behavior.
