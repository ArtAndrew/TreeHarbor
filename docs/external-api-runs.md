# External agent API contexts — protocol v1

Available in [0.1.4-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.4-alpha.1). Earlier packages do not include these commands.

Use this path when your agent platform already provides execution, files and
isolation. TreeHarbor pins an exact source revision and accepts immutable changes
through its existing proposal API. It does not launch or observe your process.
The managed [files/runtime workflow](connected-pr-preview.md) remains available.
Repository authority is still Connected or Standalone; API delivery is neither a
third authority mode nor a different license edition.

## Requirements

Run the Rust service with PostgreSQL and its configured local object store. Import
an exact [native revision](native-import.md) (including a Git-derived revision),
then register a [source profile](source-profile-v1.md). This API path requires no
TreeHarbor node, Docker/gVisor installation, runtime profile or runtime credential.
The client remains responsible for its execution isolation, network and secrets.
The current transport is the private local Unix socket; this is not a remotely
exposed HTTP deployment.

Provision a repository-scoped credential for the owning controller with `read`,
`workspace.write` and `change.write`. These are separate permissions: creation
and closing require `workspace.write`, source/status reads require `read`, and
proposal submission requires `change.write`. They grant no validation, review,
GitHub publication or merge authority. Keep credentials outside untrusted code.

## One task

Choose and **save** a non-nil workspace UUID, run UUID and request key before the
first call. Keep all arguments and the key unchanged on uncertain retries.
Unlike `workspace-new`, these commands do not generate a local intent journal.
`BASE` must identify an exact registered `native:sha256:…` revision, not a branch.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  api-run-create --key "$CREATE_KEY" --authority connected --epoch 1 \
  --workspace "$WORKSPACE" --run "$RUN" --base "$BASE" \
  --profile-digest "$SOURCE_PROFILE" --agent my-controller/1 --task fix-parser

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-file --authority connected --epoch 1 \
  --workspace "$WORKSPACE" --generation 1 --path src/parser.rs \
  --output "$TH_PRIVATE/parser.rs"

# Produce changes in your own execution environment. Upload complete blob bytes
# and explicit expected-before operations using change-begin/put/submit:
# see change-proposals.md. Missing files are not implicit deletions.

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  api-run-status --run "$RUN"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  api-run-close --key "$CLOSE_KEY" --authority connected --epoch 1 \
  --run "$RUN" --generation 1
```

Use `--authority standalone` for a repository owned by TreeHarbor. This does not
certify the full Standalone release. Create a second context with distinct IDs
and key for another task; both may use the same exact base and propose edits to
the same file. Each accepted result is retained independently. Acceptance is not
integration: conflicting proposals still need rebase/merge and checks of the exact
combined candidate. Syntactically compatible changes can be semantically wrong.

`api_run_create` atomically creates a workspace with `delivery: api` and state
`source_ready`, and a run with `origin: external_api` and state `recorded`.
`recorded` means only that the authenticated caller registered a task. Responses
explicitly return `execution_observed: false`; no execution start time, runtime,
exit status, isolation or test success is inferred. This workspace has no files
materialized by TreeHarbor and cannot be used by `run-request` or filesystem seal.

Successful proposal receipts and retained version reads add
`run_origin: external_api` and `execution_observed: false`. The proposal still
identifies the exact run, base revision/snapshot, operations and computed result.
Current validation and merge rules are unchanged. Historical control-plane
receipts keep their previous JSON shape.

## Closing, retries and failures

Close atomically changes the run to `closed`, tombstones the workspace as
`deleted`, increments workspace generation and releases its source pin. The
ordinary `workspace-delete` command performs the same logical close for API
delivery. Neither stops an external process nor deletes its files. There is no
physical cleanup/removal receipt. Unexpected attached managed resources cause
refusal rather than bypassing their cleanup protocol.

New source reads and proposals fail after close. A race with acceptance either
retains a complete accepted version/receipt/pins before close or rejects the new
write; it cannot leave an acknowledged half-version. Accepted versions and their
independent graph pins survive close. Existing source-read/upload holds retain
their bounded storage lifetime but do not authorize further reads/writes of a
closed context. Incomplete uploads retain their existing expiry/abort rules.

The same request key and identical arguments return the saved committed receipt,
including after a lost response or close. That receipt is historical, not a claim
that the context remains open. Query `api-run-status` for its current state.
A different request under an existing key fails with `idempotency_conflict`.
Wrong owner/scope, revoked credentials, changed repository authority/epoch, stale
generation and closed contexts fail using the existing authorization/lifecycle
errors. Do not fix uncertain delivery by generating a new key or new IDs.
The managed `run-stop`/`run-status` endpoints do not describe external tasks.

## Wire contract and upgrade

`POST /v1/commands` uses the existing version-1 envelope, Bearer authentication
and fixed `Host: treeharbor.local` on the private socket. Unknown fields are
rejected, including caller-supplied origin, principal, runtime and success claims.
The [v1 vector](../crates/metadata/tests/fixtures/api-run-v1.json) fixes a creation
envelope, result and independently computed idempotency digest. Its sample object
IDs describe protocol values, not objects to import into a live repository.

Commands inside that envelope:

| Method | Required fields besides `method` |
| --- | --- |
| `api_run_create` | `key`, `authority`, `epoch`, `workspace`, `run`, `base`, `profile_digest`, `agent`, `task` |
| `api_run_status` | `run` |
| `api_run_close` | `key`, `authority`, `epoch`, `run`, `generation` |

UUIDs must use canonical lowercase spelling; creation IDs must be non-nil.
Agent/task labels use the existing identifier rules and limits of 128/256 bytes.
Success is `{ "version": 1, "result": { ... } }`. Status preserves the run's
original `generation`/`epoch`, reports the current `workspace_generation` and
`workspace_state`, and includes exact `base`/`profile_digest`, delivery and origin.
Close returns workspace/run IDs, new `generation`, `state: deleted`,
`run_state: closed`, delivery/origin and `execution_observed: false`.

Schema migration 032 is additive. Stop the service and workers, take a consistent
[backup](backup-restore.md) using the documented procedure, install the new binaries,
then run `admin migrate` with the new CLI before restarting the updated service and
workers together. Existing workspaces become `delivery: files`; existing runs
become `origin: control_plane`, which includes legacy trusted coordinator records
and does not falsely assert a managed runtime. Existing object formats, request
digests and saved receipts remain unchanged. Mixed-version/rolling upgrades and
downgrade after API records are created are unsupported.
