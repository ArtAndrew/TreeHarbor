# Native integration (development)

The local service can now enqueue an accepted native ChangeSet, compute an
immutable merge candidate, collect scoped validation/review receipts and advance a
native ref in one PostgreSQL transaction. This is a development increment of
Standalone. Connected/GitHub publication, automatic queue/CI scheduling,
full backup/restore and an installable release remain unfinished.

A trusted validator reporting `passed` is an assertion by that service identity.
The external reporting API does not execute tests or turn an agent's exit code
into validation. Approved offline commands use [managed checks](managed-checks.md).
Operators must keep validator, integrator and human-review credentials out of
agent workspaces. The transport tests use explicit CI/runtime fixtures; the
separate Linux runtime tests exercise actual agent execution.

## Roles and setup

Use the [local service guide](local-service.md) to build the binary, migrate the
disposable database and configure local object storage. Migration 010 upgrades the
published schema checksum chain and preserves existing proposals and credentials.
The local service uses a private Unix socket; remote TLS transport is not ready.

Provision repository-scoped principals/tokens with these permissions:

| Actor | Kind | Permissions for this workflow |
|---|---|---|
| Bootstrap/ref administrator | `human` | `repository.write`, `policy.write`, `read` |
| Proposal owner | `agent` or `human` | `change.write`, `queue.request`, `read` |
| Integration worker | `integrator` | `integration.control`, `read` |
| Result collector | `validator` | `validation.report`, `read` |
| Reviewer | `human` | `review`, `read` |

Workspace creation also needs `workspace.write`. No role receives powers merely
because of its name; both a current repository grant and a narrowed token are
required. Agents cannot receive review, policy or integration permissions.
Example operator provisioning (using your existing private config):

```sh
treeharbor admin --config config.json principal-create \
  --tenant team --principal integrator --kind integrator
treeharbor admin --config config.json access-set \
  --tenant team --repository "$REPOSITORY" --principal integrator \
  --permission read --permission integration.control
treeharbor admin --config config.json token-issue \
  --tenant team --repository "$REPOSITORY" --principal integrator \
  --permission read --permission integration.control --output integrator.json
```

Do not commit config or credential files. Command request files also require
owner-only regular-file permissions, as with existing source/proposal manifests.

## CLI and wire v1

Native control commands use the existing authenticated `POST /v1/commands`
envelope and one strict nested command:

```json
{
  "version": 1,
  "tenant": "team",
  "repository": "00112233-4455-6677-8899-aabbccddeeff",
  "command": {
    "method": "native",
    "request": {
      "action": "ref_resolve",
      "name": "main"
    }
  }
}
```

The CLI supplies the outer envelope from its scoped credential file. Save only
the `request` object in `request.json`, then run:

```sh
treeharbor client --socket service.sock --credential-file integrator.json \
  native --request request.json
```

All mutations below include `key` (stable idempotency key) and `epoch` (positive
current authority epoch). These commands always require Standalone authority.
Unknown fields are rejected. UUIDs use canonical lowercase hyphenated spelling;
object/policy digests are `sha256:<64 lowercase hex>` and revisions are
`native:sha256:<64 lowercase hex>`. Read actions omit key/epoch.

| Action | Additional fields | Permission |
|---|---|---|
| `policy_put` | `policy` (see below) | `policy.write` |
| `ref_create` | `name`, `head` (registered native revision), `policy` (digest) | `repository.write` |
| `ref_policy` | `name`, `generation` (expected current), `policy` | `policy.write` |
| `ref_resolve` | `name` | `read` |
| `enqueue` | `entry` UUID, `target`, `changeset` UUID, `version` | `queue.request`, proposal owner |
| `queue_pending` | `target`, `after` (nonnegative position), `limit` (1–100) | `read` |
| `queue_status` | `entry` UUID | `read` |
| `cancel` | `entry` UUID | `queue.request`, entry owner |
| `candidate_start` | `entry` UUID, new `candidate` UUID | `integration.control` |
| `candidate_status` | `candidate` UUID | `read` |
| `candidate_fail` | `candidate`, bounded `reason`, `conflict` boolean | `integration.control` |
| `check_assign` | `candidate`, check `name`, new `run` UUID, `validator` principal | `integration.control` |
| `validation_report` | `run`, `receipt` UUID, `outcome`, `evidence` digest | assigned validator |
| `validation_revoke` | `run` UUID | assigned validator |
| `review` | `candidate`, `review` UUID, `decision` | allowed human reviewer |
| `review_revoke` | `review` UUID | original reviewer |
| `land` | `candidate` UUID | `integration.control` |

### Discover pending work

The `queue_pending` read action (available since 0.1.5-alpha.1) lists `queued`, `preparing` and
`checking` entries for one target in queue position order. It does not start a
candidate or grant permission to integrate it. For example, save this request
with owner-only permissions and use the existing `client native --request` command:

```json
{"action":"queue_pending","target":"main","after":0,"limit":50}
```

The response contains `target`, `entries` and `next_after`. Each entry contains
`entry`, `position`, `epoch`, `changeset`, `version`, `state` and `candidate`.
Use a non-null `next_after` as the next request's `after`; null means no further
pending entries were observed. The target must exist in the credential's
repository. Current read permission is checked within each transaction.

This is a live view, not an immutable event log or a reservation. Entries can
finish or new entries can commit between pages. Restart at `after: 0` for each
polling sweep so newly visible work is not skipped. Use `queue_status` for a known
terminal entry; use the existing candidate-start command, with its concurrency
and ordering checks, to claim work. Positions are cursors, not counts.

Validation outcomes are `passed`, `failed`, `error`, `cancelled`; review decisions
are `approve` and `reject`. A proposal author cannot approve their own candidate.

An example policy (replace digest values with identities of the actual check
specification, environment, toolchain and dependency manifest):

```json
{
  "version": 1,
  "checks": [{
    "name": "tests",
    "definition": "sha256:0707070707070707070707070707070707070707070707070707070707070707",
    "environment": "sha256:0808080808080808080808080808080808080808080808080808080808080808",
    "toolchain": "sha256:0909090909090909090909090909090909090909090909090909090909090909",
    "dependencies": "sha256:0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a",
    "validators": ["ci"]
  }],
  "reviewers": ["reviewer"],
  "approvals": 1
}
```

Policies require 1–32 distinct checks, 1–16 permitted validators per check,
1–32 reviewers and a positive approval threshold no greater than reviewer count.
Checks are sorted by name and principal sets lexically. Canonical UTF-8 JSON has
no whitespace, fields in the order shown, and a SHA-256 digest over
`treeharbor-native-policy\0v1\0` followed by those bytes. The
[independent vector generator](../crates/metadata/tests/fixtures/native-integration-v1.py)
and [fixed vector](../crates/metadata/tests/fixtures/native-integration-v1.json)
define the exact policy and reservation encodings. Policies are immutable; selecting
a policy increments the ref generation even when reselecting the same digest.

## Compute, check and publish

1. Import native source and submit immutable proposals using the
   [source import](native-import.md) and [proposal](change-proposals.md) paths.
2. Put a policy and create a named ref from a registered native revision.
3. Each owner enqueues its accepted ChangeSet version. The oldest nonterminal
   entry per target must be started first; there is only one preparing/checking
   candidate per ref. Duplicate active submissions return the existing entry.
4. The integrator starts a candidate, then requests server computation:

```sh
treeharbor client --socket service.sock --credential-file integrator.json \
  native-prepare --key prepare-001 --epoch 1 --candidate "$CANDIDATE"
```

This sends `POST /v1/native/prepare` with fields `version:1`, `tenant`, `repository`,
`key`, `epoch`, `candidate`. It accepts no proposed result snapshot or check status.
The response is `{ "version":1, "result":{...} }` and identifies exact inputs,
profile, resulting revision/snapshot and state. Retry the same key after timeout
or response loss. A saved result is returned without repeating publication.

5. `check_assign` returns the exact revision and check context. The trusted external
   collector must actually verify that revision using the declared inputs before
   `validation_report`. A new run UUID replaces the current run for that check;
   old reports cannot satisfy it. Reusing a run UUID is rejected.
6. Read the exact prepared result with `diff --authority standalone --epoch 1
   candidate --candidate "$CANDIDATE"` using a credential with repository `read`
   permission, then record human review of that candidate. See
   [review diffs](review-diff.md) for context, paging and limits.
   `land` requires all current checks and
   enough distinct, allowed, non-author reviewers. A current rejection blocks.
7. Inspect `queue_status`, `candidate_status` and `ref_resolve`. A successful land
   atomically updates the ref, retains its complete parent graph, appends a ref
   audit record, completes the queue entry and records an idempotent receipt/outbox.

The core re-applies the original operations to their original base. It creates a
proposal commit parented by that base and an integration commit whose first parent
is the current target and second parent is the proposal. The integration delta is
recomputed and verified against the current snapshot. Commit messages bind the
merge profile; candidate inputs and identities are immutable.

Each check context binds repository, epoch, exact revision/tree, target name,
expected target head/generation, policy, check definition, environment, toolchain,
dependencies and current run UUID. Before publication, one short transaction
rechecks the target, current policy, roles/grants, explicit token revocations,
check runs and review. Even two definitive receipts saying `passed` block admission;
reschedule with a new run rather than choosing a convenient receipt.

Credential expiry prevents new submissions; an explicit token revocation also
invalidates pending evidence. There is no success cache across candidates. Retrying
a completed operation returns its historical receipt, not a claim that its old
state is still current; query status for the latest state.

## Failures and resource bounds

Queue states: `queued → preparing → checking → landed`. Preparation may terminate
as `conflicted` or `failed`; policy changes invalidate active candidates as `stale`;
the owner can `cancel` any unlanded entry. Accepting a newer ChangeSet version,
including through rebase, makes that change's older pending entries/candidates
`stale`. Already landed history is preserved. A new entry/candidate is required after a
terminal failure. Conflict results contain byte paths encoded as hex, kind, total
count and a truncation flag (at most 16 details/30 KB). Conflict markers are never
published. Semantic incompatibility still requires tests and review.

Merge computation uses the [bounded native profile](native-merge.md). Service
scratch is additionally limited to 4,096 objects / 128 MiB encoded data. The loader
scans the three directory trees, then fetches blobs on changed paths and explicit
replacement blobs. Unchanged file bodies and parent history are not fetched.
Currently reads are sequential and metadata is scanned in full; no speed claim
is made. Resource errors before computation leave the entry preparing so an
operator can cancel/fail it; corrupt/IO failures also leave it retryable.

Preparation has a fixed one-hour deadline, five-minute source read leases and
bounded deterministic object reservations. Holds derive from tenant, repository,
candidate and object identity; restart cannot extend the preparation deadline.
No computation, fsync or CI run occurs under a long database transaction.
Committed candidates have persistent pins; their preparation deadline does not
expire an ongoing check. The merge permit stays held by blocking work even if its
request is cancelled. Temporary holds expire after process loss.

This increment conservatively retains registered candidate bases, published
history and evidence. Automatic expiry handling, abandoned-candidate cleanup,
review UI, CI scheduling, full history recovery and bounded long-term retention
still need implementation. [Source export](live-source-export.md) and optional
[Git projection](git-export.md) now preserve registered history;
[history queries](native-history.md) expose its commits and ancestry. This is
not a complete standalone release.

## Reproduce verification

```sh
cargo test -p treeharbor-merge --locked
cargo test -p treeharbor-metadata --test intake integration:: --locked -- --ignored
cargo test -p treeharbor-service --test transport native:: --locked -- --ignored
```

The last two require the disposable PostgreSQL variables described in
[database development](database-development.md). The CLI/service test imports a
real native pack, submits three independent agent proposals, lands two compatible
edits of one file, reports the third conflict, kills the server before catalog
publication and after candidate commit, and reads all durable published ancestry
again. Git is absent from both server and client PATH. Test runtime lineage and
validator outcomes are explicitly fixtures, not actual CI execution evidence.

For approved offline commands, use [managed native checks](managed-checks.md).
They derive evidence from bounded execution and reject manual validation reports
for registered programs. External collectors remain a separate trust boundary.
