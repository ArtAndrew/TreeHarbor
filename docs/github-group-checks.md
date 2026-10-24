# Retained GitHub candidate checks (development)

An entry can leave the queue before CI finishes or the bridge recovers. Given an
already saved [membership capture](github-queue-groups.md), TreeHarbor can collect
checks for that exact candidate commit and tree later. It preserves separate
suite, check and workflow identities, including a later `push` workflow on the
same SHA/name/App. It does not choose the newest matching name as merge evidence.

## Collect

Read `queue_groups` and select a capture's `sequence` and `before` or `after` side
containing the original PR head and candidate. Use the same private journal as
the existing queue command:

```sh
treeharbor github queue --config /private/treeharbor/github-app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/bridge.json \
  --queue "$QUEUE_ID" --epoch 1 --state /private/treeharbor/queue-state \
  --group-report 4 --group-side after
```

Here `4` is an example retained report sequence, not a PR or workflow number.
`after` is the default side. The service checks the original queue, membership,
source head, candidate and authority epoch. No recorded candidate means no
collection; this command cannot reconstruct membership that was never captured.
The queue must already have an attempt. This option never begins an enqueue.

The assigned bridge keeps its existing local `host.merge` permission. The App
token is narrowed to the pinned repository with Contents, Metadata, Pull requests
and Checks read permissions. It requests neither Actions nor Merge queues for
this operation. Two bounded reads are journaled together. Pending reports replay
first, even without the App config; rerun the command to request a fresh capture
after recovery. Upgrade CLI and server together.

## Read

Save this command as `checks.json`, replacing the UUID and epoch:

```json
{
  "action": "queue_checks",
  "epoch": 1,
  "queue": "66666666-6666-4666-8666-666666666666",
  "after": 0
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/reader.json github --request checks.json
```

This local read needs current repository `read` permission and no App key. Each
page contains at most one capture; use `next_after` as the next cursor until null.
Each capture includes the original version5 `report`, `checks_consistent`,
`membership_consistent`, correlated `signed_events` and `selected_checks`.
The latter lists all matching App/name checks before and after, with suite and
workflow IDs and `merge_group_event`. Empty matches are missing evidence.
The report links to its immutable membership sequence and side. Check-only
reports do not replace the latest queue snapshot. No database migration is needed.

## Guarantees and limits

Collection rejects foreign repositories/commits, truncated connections, duplicate
identities, contradictory suite/check relationships and invalid times. Limits are
20 suites and 100 total check runs per snapshot, plus 48 KiB for the encoded pair
and target. The byte limit can bind first. There is no truncated-success fallback.
Earlier records remain available after a failed collection. Workflow file URLs
are recorded as metadata only; they are never followed.

`observed_run_attempt` is the workflow's current attempt at observation. It does
**not** establish which attempt produced every historical check returned by
`checkType: ALL`. Missing workflow metadata remains unknown. Agreement between
two reads is not an atomic GitHub snapshot. A familiar name/App or valid syntax
does not establish a trusted workflow definition.

`attempt_verified`, `definition_verified`, `merge_verified` and
`host_policy_verified` remain **false**. These observations do not create native
validations or grant merge authorization. Exact job/attempt linkage, trusted
workflow/policy and final published-history verification are still release gates.

Contract/provider-fixture tests cover duplicates, reruns, incomplete pages and
wrong identities. PostgreSQL and actual CLI/server process-death tests cover
retained membership, stale context rejection and replay after the commit reply is
lost. The [real Rust transport diagnostic](evidence/group-checks-read-20261024.json)
read distinct `merge_group` and `push` suites from the public test repository
and revoked its token. Its subject came from previous operator evidence, so it
does not prove the full product membership path or private-repository entitlement.
