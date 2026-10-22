# Immutable ChangeSet rebase (v1)

An owner can reapply one accepted ChangeSet version onto an exact registered
native revision. This creates version N+1, a recomputed delta against that base,
and a single-parent native commit. It retains version N and the original AgentRun.
The original workspace can already be draining. No ref moves and no CI or review
evidence is created by rebasing. This development feature is Standalone only.

This is a one-ChangeSet operation, not rewriting a stack of historical commits.
Integration still creates its own candidate against the then-current target.
The original version remains available as an immutable proposal; its lineage is
retained separately from the new commit's source ancestry.

## CLI

Use an owner credential with `change.write` and the existing private local service
installation. Obtain the exact target revision through `client native` ref resolve
or `client history`; neither command below accepts a moving ref name.

```sh
treeharbor client --socket "$SOCKET" --credential-file "$OWNER" rebase-start \
  --key "$START_KEY" --epoch 1 --rebase "$REBASE_UUID" \
  --changeset "$CHANGESET_UUID" --source-version 1 --onto "$NATIVE_REVISION"

treeharbor client --socket "$SOCKET" --credential-file "$OWNER" rebase-prepare \
  --key "$PREPARE_KEY" --epoch 1 --rebase "$REBASE_UUID"

treeharbor client --socket "$SOCKET" --credential-file "$OWNER" rebase-status \
  --epoch 1 --rebase "$REBASE_UUID"

# Only a preparation that has not completed can be cancelled.
treeharbor client --socket "$SOCKET" --credential-file "$OWNER" rebase-cancel \
  --key "$CANCEL_KEY" --epoch 1 --rebase "$REBASE_UUID"
```

`ready` returns `result_version`, `revision`, `changeset`, `source_version`,
`run`, `onto`, `epoch`, `profile` and `failure: null`. Submit that new version to
the normal queue, prepare a new integration candidate and run its required checks
and reviews. Older pending entries for this ChangeSet become stale on successful
acceptance. Already landed entries and their history are retained. A deliberate
new enqueue of an older immutable version still requires its own current candidate
and evidence; rebase does not erase previously accepted proposals.

`conflicted` returns bounded structured path/kind details with a total count and
truncation flag. Inspect source versions and submit corrected edits from an
appropriate workspace/run. A conflict never contains invented resolution content
or an automatically approved tree. Unsupported cases and resource limits are
explicit `failed` results. See the [merge profile](native-merge.md).

## HTTP and command v1

The existing authenticated Unix HTTP transport, strict envelope, host/origin and
body limits apply. All operations require the same repository owner identity;
knowing a UUID is not authorization. Status also requires `change.write`.

Start/cancel use `/v1/commands` with the normal `{version:1,tenant,repository,
command:{method:"native",request:...}}` envelope. Requests are:

```json
{"action":"rebase_start","key":"start-123","epoch":1,"rebase":"ffeeddcc-bbaa-9988-7766-554433221100","changeset":"00112233-4455-6677-8899-aabbccddeeff","source_version":1,"onto":"native:sha256:<64 lowercase hex digits>"}
```

```json
{"action":"rebase_status","epoch":1,"rebase":"ffeeddcc-bbaa-9988-7766-554433221100"}
```

```json
{"action":"rebase_cancel","key":"cancel-123","epoch":1,"rebase":"ffeeddcc-bbaa-9988-7766-554433221100"}
```

Preparation uses `POST /v1/changes/rebase`, with a strict body:

```json
{"version":1,"tenant":"example","repository":"00112233-4455-6677-8899-aabbccddeeff","key":"prepare-123","epoch":1,"rebase":"ffeeddcc-bbaa-9988-7766-554433221100"}
```

Responses use `{version:1,result:...}`. Reuse the original key after transport
failure or timeout: success may already have committed. Changed input with that
key is an idempotency conflict. A committed receipt is replayed even after the
deadline, superseding versions or authority changes, but only after checking
current credential permissions. A fresh operation under an old epoch is fenced.
`unexpired` in a replay is the value recorded at commit; use status for current
state. Status is itself fenced by the operation's current authority epoch.

## Correctness and storage

Start fixes source version, exact new base, computation profile, service timestamp
and a 300-second deadline. The source must still be the latest owned version at
start and final acceptance. A concurrent original upload or another rebase wins
or loses an atomic version comparison; it cannot overwrite the winner.

Scoped read leases protect the accepted source base, operations, result and new
base. Auth/epoch/state/latest-version checks repeat on reads and object reservations.
The core verifies original operations, performs a three-way snapshot merge, derives
and verifies the new delta, then creates one new commit with `onto` as its sole
parent. Service attribution and a stable label identify the operation, source
version and original run; original Git author/signature claims are not copied.

Generated bytes are fsynced before acceptance. One PostgreSQL transaction records
the immutable version, lineage, source/commit pins, registered native base,
supersession of older pending candidates/queue entries, receipt and outbox event.
It grants no merge authority. A running managed check for a superseded candidate
is fenced by the normal execution controls. The new candidate requires new checks.

Schema 12 is additive. Stop old service processes and run the existing admin
migration before starting the new binary. The populated schema-11 upgrade test
retains accepted versions, policies, checks, pins and receipts. Do not run an old
binary after migration. The source export of the new revision includes its complete
commit ancestry; it is not a full metadata/lineage backup or all-proposal export.

## Profile, limits and retry identity

Profile v1 is exactly:

```text
treeharbor-change-rebase-v1/treeharbor-native-merge-v2/diffy-0.5.2/exact-sibling-renames/subtree-delta
```

The versioned reservation hash input is:
`"treeharbor-rebase-object\0v1\0" || u64be(tenant UTF-8 byte length) || tenant ||
repository UUID bytes || rebase UUID bytes || raw 32-byte object digest`.
Use the first 16 SHA-256 bytes as the UUID without changing variant/version bits.
An independent Python producer and JSON vector live in
`crates/metadata/tests/fixtures/change-rebase-v1.*`. This namespace is distinct
from candidate/upload reservations. Repeats reuse the same physical generation and
hold; hold deadlines are clamped to the original operation deadline.

Current computation scratch is bounded to 4,096 objects and 128 MiB of canonical
frames. Aggregate core reads/writes each have a 128 MiB budget across verification,
merge and delta generation. Tree/text/rename limits from the merge profile also
apply. Unchanged blobs are not loaded by the computation loader. Requests use the
existing server timeout, semaphore and pool limits. Cancelled/failed preparations
can leave unpinned intermediate objects; automatic reclamation remains unfinished,
and existing catalogue retention rules must be used. Semantic correctness is
determined by checks/review; syntactic merging does not prove it.
