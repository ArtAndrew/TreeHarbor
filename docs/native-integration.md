# Native integration (development)

The local service can now enqueue an accepted native ChangeSet, compute an
immutable merge candidate, collect scoped validation/review receipts and advance a
native ref in one PostgreSQL transaction. This guide covers the operator-assisted
Standalone workflow. It uses a local PostgreSQL database, the local object store
and a native repository. Automatic queue and CI scheduling and migration between
repository authority modes remain separate work.

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
Run the native queue, validation and review commands over the private Unix socket.
The [agent HTTPS endpoint](agent-https.md) covers external task and proposal
operations.

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
| `ref_snapshot` | None | `read` |
| `handoff_freeze` | None | `repository.write` |
| `handoff_snapshot` | `epoch`, `freeze_key` | `read` |
| `handoff_resume` | `freeze_key`, `target_stays_fenced` | `repository.write` |
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

### Capture the current branch tips

Send `{"action":"ref_snapshot"}` on the local `native` command. The v1 result
contains `version`, `tenant`, `repository`, `authority`, `epoch` and `refs`.
Each entry has the `name`, `head`, `snapshot`, `generation`, `epoch` and `policy`
returned by `ref_resolve`. The service reads the complete set under a shared
repository lock, in branch-name byte order. An empty repository has `refs: []`.
More than 128 branches returns HTTP 413 with `native_ref_snapshot_limit`.
An inactive or changed authority returns `repository_fenced`.

Use the returned exact heads and epoch for [source export](live-source-export.md)
and [one Git bundle](git-export.md#export-several-branches). The snapshot
describes one instant. The caller must stop branch writers and verify the final
tips separately for an authority transfer.

Use the [frozen handoff export](git-export.md#freeze-standalone-writes-and-export-a-handoff-bundle)
to keep the captured refs stable while preparing a reverse transfer. The
`handoff_snapshot` read takes the frozen epoch and the freeze key.

### Start a new project with empty history

Use [0.1.17-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.17-alpha.1)
or a newer build whose `source-pack --help` lists `--empty`. Create a repository
with `admin repository-create --authority standalone` and set up the service
and separate roles above. Use an owner credential with `repository.write` for
the import and initial ref. A policy administrator registers the protected
[check program](managed-checks.md) and records the policy with `policy_put`.

With an absolute owner-private `TH_PRIVATE` directory, a running service at
`TH_SOCKET` and an owner credential file at `TH_OWNER`, create and import the
initial revision.

```sh
umask 077
treeharbor source-pack --empty --output "$TH_PRIVATE/genesis" \
  --author-name 'Example Maintainer' --author-email 'maintainer@example.invalid' \
  --message 'Initial empty revision'
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  source-import --pack "$TH_PRIVATE/genesis" --state "$TH_PRIVATE/genesis-import.json" \
  --authority standalone --epoch 1
```

Set `TH_INITIAL_REVISION` to the returned `result.revision` and `TH_POLICY`
to the digest returned by `policy_put`. Create `main` at that exact revision
with the policy in force.

```sh
(set -C; cat > "$TH_PRIVATE/create-main.json") <<EOF
{"action":"ref_create","key":"initial-main","epoch":1,"name":"main",
 "head":"$TH_INITIAL_REVISION","policy":"$TH_POLICY"}
EOF
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  native --request "$TH_PRIVATE/create-main.json"
```

Resolve `main` with `ref_resolve`. It starts at generation 1 and has an empty
tree and one parentless commit. Register a full source profile with
`profile-put --full`, then give the first agent the exact revision and
profile digest. The first file can come from a managed stopped-file capture
or an API proposal with `expected: null`. Enqueue it and use the same protected
check and review workflow as every subsequent change. Keep the import state
and the ref request unchanged when retrying after a lost reply.

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

### Installed two-agent overlap drill

Development source also accepts `--shared-node`: two different agent owners use
one bounded node worker. The first task is stopped, captured and cleaned up while
the second keeps writing. Exact retries and rejection of a wrong-node cleanup
precede the same integration drill below. See the [operator interface](node-work-selection.md)
and [local trial receipt](evidence/shared-node-work-20261109.json). This new option
is not present in the 0.1.15-alpha.1 package; it is included in the subsequent
installed-package CI and release checks.

Two scripted gVisor agents start from one native revision and independently
edit `same.txt`. The first proposal is reviewed, checked and landed on a
protected `main`. The second queue entry then reports a content conflict and
the ref stays on the first result. Its owner prepares a new workspace from
that result and submits a resolved version of the same ChangeSet. The queue
requires a fresh review and an actual check on the new candidate. The drill
reads native history after workspace cleanup and then checks the optional
export by restoring the final bytes and earlier revisions in ordinary Git.

The current drill first creates `main` with an empty tree and publishes its
first file through an actual managed check and independent review. Both
overlapping agents then start from that published revision. Match the test
script to the source revision of your binary when reproducing an older release.

The full installed drill also backs up this overlapping history and reopens it
from a separate PostgreSQL database and object store. It compares the complete
native graph and first-parent view, including ordered merge parents, and checks
the projected Git graph. Removing or corrupting the second parent in the
restored copy fails both complete history queries and export. Restoring the
original bytes recovers the same graph. Run the script without
`--overlap-only` to exercise this recovery, replacement-node validation and
cleanup with a disposable database.

Run the focused drill in a disposable Linux runner with Docker configured for
gVisor, a disposable PostgreSQL container reachable through a local Unix socket,
and permission to mount temporary ext4 volumes. Set `TH_RUNTIME_PG_CONTAINER`
and `TH_RUNTIME_DATABASE_URL` as in the runtime jobs in [CI](../.github/workflows/ci.yml).
Use absolute paths from the verified installation.

```sh
cargo build -p treeharbor-runtime --example runtime_probe --locked \
  --target "$(uname -m)-unknown-linux-musl"
TH_TEST_SERVICE_BINARY=/absolute/install/bin/treeharbor \
TH_TEST_RUNTIME_INIT=/absolute/install/bin/treeharbor-runtime-init \
  python3 scripts/test-native-checks.py --overlap-only
```

The installed ARM64 run used a network namespace with no external interfaces
and traced 207 native TreeHarbor invocations. The trace observed zero subprocess
executions and zero IPv4 or IPv6 connection attempts before the separate Git
projection. This evidence covers one scripted workflow. [Installed ARM64 receipt](evidence/standalone-agent-overlap-20261109.json)

A second run installed the signed `0.1.20-alpha.1` package on a new Ubuntu ARM64
VM with Docker, gVisor and PostgreSQL. It used a prebuilt agent test probe and
started from an empty repository. The two agents reached a real file conflict,
the resolution passed a fresh check and review, and ordinary Git recovered the
landed bytes after cleanup. The trace recorded 270 native calls with zero child
processes and zero IPv4 or IPv6 connections. The Git projection was run separately.
[Clean installation receipt](evidence/standalone-clean-install-20261112.json)
