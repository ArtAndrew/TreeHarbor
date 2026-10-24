# GitHub queue removal (development)

An explicit Human request can authorize the assigned Bridge to remove a PR from
GitHub's merge queue. `queue_cancel` remains a local cancellation flag; upgrading
never turns an old flag into permission for a new host write. This command is
experimental. Actual installed-App acceptance, final merge/group verification and
automatic scheduling are still release gates.

## Owner authorization

The owner needs `host.queue`; the assigned Bridge needs `host.merge`. Agents,
read-only bridges and publishers cannot remove queue entries. Save the following
as `dequeue.json`, replacing both UUIDs with the intended identities:

```json
{
  "action": "dequeue_request",
  "key": "remove-selected-pr",
  "epoch": 1,
  "dequeue": "77777777-7777-4777-8777-777777777777",
  "specification": {
    "queue": "66666666-6666-4666-8666-666666666666",
    "ttl_seconds": 600,
    "scope": "pull_request"
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/queue-owner.json \
  github --request dequeue.json
```

Creating this request atomically sets the original queue's local cancellation
flag. Its separate dispatch deadline is at most 900 seconds. A source revision
change or expired original enqueue deadline does not prevent removal; current
binding, authority epoch and credentials still fence new writes.

**Authorization targets the PR identity.** GitHub's `dequeuePullRequest` input has
no expected-head or expected-entry comparison. Another actor can change the head
or queue entry between preflight and mutation. TreeHarbor retains both the observed
and returned identities; it does not promise an atomic removal of one exact head.
`clientMutationId` correlates the reply and is not treated as provider idempotency.

## Dispatch and recovery

```sh
install -d -m 700 /private/treeharbor/dequeues/change-1
treeharbor github dequeue \
  --config /private/treeharbor/app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/queue-bridge.json \
  --dequeue 77777777-7777-4777-8777-777777777777 --epoch 1 \
  --state /private/treeharbor/dequeues/change-1 \
  --queue-state /private/treeharbor/queues/change-1
```

Use a distinct private directory for removal. `--queue-state`, when needed, points
to the **original enqueue journal**, including any response that has not reached
the server. It performs recovery only and never grants or sends another enqueue.
If omitted while enqueue is unresolved, the command reports that state without
dispatching a removal. Repeat the same command to advance recovery; no background
poller is installed. Exit zero means an observation was recorded, not necessarily
that removal succeeded.

Before dispatch, the service requires evidence that the original enqueue settled:
no enqueue grant existed, the sole invocation proved it sent no mutation, or a
definitive immediate/async terminal result was retained. A 404, elapsed time or an
absent queue entry is insufficient. Otherwise a background enqueue could appear
after an apparent removal. Losing the only async UUID before journal persistence
can leave this state unresolved; the command never repeats a mutation to discover
its identity. An operator must investigate that upstream operation.

A durable grant allows one GraphQL mutation. The bridge checks current permission
before acquiring credentials and immediately before mutation. Lost replies and
repeated commands observe only. Returned receipts are journaled before further
host reads and before database reporting. A pending report can recover without
the App key or Git installed, while still requiring the current TreeHarbor bridge
credential. Known final merge facts are retained even when later reads fail.

The removal token currently requests Contents-write, Metadata-read, Pull
requests-read and Merge queues-write for one numeric repository. The exact minimal
permission set and populated mutation response still need live App verification.
Subsequent observations use the existing read-scoped queue transport. Secrets are
not stored in journals; token revocation is attempted after use.

## Status and boundaries

Send this through `client ... github --request FILE` using a `read` credential:

```json
{"action":"dequeue_status","epoch":1,"dequeue":"77777777-7777-4777-8777-777777777777"}
```

| State | Meaning |
|---|---|
| `enqueue_unresolved` | The earlier enqueue may still complete; removal is deferred |
| `requested` | Explicit removal intent exists, no dispatch yet |
| `uncertain` | Dispatch was granted, outcome not established |
| `not_sent` | The sole invocation positively reported no mutation sent |
| `dequeue_acknowledged` | A correlated removal response was retained; absence still needs observation |
| `removal_observed` | A later capture found the unmerged PR absent after enqueue settled |
| `host_queued` | A capture found a queue entry, including an external re-enqueue |
| `host_merged_unverified` | The proposed head was reported merged; final proof is still incomplete |
| `other_head_merged` | A different head of this PR was reported merged |

`retained_receipt`, `mutation_not_sent` and `retained_merge` survive later missing
or older reports. `observation` is the latest capture by completion time;
`recorded_observation` acknowledges the exact submitted report, including late
ones. `merge_verified` remains false. No state prevents future external re-enqueue.

A new enqueue/removal intent for the bound PR is blocked while a previous removal
could still run. A correlated completed removal response or positive no-send
evidence permits a **new explicit owner request**, never replay of the old write.
A new enqueue following a previously successful enqueue additionally requires a
later unmerged absence capture; acknowledgement alone is insufficient.
A newer active enqueue prevents removing through an old queue intent; select the
current intent. Absent entries or transport timeouts alone cannot prove an earlier
mutation stopped. A revoked binding requires owner/operator action, not broader
bridge credentials or automatic retries.

## Wire and migration

Schema22 preserves schema21 grants, reports and local cancellations. Removal
version1 separates intent, one-shot begin, append-only reports and scoped reads.
QueueObservation version2 adds positive `mutation_not_sent` evidence; version1
remains readable and cannot assert that fact. No-send cannot coexist with a
provider mutation receipt/result for the same attempt. See the
[removal vectors](../crates/github-contract/tests/fixtures/dequeue-v1.json) and
[queue no-send vector](../crates/github-contract/tests/fixtures/queue-no-send-v2.json).

Database, provider and actual CLI process-loss tests use labelled host fixtures.
They do not prove installed-App permissions, live queue behavior or final merge
verification. Workspace and runtime isolation remain separate concerns.

Read-only recovery uses observation version2 with a separate snapshot `queue_id`,
without requesting write-protected queue settings. Version1 reports remain
readable unchanged; upgrade server and CLI together. The
[version2 vector](../crates/github-contract/tests/fixtures/dequeue-read-v2.json)
records queued membership without inventing settings or claiming removal.
