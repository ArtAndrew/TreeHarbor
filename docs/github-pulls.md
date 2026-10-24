# GitHub pull request delivery

Development API and CLI. After an exact [branch publication](github-publications.md)
is observed, a repository owner can request a PR or update the title and body of
one previously delivered PR. This command does not approve checks or merge code.
Two [actual installed-App PR deliveries](evidence/connected-live-20261024.json)
and their completed-result recovery passed on a public synthetic repository.
Complete merge-queue integration remains a release gate; local host fixtures do
not establish real provider failure behavior or final integration.

## Request and deliver

Use a Human credential with `repository.write` for the request. The selected
publication must be observed, its projection ready and current, and its binding
active in the same Connected authority epoch. Save the following as `pull.json`,
replacing both UUIDs with your publication and a new pull-intent UUID:

```json
{
  "action": "pull_request",
  "key": "open-selected-change",
  "epoch": 1,
  "pull": "44444444-4444-4444-8444-444444444444",
  "specification": {
    "publication": "33333333-3333-4333-8333-333333333333",
    "base": "main",
    "title": "Update selected content",
    "body": "Description of the proposed change.",
    "draft": true,
    "previous": null,
    "ttl_seconds": 600
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/owner.json \
  github --request pull.json
```

The server derives the generated head branch and exact expected Git revision from
the publication. It appends an opaque operation marker to the body. Do not remove
that marker during delivery or recovery. The caller cannot supply repository IDs,
arbitrary head refs or an API URL. A base name has at most 128 ASCII characters;
components start with an alphanumeric character and contain alphanumerics, `.`,
`_` or `-`, with Git-unsafe forms rejected. Titles are limited to 256 bytes and
owner bodies to 8,192 bytes. The deadline is at most an hour and is capped by the
projection deadline. Only one unresolved PR intent may exist per generated branch.

On the trusted bridge host, use the private [App configuration](github-connection.md)
for that binding and its assigned Bridge credential with `host.publish`:

```sh
install -d -m 700 /private/treeharbor/pulls/change-1
treeharbor github pull \
  --config /private/treeharbor/app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/publisher.json \
  --pull 44444444-4444-4444-8444-444444444444 --epoch 1 \
  --state /private/treeharbor/pulls/change-1
```

This JSON transport works on the supported Linux/macOS hosts without a Git
executable. It connects directly to GitHub.com with redirects and proxies disabled.
The App installation needs **Pull requests: write**, **Contents: read** and
**Metadata: read** for this command. Fresh tokens are narrowed to the one numeric
repository. Recovery requests only Pull requests-read plus Contents/Metadata-read.
Branch publication separately requires Contents-write. Agent workspace processes
must not receive either the App key or bridge credential.

## Retry, status and cancellation

Run the same command with the same state directory after interruption. It retains
the immutable scope/target, single attempt and observed reports in private files.
The server issues a mutation grant only once; replaying a successful begin never
issues another. An existing attempt uses read-only recovery. Missing results,
timeouts and ambiguous matches remain uncertain; they do not permit another POST
or PATCH. This is not a guarantee of exactly-once remote creation.

The command searches all open/closed PR states by exact head/base and marker, or
reads a previously retained PR number. It checks numeric head/base repository IDs,
ref names, PR number/node ID and observed revisions. Searches are bounded to 100
entries over 20 pages; exhaustion is an error, not proof of absence. It ignores
provider-supplied URLs. Concurrent edits between list and detail reads can make a
result uncertain. Keep the original intent for reconciliation.

Submit these through the same `client ... github --request FILE` interface:

```json
{"action":"pull_status","epoch":1,"pull":"44444444-4444-4444-8444-444444444444"}
```

```json
{"action":"pull_cancel","key":"cancel-selected-pr","epoch":1,"pull":"44444444-4444-4444-8444-444444444444"}
```

Status requires `read`; cancellation requires `repository.write`. States are
`requested → attempting → observed / uncertain`. An uncertain attempt can later
become observed. Cancellation before the attempt produces `cancelled`; afterward
it records `cancel_requested` without claiming to undo a remote effect. Terminal
observations remain historical: `observed` says the exact intended fields and head
were seen, not that the PR is still current, open, validated or merged. Closed and
merged results are retained as facts. `merge_authorized` is always false here.

Exit zero means `observed`; other states return nonzero. Terminal results and
pending durable report recovery do not need the App file. An OS lock prevents two
drivers sharing one directory; the server's one-shot grant also applies across
different directories. Reports can recover after expiry, cancellation or binding
revocation, but require the original bridge's current credential and authority epoch.

The private journal records known token expiry. Failed or unknown cleanup remains
separate from successful PR observation: revoking a new read token does not prove
the original write token was revoked. No token/key is stored in the journal or
database. Interrupted token creation can still leave a token alive until expiry.

## Update an existing PR

Publish a new version to the same generated branch for code changes. For an
explicit title/body update, request a new pull intent with `previous` set to the
UUID of the earlier observed pull intent. The service derives its PR number,
node ID and previous text. The base branch and draft status must remain unchanged.
The adapter checks old metadata and exact current head before one PATCH containing
only the selected title/body. A changed, closed or merged PR is not overwritten.

GitHub's metadata PATCH has no documented compare-and-swap parameter. A human
edit between that check and PATCH may be overwritten; this command cannot promise
atomic conflict protection for PR text. It reads the PR again after the attempt.
Changes to the source branch likewise require fresh checks before eventual merge.

## Bridge API and verification boundary

All bridge actions require `host.publish`, `epoch` and `pull`:

| Action | Additional fields | Result |
|---|---|---|
| `pull_context` | None | Retained intent and assigned binding |
| `pull_begin` | `key`, fresh `attempt` UUID | Receipt and one-shot `dispatch_granted` |
| `pull_dispatch_context` | Original `attempt` | Current eligibility; no new grant |
| `pull_report` | `key`, report | Scoped observation, never merge permission |

Reports contain `attempt`, `binding`, `git_revision`, `observed_at_unix_ms`,
`token_revoked` and `pull` (null or the exact snapshot returned by the Rust adapter).
The service trusts its assigned bridge reporter; this is not a cryptographic proof
of a GitHub exchange. Mutations reject reuse of a key with a different payload.
Request/dispatch checks run again after transactional receipt/outbox writes.

Schema 19 adds retained intents and full-graph pins without changing existing
publication records or permissions. Tests cover populated upgrades, scope/epoch/
revocation fences, concurrent claims, lost POST/PATCH responses, closed/ambiguous
searches and actual CLI/server process loss. Automatic uncertain-outcome resolution,
retention cleanup, broader real provider failure testing and the full merge
workflow remain unfinished.

Use [exact check observations](github-inspections.md) after PR delivery to retain
selected check evidence. A PR observation alone does not satisfy validation.
