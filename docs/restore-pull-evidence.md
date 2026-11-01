# Capture an exact PR during recovery

Development command after 0.1.11; not included in the published 0.1.11 package.
Use it after [restore assessment](restore-github-assessment.md) to compare one
retained PR intent with an explicitly selected GitHub PR. It reads the PR twice
and saves the full observations, including title, body, draft status and exact
code identities. It does not publish, update, enqueue, merge or reconnect anything.

Choose the retained `pull` operation UUID and the PR number/node ID from the
assessment. If the operation already has a saved PR identity, that identity must
match. If its identity was lost, selection is explicit: the command does not adopt
a candidate just because its branch or commit matches. An operation absent from
the restored database is not supported by this comparison yet.

The operator must keep old publishers fenced. Use the original private App
configuration and a fresh `host.read` credential for the binding's assigned
bridge. The App token is narrowed to one repository with contents, metadata and
pull-request **read** permissions. The normal GitHub API endpoint is fixed;
response URLs cannot redirect requests to another host.

```sh
umask 077
mkdir -m 700 /absolute/private/treeharbor/pr-capture-001
cargo run -p treeharbor-service --locked -- github restore-inspect-pull \
  --config /absolute/private/treeharbor/github-app.json \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-bridge.json \
  --binding "$TH_BINDING" --epoch "$TH_EPOCH" \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  --operation "$TH_PULL_OPERATION" \
  --number "$TH_PR_NUMBER" --node-id "$TH_PR_NODE_ID" \
  --state /absolute/private/treeharbor/pr-capture-001
```

Set the variables to reviewed identities before running. Keep the state directory
private: it contains repository metadata and PR text. Use a new directory for a
new observation; reuse the same directory and arguments after an interrupted run.

The bridge compares the local restore context before and after the network reads.
The server checks it again while committing the report. Each snapshot must match
the selected PR number/node ID, repository and retained head/base branch names.
Changed title/body/draft or code can be recorded as a mismatch. A missing PR,
changed repository/branch identity, incomplete response or unsupported response
size produces an error; it is not converted into proof that no PR exists.

## Read the result

| Field | Meaning at capture time |
| --- | --- |
| `classification: matches_retained_target` | Both snapshots match, and head revision/title/body/draft match the retained intent. The complete body includes the original operation marker. |
| `classification: differs_from_retained_target` | The two snapshots agree, but their code or metadata differs from the retained intent. |
| `classification: changed_during_capture` | The two snapshots differ. Review and capture again before using them as current evidence. |
| `metadata_and_marker_match` | The server-computed comparison above; the caller cannot supply this flag. |
| `specification.report.token_revoked` | Whether token revocation succeeded. False also makes the command exit unsuccessfully, even if its observations were retained. |

`source_mapping_verified`, `reconciliation_complete` and
`publication_authorized` remain false. Comparing PR metadata does not verify
source bytes; use [source mapping verification](restore-source-verification.md)
for retained code. Two equal reads are not an atomic GitHub snapshot, do not rule
out a change-and-revert between reads, and do not prove uniqueness among other
PRs. A later host change does not modify this immutable receipt.

The service trusts the **assigned bridge** to report its network observations,
as with normal host inspections. It verifies identities, scope, timing and local
state; it does not independently verify a GitHub signature or TLS transcript.
An owner or coding agent cannot submit this evidence using their ordinary roles.

## Retry, limits and API

The private journal saves the exact report and request key before submission.
Retry sends that same report, including after server restart or loss of the local
acknowledgment. A committed report replays without an App key or new GitHub read,
but current TreeHarbor authorization is still required. This is historical replay,
not a fresh observation. Different request content with the same key is rejected.

For a fresh record, capture duration is at most 120 seconds and its end must be
within the preceding 120 seconds, allowing 5 seconds of forward clock skew. Keep
bridge/server clocks synchronized. A pending report that was never committed and
has expired or refers to a changed context is refused. Keep that journal for
investigation and capture afresh in a new directory; do not edit its old receipt.

The collector has bounded token/setup and 45-second read phases, existing
128 KiB HTTP response limits, titles up to 256 bytes and bodies up to 9,000 bytes.
Null bodies mean empty text; omitted body fields are rejected. It attempts token
revocation on read errors too. Process termination can leave a token valid until
its provider expiration; local shutdown alone is not revocation.

The authenticated API command is `restore_pull_record`, with a `key`, `epoch`
and `specification`. The [version-1 wire fixture](../crates/metadata/tests/fixtures/restore-pull-v1.json)
documents its full shape; its static timestamps are test data, not a reusable live
request. `restore_pull_status` with the committed key retrieves a historical
receipt using ordinary repository `read` permission:

```json
{"action":"restore_pull_status","key":"<key returned by the capture>"}
```

This command leaves old publication attempts, observations, source pins and
bindings untouched. Complete recovery still needs discovery of post-backup work,
queue/check/delivery facts and a separately verified reconnection procedure.
[Queue membership captures](restore-queue-evidence.md) cover retained enqueue
and removal intents without settling their old mutations.
