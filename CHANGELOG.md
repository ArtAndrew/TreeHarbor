# Changelog

## Unreleased

## 0.1.5-alpha.1 — workspace preparation and native queue helper (candidate)

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
