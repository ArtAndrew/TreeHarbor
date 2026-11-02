# Observe queue membership during recovery

Introduced in 0.1.12-alpha.1; not included in 0.1.11 or earlier packages.
After [restore assessment](restore-github-assessment.md), this command reads the
exact PR associated with a retained queue or dequeue operation twice. It saves
the observations without enqueueing, removing, merging or reconnecting anything.

Keep the old deployment and its publishers fenced. Use the original private App
configuration and a fresh `host.read` credential for the binding's assigned bridge.
The App token is narrowed to one repository and read permissions for contents,
metadata, pull requests, checks and merge queues. It does not request the queue's
write-protected configuration. This is an existing capability profile, not a
request to grant extra permissions to an installed App automatically.

```sh
umask 077
mkdir -m 700 /absolute/private/treeharbor/queue-capture-001
treeharbor github restore-inspect-queue \
  --config /absolute/private/treeharbor/github-app.json \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-bridge.json \
  --binding "$TH_BINDING" --epoch "$TH_EPOCH" \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  --kind queue --operation "$TH_QUEUE_OPERATION" \
  --state /absolute/private/treeharbor/queue-capture-001
```

Set variables to reviewed identities from the assessment. To observe a removal,
use `--kind dequeue --operation "$TH_DEQUEUE_OPERATION"` and a separate state
directory. The server resolves that removal to its original queue and retained
PR target; the caller cannot substitute another PR. Operations missing from the
restored database are not supported by this command.

## Interpret the receipt

| Classification | Observation |
| --- | --- |
| `changed_during_capture` | The snapshots differ; review and capture again. |
| `head_differs` | Stable PR state, but its head revision differs from the retained target. |
| `queued` | Both snapshots show the PR in the same queue entry state. |
| `not_in_queue` | Both snapshots show an open PR absent from an enabled queue. |
| `closed_unmerged` | The PR is closed and not merged. |
| `queue_disabled` | The open PR has no enabled queue. |
| `host_merged_unverified` | GitHub reports a merge; its exact final commit/tree is retained but source correspondence is not verified here. |

Every classification has `prior_mutation_settled: false`,
`source_mapping_verified: false`, `reconciliation_complete: false` and
`publication_authorized: false`. **Absence now does not prove that an old in-flight
enqueue or removal cannot execute later.** Equal reads are not an atomic host
snapshot and do not rule out a change-and-revert between reads. Queue membership
does not prove checks passed or that the merged source is correct.

The service validates the report against the current restore context, original
queue target, repository, PR identity, App/installation and clock bounds. It trusts
the assigned bridge's network observations; it does not independently verify a
TLS transcript or GitHub signature. A coding agent or ordinary owner role cannot
submit these observations. A historical receipt does not describe current GitHub
state after later changes.

## Interrupted captures and limits

Reuse the same arguments and private directory after an interrupted command.
The journal persists the exact report/key before submission. If it was committed,
retry returns that receipt even after a server restart, without an App key or a
new GitHub read. Current service authorization is still required. Use a new
directory for a new capture; do not edit old reports to renew their timestamps.

Fresh reports must finish within the last 120 seconds, allowing 5 seconds of
forward clock skew, and span at most 120 seconds. A stale uncommitted report is
refused. The existing HTTP adapter bounds setup/read phases and responses at
128 KiB. Missing requested nullable GraphQL fields, partial responses, changed
PR/repository/branch identity and malformed queue entries fail the capture.

The collector attempts to revoke the token on success and error. A false
`specification.report.token_revoked` causes an unsuccessful CLI exit even if the
receipt was stored. Process termination can leave a token valid until provider
expiration; it is not itself revocation.

`restore_queue_context` takes `epoch`, `binding`, `manifest_sha256` and a
`subject` such as `{"kind":"dequeue","operation":"<UUID>"}`. It returns the
unchanged version-2 restore context, its digest, and the resolved queue target.
`restore_queue_record` takes `key`, `epoch` and `specification`; the
[version-1 wire fixture](../crates/metadata/tests/fixtures/restore-queue-v1.json)
documents the shape. Its static timestamps are only test data.
An ordinary repository `read` credential can retrieve a committed receipt:

```json
{"action":"restore_queue_status","key":"<key returned by the capture>"}
```

These operations leave old attempts, reports, source pins, bindings and dispatch
queues unchanged. Full recovery still requires check/delivery facts, discovery
and preservation of post-backup work, external fencing and verified reconnection.
For retained managed-check publications, see [exact check captures](restore-check-evidence.md).
