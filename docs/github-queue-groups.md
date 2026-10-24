# Retained GitHub queue groups (development)

Queue membership can disappear before a bridge recovers. The read phase of
[`github queue`](github-queues.md) now retains two successive observations of
the target PR, the actual target branch, complete bounded queue membership and
candidate commit parents. Later empty membership or a failed request does not
erase the saved observations. Capture while the PR is queued: this command cannot
reconstruct membership that was never observed.

## Collect and read

Continue the existing `github queue` command with its original private state
directory. The driver retains its existing local `host.merge` requirement for
the assigned Bridge; it narrows the App token to read permissions for observation,
including Merge queues. This observation adds no permission, configuration read,
enqueue or background scheduler. Enqueue still needs its separate dispatch grant.

Save this command as `groups.json`, using your queue UUID and authority epoch:

```json
{
  "action": "queue_groups",
  "epoch": 1,
  "queue": "66666666-6666-4666-8666-666666666666",
  "after": 0
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/reader.json \
  github --request groups.json
```

Reading needs the repository's current `read` permission. It contacts only the
local service, requires no App key and does not modify GitHub. The version1 result
contains `queue`, `binding`, `epoch`, `captures` and `next_after`. Each page contains
at most one capture. Set `after` to the returned `next_after` to continue; null means
there is no additional retained capture at the time of this read. No captures
means no saved membership evidence, not an empty GitHub queue.

Each capture contains:

| Field | Meaning |
|---|---|
| `sequence`, `report` | Immutable report sequence and original version4 observation |
| `consistent` | Both reads agree, including source head, actual main and members |
| `before_exact_merge_parents`, `after_exact_merge_parents` | Selected candidate has exactly two ordered parents: observed queue base and exact PR source head |
| `before_signed_events`, `after_signed_events` | Matching retained signed facts, with `events` and `more_events` |

`report.group_capture.before` and `.after` contain the actual main commit/tree,
target PR snapshot and ordered members. Members retain PR/node/number, source
repository/head, queue entry/position, base commit/tree and candidate commit/tree
with complete ordered parents. A PR's `baseRefOid` is recorded separately; it is
not substituted for the branch tip.

Matching signed facts require the original tenant/repository, binding, authority
epoch, App, installation, provider repository/owner and exact base/head/tree.
Events can arrive before or after the capture. Revoking an event channel preserves
its historical facts. Reconnecting the same upstream does not attach a new
binding's events to old queue records. The event list contains at most eight
references per side; `more_events: true` means it is incomplete. Use
[`event_page`](github-events.md) with each referenced channel for the event ledger.

## Limits and recovery

Observation version4 adds optional `group_capture` to existing queue reports. The
[golden vector](../crates/github-contract/tests/fixtures/queue-groups-v4.json)
pins its wire format; older version1/2/3 vectors remain unchanged. Upgrade server
and CLI together. Storage uses the existing append-only ledger; no database
migration is needed.

The collector rejects truncated pages, null members, duplicate identities or
positions, foreign queue/base repository and contradictory content for the same
commit ID within a capture. It accepts at most 100 members and eight parents per
candidate. The serialized pair is also limited to 48 KiB; that bound can be reached
before 100 entries. Provider responses keep their existing 128 KiB limit.
Exceeding a bound records the ordinary uncertain outcome rather than a truncated
capture claimed to be complete. Earlier successful captures remain available.

Both reads are journaled together before reporting. Losing the response after
the database commit and restarting client/service replays the same report without
another provider request. As with other queue observations, interrupted App-token
revocation requires waiting for token expiry; `token_revoked` is an observation,
not a guarantee for a killed process.

## What this establishes

Agreement is a capture-time fact, not an atomic snapshot of GitHub or a guarantee
of continuous membership. A two-parent relationship does not certify checks,
all preceding queued changes or semantic compatibility. Unknown squash/rebase
layouts are retained without inventing a parent mapping.

`merge_verified` and `host_policy_verified` remain **false**. Required group
checks, trusted workflow definitions, policy coverage and final published
commit/tree mapping still need complete verification.
The separate [candidate-check collector](github-group-checks.md) retains complete
bounded check identities for a selected membership report; it does not certify
their attempt, definition or final publication.

Provider fixtures exercise nonempty membership, drift, truncation and invalid
identity. Real PostgreSQL tests cover reordered/late signed events, reconnection,
pagination and revocation. The CLI test kills the client and server after a report
commit, then recovers the exact saved capture. These are distinct from the
[actual installed-App read](evidence/queue-group-read-20261024.json): it verified
the new Rust query, narrowed permissions and actual main against an empty queue
after the earlier live acceptance. A live populated capture remains to be tested.
