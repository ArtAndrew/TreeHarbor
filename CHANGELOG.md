# Changelog

## 0.1.16-alpha.1 — shared Copy-on-Write workspaces — 2026-11-09

- Add an operator-only Copy-on-Write preparation path with one verified source
  image per node, separate writable workspace layers and guarded runtime views.
  A bounded batch prepares independent slots concurrently.
- Add a bounded node worker for exact run, capture and cleanup selections.
  A local Linux trial stopped, captured and deleted one agent's workspace while
  another agent kept writing through a separate view of the same source image.
  Owner requests and acceptance remain explicit. See [the operator command](docs/node-work-selection.md).
- Support an explicitly registered source filesystem with durable ownership
  and writeback checks. Missing or mismatched placements fail closed.
- Include a Rust mount helper in the Linux archive, with the same dependency
  reports, checksums and provenance as the service and runtime init. Installation
  grants no host privileges; the optional helper needs a protected host copy and
  explicit administrator approval. See [the setup and recovery profile](docs/experimental-cow.md).
- Recover the same two running containers after terminating a shared worker.
  Exercise exact mount replay, independent lock contention and refusal of a
  different source or unprotected helper before the existing capture/conflict/export drill.
- Publish first/repeat four-workspace measurements and raw data, including
  cases where Git worktree remains faster. Measurements identify the earlier
  development build and do not claim new release measurements or production readiness.
- Publish signed Linux x86-64 and ARM64 packages after installed shared-worker
  recovery checks. Independent verification matched the original archives,
  all signed subjects and the exact producing workflow. A separate ARM64
  installation passed the same-node recovery, overlapping-task and Git-export
  scenario. See the [release evidence](docs/evidence/cow-workspace-release-20261109.json).

## 0.1.15-alpha.1 — faster repeat Connected workspaces

- Stage authorized Git history in one private scratch file.
- Reuse verified Git metadata on the same node for repeat preparations of
  the same authorized revision. Each workspace receives its own index, refs
  and object files.
- Reuse the validated object-store directory within protected reads and
  buffer baseline manifest writes with the existing durability barriers.
- Publish paired Linux ARM64 measurements for fresh and repeated
  Git-compatible workspaces. See the [preparation benchmark](docs/benchmark-preparation.md#protected-reads-and-manifest-writing--9-november-2026).

## 0.1.14-alpha.1 — faster workspaces on guarded Linux volumes

- Synchronize staged source files once on an exclusive Linux ext4 volume and
  report its writeback time in preparation diagnostics.
- Let the Connected PR example select prepared Git-compatible workspaces explicitly.
- Exercise two conflicting agent edits on one protected native branch, a resolved
  version with a fresh check and review, and a separate Git history export.
  See the [installed Standalone drill](docs/native-integration.md#installed-two-agent-overlap-drill).

## 0.1.13-alpha.1 — Git-compatible Connected workspaces — 2026-11-08

- Prepare full Connected workspaces with private Git metadata for agents that
  use Git locally. The checkout starts at the exact imported commit. Managed
  capture keeps selected file changes against that base after the runtime stops,
  regardless of changes to the local Git index, refs or configuration. See the
  [operator workflow](docs/local-node.md).
- Add a first-run guide with installation checkpoints and an illustrated walkthrough
  of the recorded two-task GitHub PR trial. Add community entry points for setup
  questions and pilot feedback.
- Publish an opt-in file-preparation comparison with Git worktree, a deterministic
  fixture and raw observations. The first small macOS sample favors Git worktree;
  it does not establish large-repository or end-to-end performance.
- Expose materialization counters in `node prepare-once` JSON: source reads/bytes,
  content written, copied local bytes, logical workspace bytes and filesystem entries.
  Reopened/recovered placements report `preparation_statistics: null`, rather than
  inventing historical measurements. See [node diagnostics](docs/local-node.md).
- Split the preparation probe into workspace request and node command timings.
  Record available materialization counters and support reproducible fixture sizes.
- Add a playable MP4 and a static storyboard to the recorded Connected trial.

## 0.1.12-alpha.1 — operator-assisted Connected recovery — 2026-11-02

- Enforce durable quarantine before any new GitHub connection. Discover all host
  branches/PR states, compare them with retained operations and preserve complete
  inventories through restartable, hashed-page uploads. Missing local records do
  not imply an empty remote history.
- Inspect exact retained PR, queue/removal, check and delivery identities through
  read-only App access. Keep operator decisions, immutable receipts and current
  context hashes; an old success is never fresh validation or merge permission.
- Acquire explicitly selected missing Git revisions, preserve independent source
  pins and verify native/Git mappings on the server. A repository-wide review
  checks every retained binding, current decision and source obligation together.
- Add bounded `restore-reconnect plan/probe/activate/status/cancel`. Admit one fresh
  connection only after operator-confirmed external fencing, fresh observations
  and atomic re-review. Old connections remain disabled and old writes are not
  replayed. Backups predating their first binding can register one permanently
  disabled discovery target before following the same recovery path.
- Record an [installed ARM64 trial](docs/evidence/connected-restore-live-20261102.json)
  of real backup/restore, live source discovery and retained/new proposal export.
  The receipt keeps its actual development source/archive identity; it is not
  evidence for host writes, managed agent execution or every later release binary.
- Add a [recovery and upgrade guide](docs/connected-recovery.md). No database schema
  or third-party dependency change from 0.1.11. The metadata component now uses the
  existing Rust Git projection component for source verification. Upgrade client/server together for restore
  context v2; old context hashes cannot authorize new work. Full publication/queue
  disaster certification, external fencing automation and production readiness
  remain open.
- Publish [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.12-alpha.1)
  after installed runtime/cold-restore acceptance, independent signature/source
  verification and a separate signed ARM64 installation/backup verification.

## 0.1.11-alpha.1 — local recovery continuation — 2026-11-01

- Add `backup restore-configure` after explicit quarantine and operator fencing.
  Recheck current metadata, copy objects into a separate live store and prepare
  private configuration for the exact restored database. New credentials and
  service startup remain explicit; Connected reconciliation remains unfinished.

- Add an offline `backup restore-quarantine` step after staging: drain old file
  workspaces, cancel pending native candidates, disable old nodes and revoke old
  GitHub connections while preserving source, history and recorded outcomes.
  Journal the transaction for safe retries; this does not activate the service.

- Fence generic background-job claims, renewals and acknowledgments while their
  repository is frozen or retired. Serialize them with repository state changes,
  retain pending jobs, and skip locked repositories during claims. This does not
  stop an already running worker or authorize recovered-deployment activation.

- Publish [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.11-alpha.1)
  after installed recovery acceptance and independent signature/source/asset checks.
  No schema or dependency change from 0.1.10. Full Connected disaster recovery,
  old-node recovery cleanup and unattended production operation remain unfinished.

## 0.1.10-alpha.1 — restore staging — 2026-10-30

- Add `backup restore-stage` for trusted cold backups into an isolated local
  PostgreSQL server. Create a new database and private copied inventory, verify
  restored identities/objects, and revoke old tokens with an append-only audit.
  Retain partial failures and the precise target identity; never overwrite an
  existing destination, create runnable configuration or authorize activation.
- Extend the existing PostgreSQL and installed native recovery drills to use
  the operator command, require denial of old credentials, and deliberately
  issue selected new credentials for inspection/replacement-node execution.
- No schema or dependency change from 0.1.9. Publish
  [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.10-alpha.1)
  after installed recovery acceptance and independent signature/subject checks.
  Full activation and Connected recovery remain open.

## 0.1.9-alpha.1 — local initialization — 2026-10-30

- Add `init` to create private service configuration, local object storage and
  operator-state directories from a protected PostgreSQL connection file. Existing
  destinations are refused; interrupted output is retained. Database migration,
  repository/credential setup and process startup remain explicit.
- Exercise generated configuration in the existing real PostgreSQL diagnostic
  test and installed managed/native workflows. Publish
  [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.9-alpha.1)
  after installed acceptance and independent signature/subject verification.
  No schema migration or new dependencies from 0.1.8.

## 0.1.8-alpha.1 — cold backup — 2026-10-30

- Add experimental operator `backup create` and `backup verify` for a stopped
  local PostgreSQL/object-store deployment and explicitly enumerated private
  state. Copies have an exact inventory and durable completion marker; existing
  output, active service/write locks and missing/corrupt live objects are refused.
  This does not implement safe restore activation or Connected reconciliation.
- Extend the installed native recovery drill to use the operator commands and
  preserve enumerated private journals/configuration with the dump and objects.
  Existing restore, corruption refusal and replacement-node checks remain.
- No metadata migration from 0.1.7. Publish
  [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.8-alpha.1)
  after installed backup/recovery acceptance and independent signature/subject
  verification. Full restore activation and Connected recovery remain incomplete.

## 0.1.7-alpha.1 — saved external task requests — 2026-10-30

- Add `client api-run-new` to save external task/workspace identities before
  creation and replay the same request after an uncertain response. Existing API,
  permissions and logical-close behavior remain unchanged; no execution is inferred.
- Exercise saved creation and replay after close in the installed external-task
  workload, alongside explicit API creation and MCP. No schema migration from 0.1.6.

- Publish [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.7-alpha.1)
  after installed task/runtime acceptance and independent signature/subject verification.

## 0.1.6-alpha.1 — MCP external tasks — 2026-10-30

- Expose external API task creation/status/close through three scoped MCP tools.
  Context registration does not execute code, attest isolation or grant validation;
  closing retains accepted changes and does not stop an external process.

- Exercise the packaged MCP adapter in installed Linux acceptance alongside the
  existing CLI and managed-runtime scenarios. No schema migration from 0.1.5,
  new permissions or managed process launch; full mode readiness remains open.

- Publish [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.6-alpha.1)
  after installed MCP/runtime acceptance and independent signature/subject verification.
  This does not establish compatibility with every MCP host or full mode readiness.

## 0.1.5-alpha.1 — workspace preparation and native queue helper — 2026-10-30

- Add `native-advance` to journal and resume preparation of one pending native
  candidate; checks, review and integration remain separate authorized actions.

- Add native `queue_pending` for bounded discovery of pending entries without
  saved entry IDs. Pages are repository-scoped live reads, not integration claims.

- Synchronize newly materialized files in bounded batches (up to 32 open files and
  four workers per active preparation). Every file still completes synchronization
  before the manifest and parent directories are synchronized and the workspace
  is published; write, thread creation or synchronization failures abort preparation.
  No object format, database schema or public API changes.

- Add an opt-in source-import benchmark with an actual CLI/service/PostgreSQL
  sample and explicit scope limits; no performance superiority claim.
- No schema migration from 0.1.4. Stop tasks/workers and the service before updating
  binaries together; retain source data, credentials and private retry journals.
  Native queue helpers remain experimental; Connected PR Preview scope is unchanged.

- Publish [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.5-alpha.1)
  after installed runtime CI and independent signature/subject verification.
  MCP external task tools in the later 0.1.6 release are not part of this tag.

## 0.1.4-alpha.1 — external agent API contexts — 2026-10-29

- Add `api-run-create/status/close`: pin an exact source and accept immutable
  proposals from a controller with its own execution environment. Explicit API
  delivery and external provenance do not assert runtime isolation or test success.
- Retain accepted changes after logical close; fence new writes and preserve
  idempotent receipts. Managed runtime preparation/capture remain separate.
- Add schema 032; stop services/workers, back up, install the new binaries and
  explicitly migrate with the new CLI before restarting together. See [the API workflow and upgrade contract](docs/external-api-runs.md).
- Publish [signed Linux x86-64/ARM64 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.4-alpha.1);
  installed API and managed/native runtime scenarios passed before signing.
  Independent verification matched source, subjects, invocation and metadata hashes.

## 0.1.3-alpha.1 — local service diagnostics — 2026-10-29

- Add `doctor --config` for redacted local configuration, path, database and schema
  diagnostics without implicit provisioning. See [scope and repairs](docs/doctor.md).
- Publish signed Linux x86-64/ARM64 packages tested through installed runtime CI.
  This does not certify full deployment health or a fresh live GitHub/LLM trial.

## 0.1.2-alpha.1 — resumable workspace requests — 2026-10-28

- Add `client workspace-new`: persist a generated workspace ID and request key
  before requesting preparation; reuse the state file after a lost response.
  See [workspace requests](docs/workspace-new.md). Existing permissions and
  explicit node/runtime preparation remain required.
- Signed Linux x86-64/ARM64 packages exercise the new command and receipt replay
  in installed managed-runtime CI. No new live GitHub trial is asserted.

## 0.1.1-alpha.1 — setup fixes — 2026-10-28

- Report an actionable, redacted error when the configured PostgreSQL schema
  cannot be resolved before migration or startup. Schema creation remains an
  explicit operator action; existing migration and durability checks still apply.
- Publish signed Linux x86-64/ARM64 packages and an [installed ARM64 setup report](docs/evidence/connected-preview-setup-20261028.json).
- Add one [Connected setup path](docs/connected-pr-preview.md) covering the installed
  binary, schema, Connected authority and minimal App/local permissions.

## 0.1.0-alpha.1 — Connected PR Preview — 2026-10-28

Signed Linux x86-64/ARM64 packages and installation commands are in
[the release](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.0-alpha.1). The installed ARM64 two-agent workflow published two
real draft PRs, retained publication identities on re-entry, cleaned both workspaces
and exported the retained changes to verified ordinary Git bundles. Temporary
service access was revoked. See [the scoped report](docs/evidence/connected-pr-preview-20261028.json).
The preview supports the documented Linux operator profile: exact-revision
workspaces, two isolated scripted tasks, immutable changes and proposed GitHub
PRs. GitHub handles checks, review and merge. The helper preserves operation IDs
and Rust publisher journals across uncertain replies; it does not provision a
host, issue credentials or operate a merge queue.

Standalone, full Connected publication disaster recovery, remote workers, S3,
VFS and structural merge remain outside this first preview. There is no production
support or measured performance advantage. Standard ELv2 has no workspace limit.

## Unreleased component history

- Versioned canonical objects, immutable directory trees and exact change preconditions.
- Local immutable object storage with digest checks and no-clobber publication.
- Bounded native history traversal and all best merge bases.
- Bounded native snapshot merge with exact sibling renames, structured conflicts and real-Git reference tests.
- PostgreSQL idempotent transactions, repository fencing and leased outbox.
- Exact validation-context binding and fail-closed evidence evaluation.
- Object catalog, durable upload receipts, persistent root pins and generation-safe retirement.
- Native ChangeSet intake with immutable versions, execution lineage, atomic pins and process-loss replay.
- Repository credentials, local Unix-socket service and CLI, immutable delivery profiles.
- Bounded native graph uploads with durable completion and restart/replay.
- Explicit native directory capture and resumable import from a private local pack.
- Authenticated workspace baseline listing and verified file download with lifecycle fences.
- Managed isolated agent execution with durable requests, resource limits and process-loss recovery.
- Resumable immutable ChangeSet API/CLI with server-computed trees and exact run/base lineage.
- Native policies, refs and serial integration queue with exact candidate checks/reviews, atomic publication and crash-safe CLI/API replay.
- GitHub App bindings and owner-requested exact source import, scoped read access, private Linux download and resumable full-history acceptance without publication permission.
- Experimental native GitHub queue delivery with explicit owner/bridge grants, exact-head GraphQL dispatch, retained legacy async-result recovery and separately authorized dequeue. Uncertain writes are not automatically retried.
- Retained queue membership, actual main and candidate parents, with scoped reads and exact signed-event correlation after process loss. Complete group/check/policy/final-tree verification remains pending.
- Exact candidate-check collection linked to retained membership, with separate push/merge-group suite identities, bounded read access and crash-safe replay. Attempt, workflow definition and final merge verification remain pending.
- Explicit selected workflow-attempt/job/check collection, with immutable source linkage and separate current-attempt comparison. Trusted definitions and final merge authorization remain pending.
- Owner workflow policies with immutable queue pins, generation-fenced dispatch, bounded executed root-file capture and crash-safe replay. A file match does not certify runtime context, host policy or final merge.
- Exact published-candidate identity/tree/parent and target-ancestry observations, bound to retained membership with crash-safe replay. CI context and complete merge admission remain separate gates.
- Owner-approved managed checks of exact imported GitHub queue candidates, with shared trusted runtime evidence, revocation fences and retained receipts. Required App-check publication and full live Connected acceptance remain pending.
- Managed checks of published PR heads before queue admission, sharing the same runtime and scoped lifecycle. PR evidence is separate from group evidence; populated group records survive the additive schema upgrade.
- Execution-derived required App-check publication with explicit owner/bridge permissions, one-shot dispatch, immutable reports and CLI recovery after lost replies. An uncertain write is not retried; full live Connected admission remains a release gate.
- Contract tests and product documentation.

Earlier entries describe component increments, not completed mode releases.
