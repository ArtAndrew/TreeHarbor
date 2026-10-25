# Exact GitHub workflow attempts (development)

A workflow's current attempt number does not identify the attempt of every check
in its history. TreeHarbor can now resolve a selected retained merge-group check
to an exact workflow attempt, job and check ID. It records a later current attempt
separately. This is provenance evidence, not merge authorization.

## Collect

Read [`queue_checks`](github-group-checks.md). Select its capture `sequence`, a
`before`/`after` side, a suite ID and a check run ID from that same side. Use a
`merge_group` workflow and a check matching the queue's original required App/name.
The service derives the exact attempt from that saved workflow observation.

```sh
treeharbor github queue --config /private/treeharbor/github-app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/bridge.json \
  --queue "$QUEUE_ID" --epoch 1 --state /private/treeharbor/queue-state \
  --check-report 5 --check-side after \
  --check-suite "$SUITE_NODE_ID" --check-run "$CHECK_NODE_ID"
```

`5` is an example check-report sequence, not a membership sequence, PR number or
workflow ID. Use the original private queue journal. This option requires an
existing attempt, never enqueues and cannot be combined with `--group-report`
or `--attempt-report`.
Pending journal delivery happens first and works without the App configuration;
rerun after recovery to request a new provider read.

The assigned bridge retains its local `host.merge` permission. This operation
additionally requests **Actions: read** in the repository-scoped App token, alongside
Contents, Metadata, Pull requests and Checks read. The operator must grant the App
this permission before using this collector. Existing membership/check collectors
keep their narrower tokens. No workflow is started, rerun or cancelled. There is
no unauthenticated fallback when an App permission is missing.

## Read

Save this command as `attempts.json`, replacing the queue UUID and epoch:

```json
{
  "action": "queue_attempts",
  "epoch": 1,
  "queue": "66666666-6666-4666-8666-666666666666",
  "after": 0
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/reader.json github --request attempts.json
```

This local API requires current repository `read` permission. One version6
capture is returned per page; continue with `next_after` until null. Each capture
contains the original report and:

| Field | Meaning |
|---|---|
| `attempt_linked` | Both reads agree and the retained check exactly matches a job/check in the selected attempt |
| `current_attempt_matches` | Both observed current attempt numbers equal the selected historical attempt |
| `source_checks_consistent` | The original version5 check snapshots agreed |
| `definition_verified` | Always false at this milestone |

`attempt_linked: true` can describe a failed check or an old attempt. It says
nothing about successful validation of a later attempt. A missing job, changed
check outcome or disagreement between reads does not inherit the retained success.
`merge_verified` and `host_policy_verified` remain false. The service does not
create native validations or change queue authorization from these reports.

## Recovery and limits

The service independently validates the original queue → membership → check →
attempt references. Later missing live membership does not erase this history.
Version6 reuses the report ledger and private journal and does not replace the
latest queue snapshot. Upgrade CLI and service together; no schema migration.

The collector resolves numeric IDs through fixed GraphQL queries and verifies
opaque identities, repository, suite and exact SHA before constructing REST
routes. It reads the selected attempt, its complete bounded job list, the exact
check and current workflow twice. Job and check node IDs are not assumed equal.
Provider URLs are validated as metadata and never followed. Pagination links,
incomplete counts and foreign identifiers fail collection.

Limits are 100 jobs per snapshot, 20 referenced workflows per attempt, 128 KiB per
provider response and 48 KiB for the encoded report. The byte limits may bind
first. Two reads are not an atomic snapshot of GitHub or proof of continued
eligibility. Executed-file/referenced-workflow metadata is retained, not trusted.

Contract, local HTTP, PostgreSQL and actual CLI/server process-death tests pass.
The [actual Rust installed-App diagnostic](evidence/group-attempt-read-20261024.json)
also passed on the public acceptance repository after granting Actions-read. It
linked the original merge-group check to attempt 1, observed the same current
attempt and revoked its narrowed token. Its source was a previous provider
diagnostic, not a product membership record; this is not end-to-end queue proof.
Private-repository entitlement, trusted definitions/policy and final
published-history proof remain release gates.

An explicitly selected attempt can now supply [executed root-file evidence](github-workflow-policy.md). That separate collection still does not certify runtime context or final merge.
