# Implementation status

Updated: 2026-10-23. This file describes implemented behavior and remaining release
gates. A foundation milestone does not complete either mode.

New [accepted-version GitHub projection](github-projection.md): an owner selects
the exact current accepted ChangeSet version and binding. The service derives one
immutable commit from the retained base/operations/result and acceptance time,
reserves and syncs it, then commits a full-graph pin and replayable ready receipt.
Version, binding, authority, permission and deadline fences apply after IO and
after receipt/outbox writes. The assigned publisher exports through a separate
`host.publish` purpose; `host.read` tokens do not gain publication access. This
does not register a base, move a ref or approve any checks/merge.

Local verification: 187 ordinary tests and 200 explicit PostgreSQL/materializer/
node/service tests passed, plus the separate public HTTPS connection test.
Five focused projection tests passed again after final HTTP/error assertions.
Coverage includes concurrent completion, cancellation/version/access changes after
disk IO, expiry inside receipt/outbox writes, full-graph retention, populated
schema-16 upgrade and actual CLI/server kills before and after commit. Exported
Git history passes strict fsck with the exact original parent and accepted content.
These fixtures do not prove a real installed App or PR/queue workflow.

Schema 17 preserves existing imports/bindings/credentials without added grants.
Actual GitHub branch/PR creation, check/queue reconciliation and installed-App
acceptance remain release gates, as do the full two-mode operational requirements.

New [exact GitHub source import](github-import.md): an owner fixes the Git commit,
binding, epoch, deadline and budgets. Only the assigned Bridge's `host.read` may
upload that request; Human source routes remain separate. Schema 16 preserves
existing bindings/observations/credentials and partially uploaded Human sources.
Completion checks the native root's GitOrigin and full graph membership, pins the
source base and commits a replayable receipt/outbox without writing main or granting
validation. Current access, cancellation, binding and expiry fences apply after IO
and after transactional writes.

The Rust CLI uses fresh one-repository App access, private full-history fetch and
native conversion, a durable intent and resumable object upload. The Linux Git
download supervisor bounds time/per-process resources, detects parent loss and
refuses redirects; hard aggregate quotas and arbitrary process teardown still
require an external runtime. Failed partial stores cannot masquerade as completed
bundles. Network download currently supports GitHub.com/SHA-1/Linux only.

Local verification: 185 ordinary tests and 194 explicit PostgreSQL/materializer/
node/service tests passed. This includes expiry inside request/begin/finish writes,
populated schema-15 upgrade, wrong Git identity, revocation between disk write and
publication, concurrent completion and actual CLI/server restart followed by exact
Git export. Strict all-target Clippy and formatting passed. Linux smart-HTTP,
redirect/timeout/parent-loss fixtures run separately in required CI; local macOS
tests do not prove those Linux behaviors. A real installed-App import and the full
PR/check/merge-queue workflow remain release gates.

New [durable GitHub bindings](github-connection.md): Human repository owners
configure/revoke immutable Connected bindings; only the assigned Bridge with
`host.read` can register a bounded connection attempt and its immutable observation.
This permission is separate from publication and cannot be granted to agents.
Schema 15 preserves existing tokens/grants without expanding their permissions.
Current scope, assignment, authority epoch, revocation and post-write expiry are
checked. CLI retries retrieve completed observations without the App key or a new
network probe. Historical receipts do not reactivate revoked bindings or confer
publication readiness. PostgreSQL stores identifiers/observations, never App keys
or temporary GitHub tokens. The server trusts its assigned bridge reporter.

Local verification: 183 ordinary tests and 187 explicit PostgreSQL/materializer/
node/service tests passed. New cases cover concurrent binding/report requests,
post-write expiry, immutable receipts, cross-tenant/role/epoch/revocation fences,
unchanged existing credentials after upgrade, and actual CLI/server restart.
The CLI rejects substituted App pins before network access and reads completed
observations with the App configuration removed. Those observations are trusted
fixtures; they do not establish an installed-App or complete Connected pass.

The earlier [GitHub App connection probe](github-connection.md): an explicit trusted
operator command checks App/installation/repository/owner identity, requests and
verifies one-repository read access, independently enumerates token scope, reads
repository metadata and revokes the temporary token. HTTPS/API version, response
bounds, deadlines, redaction and no-redirect/no-retry behavior are explicit.
Queue ownership eligibility is reported separately from unverified configuration.
That probe alone does not bind a repository. Source import is a separate command;
PR publication and queue admission remain unimplemented.

Local verification: 182 ordinary workspace tests and 178 explicit PostgreSQL/
materializer/node/service tests passed; strict all-target Clippy and formatting
passed. New HTTP-client fixtures include identity substitution, scope escalation,
expiry/suspension, lost token response, bounded slow/large responses and revocation
failure. OpenSSL independently verifies a fixture JWT signature. A separate real
public GitHub HTTPS test passed with API version 2026-03-10 and no credentials.
These are not installed-App/real-host integration evidence: authenticated App
setup, authenticated real-host import, PR/check/queue reconciliation and confirmed merge
remain release gates. Existing native runtime checks remain required in CI.

Explicit [workspace cleanup](workspace-cleanup.md) now joins schema-14 release
metadata to a guarded physical node operation, owner status and `node cleanup-once`.
A durable intent precedes unlink; a separate completion record precedes atomic
source-pin/slot release. The node checks the existing runtime journal and current
Engine, retains the registered placement inode, and resumes after partial deletion.
Completed receipt reads avoid the node/slot locks, including during successor work.
The assigned validator can explicitly release a collected native check's workspace;
immutable evidence remains available and final publication still rechecks context.

Local tests cover real file deletion, permissions, symlink/hardlink containment,
worker SIGKILL during unlink, after local completion and after metadata commit,
slot reuse, CLI/HTTP ownership and exact replay. Database tests cover current
fences, concurrent claims, expiry during writes, populated schema-13 upgrade and
retention of accepted ChangeSet graphs. These are distinct from real Linux/runtime
evidence. The required gVisor harness also passed removed-runtime proof, worker-loss,
active successor replay, check-file release before publication, and source/Git
export after agent-file deletion in
[CI 39932578201](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39932578201).
A missing real runtime marker blocked deletion despite a database removal record.
These observations apply to the pinned Linux/runtime profile, not arbitrary
unmanaged code or host replacement.
Local verification: 169 ordinary tests and 178 explicit PostgreSQL/materializer/
node/service integration tests passed (four concurrent integration cases), including
actual CLI drain/delete/replay and collected-check release. Formatting and strict
all-target Clippy passed. CI independently passed 169 tests on each Linux/macOS
runner, 178 PostgreSQL integration tests and the actual runtime harness. The five
required checks remain mandatory for subsequent heads.
Automatic cleanup scheduling, abandoned-journal/artifact retention, node replacement
and full release gates remain open. This increment is not a product release.

Experimental [managed seals](managed-seals.md) now connect filesystem capture to
owner acceptance. A [selected-file capture library](capture-format-v1.md)
now creates an independently verifiable immutable artifact from an exact baseline,
with explicit replacements/deletions/exclusions and sparse coverage checks. This
library does not prove runtime quiescence. Schema 13 now adds durable owner requests,
stop/cancel, fixed deadlines and fenced node claims, with PostgreSQL tests. A node
adapter rechecks removed runtime identity and persists a separate completion record.
Scoped server publication binds an immutable declaration, proves baseline
membership from the exact original snapshot and commits ready pins/receipts
atomically. The node adapter connects capture to publication and supports exact
completed-response replay. [Owner HTTP/CLI commands](managed-seals.md) now request,
inspect, cancel and accept seals. Acceptance computes the exact sealed operations
through the shared native engine, preserves original run/base lineage, enforces
version CAS and atomically retains the result without carrying over checks.
Automatic workspace cleanup, the wider fault matrix and full release gates remain open.

Local capture/acceptance verification: 164 ordinary tests passed; 164 explicit
PostgreSQL/materializer/node/service tests passed with four parallel test cases.
Seven new publication tests cover scoped baseline proofs, child-first uploads,
restart/reclaim, exact replay, quotas, GC retention, current access fences and
expiry inside finalization. These database tests use trusted runtime fixtures.
Four additional acceptance tests cover version CAS, borrowed reservations, current
access/epoch/drain fences, GC retention and expiry inside final commit. An actual
CLI/server test kills the server before object publication and after commit,
recovers the exact accepted binary/directory result and rejects forged identities
or result bytes. Independent reservation vectors and strict wire tests are included.
These local tests do not prove the real-Linux capture adapter. The required
gVisor harness separately exercises two real writing agents, capture/acceptance, worker
loss after artifact durability, markerless recovery, active-writer stop, exact
checks/review/publication and restoration into ordinary Git. This path passed in
[CI 39912060970](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39912060970),
together with Linux/macOS Rust checks and PostgreSQL contracts. It remains required
on subsequent changes. This evidence covers that pinned Linux/runtime profile,
not arbitrary unmanaged writers, other runtimes or a complete mode release.

New review increment: [immutable review diffs](review-diff.md) show an exact
accepted ChangeSet version, prepared integration candidate or two registered
native revisions through read-only HTTP/CLI. Candidate diffs compare the captured
target head against the prepared merged result and expose immutable target,
policy, proposal and run context. Retained candidates remain inspectable after
supersession, cancellation or publication, subject to current repository/access
fences. Reading never approves a candidate or makes old checks current.
Byte paths, empty directories and bounded text/page behavior remain unchanged.

Ordinary [proposal version updates](change-proposals.md) now atomically make that
ChangeSet's older pending queue entries/candidates stale, matching rebase behavior.
Published history and old evidence remain retained. New candidates require fresh
checks/review; failed updates and completed-request replays cannot repeat mutations.

Local verification for this candidate-review increment: 148 ordinary workspace
tests and 144 explicit PostgreSQL/materializer/node/service tests passed, along
with formatting, strict Clippy and independent fixture reproduction. The required
Linux gVisor job also tests candidate inspection against checked files and ordinary
version replacement during a running check. Local macOS tests do not prove that
runtime behavior. The complete release gates below remain open.

New rebase increment: [immutable ChangeSet rebase](change-rebase.md) adds owner
start/status/cancel and durable server preparation of one accepted version on an
exact native base. Schema 12 retains original run/version lineage and pins, and
atomically makes pending older candidates stale. This creates a single-parent
revision without moving refs or copying validation. General history stack rewriting
remains open; this does not complete Standalone.

New history increment: [scoped native history](native-history.md) now exposes
commit display, deterministic all-parent or first-parent log pages, ancestry and
all best merge bases through actual HTTP/CLI. Queries read only verified commits
from registered repository roots, with current authority/permission fences and
GC holds. They neither move refs nor implement rebase.

New source-history increment: [live source export](live-source-export.md) downloads
a registered native base and its complete reachable graph through scoped reads.
The CLI verifies a bounded binary archive and atomically publishes a full native
pack v3, usable for reimport and optional Git export. Actual CLI tests restart the
server, remove original source packs and recover both imported Git origins and
newly published two-parent native commits. Full server backup remains unfinished.

New interop increment: [Git bundle import](git-import.md) converts an exact Git
commit's complete parent/tree graph into a durable full-source pack v2. Import
preserves original commit IDs/bytes, verifies first-parent deltas, and feeds the
existing scoped resumable source upload and native workspace preparation. No
network credential, main-ref update, authority cutover or CI approval is implied.
The local native path works with Git absent after optional import/export.

New increment: [managed native checks](managed-checks.md) bind approved offline
commands to exact candidate files and derive validation from removed, bounded
runtime observations. Schema 11 persists programs, executions and immutable
evidence. Generic agent exits and manual reports cannot satisfy registered programs.
The node verifies pristine inputs before first start; recovery uses the existing
runtime and preserves build outputs. A required Linux scenario now exercises this
path in `scripts/test-native-checks.py`; local database tests cover replay, current
assignment/credential fences, missing-exit errors and populated schema-10 upgrade.
Local verification: 104 ordinary workspace tests and 117 real PostgreSQL/transport
tests passed, with fmt and strict Clippy. The first real-runtime pass is
[CI 39728681125](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39728681125).
The required job retains the added server-death/collection replay and node-revocation
checks on every subsequent head. Automatic scheduling, logs/artifacts, retention
and release gates remain open.

New admission increment: [native integration](native-integration.md) adds durable
policies/refs, a serial queue, immutable proposal/integration commits, scoped check
runs and human reviews, exact final admission and atomic publication. Schema10
preserves accepted schema9 data. Actual CLI/server restart tests merge compatible
same-file changes and report a conflicting third proposal without Git on PATH.
Validation outcomes in those transport tests are trusted fixtures; the managed
check path above has a separate real-runtime harness. Automatic scheduling and a
complete Standalone release remain open.

The [native snapshot merge](native-merge.md) combines exact
base/current/proposed trees, preserves original inputs and reports structured
conflicts. Real-Git reference fixtures and a no-Git local-storage test pass locally.
The library has explicit conservative limits; its output now feeds native admission.

New increment: [immutable change proposals](change-proposals.md) expose owner-scoped
resumable blobs and server-computed result trees through HTTP/CLI. Schema9 adds
purpose-specific upload state; accepted proposals preserve original run/base lineage.
Local PostgreSQL/transport tests passed, including server process loss and exact
replay. The required Linux harness also submits overlapping real managed-agent output.
Managed filesystem sealing now has the separate acceptance path above. Managed
offline checks are described above.

Development increment: [managed AgentRun](managed-runs.md) requests, owner-bound
status/cancellation, immutable runtime policy/placement records and terminal receipt
replay now have real PostgreSQL tests. Schema 8 preserves existing runs and reserved
preparations. The actual service/CLI-to-gVisor path passed in
[CI 39641169748](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39641169748):
native import, bounded preparation, simultaneous isolated agents, cancellation/drain,
resource outcomes and worker-loss recovery. No complete authority mode is implied.

| Component | Evidence | Remaining |
|---|---|---|
| GitHub App connection/import/projection | Scoped read token, durable bindings and exact owner requests, isolated download, resumable import, accepted-version projection/export, exact-branch transport, durable publication intents and trusted publication/recovery CLI | Installed-App evidence, incremental reuse, webhooks, PR/check/merge queue and real-host confirmation |
| Canonical SHA-256 object format | Versioned framing, independent golden vectors, strict decoder | Compatibility policy for later formats |
| Immutable directory trees and operations | Exact expected-before checks, canonical subtree operations v2, bounded structural diff, structural sharing, explicit deletes; native merge and Git reference matrix | General history rebase, broader rename/base support, live-server Git interop |
| Local object store | Atomic no-clobber writes, digest verification, file/directory fsync, tenant directories, physical generations and upload receipts | S3, backup/restore, kill fault campaign |
| Native DAG and admission | Scoped history/log/ancestry/merge-base HTTP/CLI, persisted refs/policies/queue/checks/reviews, verified candidate commits, final transactional admission and no-Git CLI/process-loss tests | General history stack rebase, automatic scheduling, full lifecycle and recovery |
| Review diff | Read-only exact ChangeSet/candidate/revision HTTP/CLI, retained integration/policy/version/run identity, scoped GC holds, bounded byte paths and text, actual Git path oracle and restart/page/error tests | Review UI, persistent diff indexing and larger-repository profiling |
| PostgreSQL metadata and outbox | Real database transaction/idempotency/lease tests; repository authority/epoch checks; additive schema 18 publication ledger; local service pool and crash/replay | Remote service integration and full server restore |
| Native ChangeSet intake | HTTP/CLI immutable proposals and separate ready-seal acceptance, scoped blobs, computed versions, original run lineage, atomic pins/outbox and process-loss replay | GitHub base synchronization, remote transport and retention |
| Protected incremental source reads | Pinned sessions, verified frontiers, bounded prefix batches and descriptors surviving local GC; authenticated workspace-root sessions | Per-tenant service quotas, bulk node delivery and S3 |
| Native file materialization | Private copies from exact snapshots, full/sparse selection, staged expansion preserving edits/deletions, bounded IO and filesystem checks; guarded node publication and selected capture after runtime removal | Cache/reflink, wider capture fault matrix and Git-compatible format |
| Object catalog and retention | Receipt-bound publication, typed edges, atomic persistent pins, read holds, generation-safe retirement/retry; independent reachability model | Automatic GC/reconciliation/dry-run, abandoned-upload cleanup, S3 and full restore |
| Exact validation and native publication | Context binding, immutable scoped receipts, current check runs, reviewer policy, final target/policy/revocation checks and atomic ref/audit/queue/pin commit | CI artifacts/logs, automatic check scheduling, evidence retention policy, review UI |
| Credentials and agent commands | Repository-scoped opaque tokens, transactional revocation, distinct actor classes, owner-bound reads and strict command dispatch | Remote transport, MCP, production identity/admission integration |
| Local HTTP service and CLI | Actual Unix-socket server/client, trusted admin and file-preparation node CLI, private credentials, bounded HTTP/pool, strict startup schema, SIGKILL replay, cancellation/rollback, reconnect and graceful shutdown tests | TLS/remote access, packaged installation |
| Workspace baseline API | Owner/generation/epoch-bound directory pages and verified blob download, repeated authorization, final revocation/drain rejection, bounded response ownership, actual CLI/HTTP tests | Bulk delivery to nodes, remote streaming, persistent cache and runtime binding |
| Source graph uploads | Authenticated scoped sessions, child membership, fixed deadlines/budgets, backend identity, binary HTTP/CLI, concurrent retry, restart and schema-v5 upgrade tests | Incremental reuse, streaming/batching and automatic cleanup |
| Native directory import | Explicit selection, descriptor-relative capture, durable pack/journal v1, independent vector, actual CLI-to-server partial-upload recovery and completed replay with Git absent from PATH | Incremental reuse, automatic spool cleanup and runtime-proven snapshots |
| Source delivery profiles | Canonical full/sparse format with independent vectors, immutable scoped definitions, workspace foreign-key binding, v4 upgrade, authenticated CLI/HTTP and exact node use | Profile retention and generation-aware expansion |
| Workspaces and runtime | Owner-bound requests, exact source/runtime bindings, trusted actual observations, drain and terminal replay; real CLI/PostgreSQL/gVisor recovery tests; selected capture/accept API | Automatic resource release, scheduling, wider fault matrix and host replacement |
| Runtime task supervisor | Linux namespace init, privilege guards, environment clearing, guest process limit, child reaping and deadline; real gVisor CI harness | Automatic workspace cleanup and production runtime provisioning |
| Local runtime controller | Versioned immutable profiles, restricted Engine API, bounded ext4 identity, exclusive durable volume assignment and create/start/remove journal; real-runtime fault harness and managed-run adapter | Provisioning, host/engine replacement recovery and cleanup automation |
| Workspace preparation reservations | Operator-registered node storage, scoped job claims, immutable source/slot/attempt identity, renewal/reclaim, exact file-ready observation and drain retention; real PostgreSQL race/upgrade tests | Cleanup scheduling after runtime removal |
| Workspace cleanup | Exact placement/run, current node claims, durable deletion journal, descriptor-relative physical removal, atomic source-pin/slot release, owner status, node CLI and collected-check release; local crash/reuse/expiry/GC tests | Broader runtime/fault matrix, automatic scheduling, journal retention and node replacement |
| Local preparation node | Private physical root identity/OS lock, durable placement intent, exact no-replace publication, independent concurrent slots, current authorization and file-ready acknowledgment; actual process-kill recovery and import-to-files CLI tests | Scheduler/daemon, automated volume provisioning, remote workers, fenced volume migration and cleanup scheduling |
| MCP adapter | Planned; not implemented | Shared-service adapter, scoped tools and real client interoperability tests |
| Git source export | Local native-pack CLI and bounded Rust full-DAG projection to self-contained Git SHA-1/SHA-256 bundle; isolated batch Git, exact mapping, verified original Git metadata/IDs, restored fsck and no-replace publication | Backup metadata, incremental projection, GitHub delivery |
| Live source export | Scoped registered-base reads with GC holds and authority/credential fences; streaming archive v1 and atomically verified pack v3; actual CLI restart/merge/origin roundtrip | Other retained proposals, all-ref export, full metadata backup, incremental transfer, longer jobs and cleanup |
| Git history import | Pinned self-contained SHA-1/SHA-256 bundle, all-parent native graph, replayed deltas, exact original metadata, atomic full-source pack v2 and resumable HTTP/CLI upload; separate scoped Linux/SHA-1 GitHub download | Real installed-App gate, incremental imports, multi-ref cutover, automated cleanup, larger-repository profiling |
| Connected | Partial: binding, read-only App access, exact source import, retained native base and exact accepted-version projection/export | Real installed-App acceptance, GitHub PR/checks/merge queue and publication reconciliation |
| Standalone S2 | Partial: durable native admission/publication, scoped history/candidate diff and single-ChangeSet rebase through CLI/API without Git, managed offline checks, registered-source export, plus explicitly separate transport fixtures | General history stack rewriting, full lifecycle, cutover, full server backup/restore |
| Release and benchmarks | Not released or measured | Clean installation, artifacts, SBOM/provenance, comparison and operating cost |

## Local verification

macOS arm64, Rust 1.99.1, 2026-10-21:

```text
cargo test --workspace --locked --offline
100 tests passed without external services, including independent policy/reservation vectors, candidate parent/delta verification and the real-Git merge oracle.
113 PostgreSQL integration tests passed explicitly against local PostgreSQL 18.6: transactions 7, catalog 12, intake/admission 20, protected reads 9, materializer 10, access/commands 10, profiles/upgrade 2, uploads/upgrade 7, preparation/run ownership/upgrade 17, node recovery 7, service process/transport 12.
The native admission suite covers missing/stale/ambiguous/revoked evidence, separate roles/owners, concurrent publication, cancellation, repository boundaries, populated schema9 upgrade and retained graph roots. The CLI test kills the actual server before object catalog publication and after candidate commit, then retries the same request. It independently reads persisted merged content and both-parent ancestry.
Both database URL variables are required for the explicit ignored integration suites.
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
Passed.
cargo fmt --all
Applied before the checks above.
Earlier published CI evidence (Linux, macOS, PostgreSQL and gVisor; commit c086d3a):
https://github.com/ArtAndrew/TreeHarbor/actions/runs/39624242714
The node increment adds its real database suite to the required PostgreSQL CI job.
```

The separate `gVisor task supervision` CI job exercises the
[runtime supervisor](runtime-supervisor.md) on a real disposable Linux runtime.
It is not run by the ordinary local test command, and the macOS checks do not
provide sandbox evidence. The harness includes namespace teardown, a launcher
which exits before completion, guest PID limits, memory exhaustion and a dedicated
persistent disk capacity. These supervisor-only cases do not produce READY or
Validation receipts; the separate managed-run harness covers the service boundary.

The [local runtime controller](runtime-controller.md) now has a separate real-engine
fault harness in the same required job. It delays actual Engine traffic and kills
the Rust controller around create/start/remove, checks independent task volumes,
retains uncertain starts and refuses configuration drift. Local tests verify its
canonical profile/intent vectors, journal exclusion and invalid recovery records.
Linux runtime evidence comes from the required CI job, not macOS tests. The additional
managed-run harness uses actual owner CLI requests, native import and volume-backed
preparation. It covers independent same-path writers, owner-bound results, nonzero/OOM/
deadline/disk outcomes, cancellation/drain and real worker death around Engine RPCs.
Permission changes on the task directory cannot disable runtime cleanup. Managed
source sealing is described above; workspace deletion remains unfinished. Task
results cannot grant trusted Validation.

The real runtime run above passed isolation, rejected invalid bootstrap settings,
primary exit and descendant teardown, PID-1 signal/proc attacks, timeout after the
launcher exits, and external cancellation. Exhaustion at task budgets 4/16/64
observed exactly 4/16/64 live tasks including the primary, repeated after reaping;
attempts to raise the hard limit failed. Actual memory exhaustion stopped the
sandbox with an OOM observation. The 128 MiB workspace filled without changing
the separate neighboring volume. These are correctness observations for the
pinned test profile, not throughput measurements or complete node integration.

Test stores used for pure model tests are not a server implementation. Local
storage tests use actual files; they have not yet proved power-loss behavior on
every filesystem. No benchmark timing or customer adoption is claimed.

The catalog tests use real files and PostgreSQL, including simultaneous pin/GC,
duplicate hold tokens, deletion retries, a crash boundary between deletion and
database completion, and published-v1 schema upgrade. Native intake now commits a
ChangeSet version, lineage, graph pins, response and outbox atomically, and recovers
the same receipt after a worker process is killed. ChangeSet submission is now
exposed through local HTTP/CLI with explicit immutable blobs and operations.
Remote transport, automatic workspace cleanup, full application
failure injection, fenced restore and automatic maintenance remain release gates.

Protected local reads now resolve only requested object frontiers and retain opened
file descriptors across GC without holding a database transaction during content
verification. Tests include expired sessions, complete physical collection, generation
reuse, corrupt bytes, resource limits and selective reads without loading commit
parents. Authenticated workspace baseline reads now expose directory pages and verified file
bytes through the local CLI/HTTP service. Bulk node delivery and scheduling remain
absent; runtime isolation is provided by the separate managed-run boundary. See [source reads](source-reads.md) for the wire format,
read limits and the precise revocation boundary.

Native filesystem preparation now consumes these reads and returns a private tree
with a sibling baseline manifest. Sparse expansion copies a previously stopped
tree into a new candidate, preserves edits/deletions/untracked files and adds source
paths without modifying the old tree. Limits, symlink/case behavior and cancellation
are tested using actual files and PostgreSQL. See [manifest v1](workspace-manifest-v1.md).
The bounded baseline loader verifies an independently recorded exact manifest hash,
scope, snapshot and canonical profile, rejects unsafe private-file provenance and
malformed metadata, and preserves current agent files. Tests cover a new process,
the independent v1 bytes/hash and expansion of a reopened catalog-backed tree with
local edits/deletions/untracked files. Historical preparation counters are explicitly
unavailable after loading. This is a baseline recovery primitive, not a node daemon
or proof of execution readiness.
The caller must still prove runtime
quiescence, fence/persist the new generation and bind the result before READY;
none of those steps is implied by a materializer receipt.

[Preparation reservations](workspace-preparations.md) now persist the source,
node and slot identity before physical work, with scoped claims, current lease/
authorization checks and immutable completion observations. This new path records
file readiness with an empty runtime binding; it cannot start an agent through
the legacy running callback. Real PostgreSQL tests cover concurrent retry,
restart/reclaim, expiry during insertion, cross-node/tenant denial, revocation,
drain retention and populated-v6 upgrade. The [local node](local-node.md) now binds
these reservations to a private physical storage identity and exact local intent.
It publishes a synced tree with a placement-bound baseline receipt, then records
file readiness in PostgreSQL. Actual worker kills before publication, after
publication and after acknowledgment recover the same attempt/path; published
files retain their inode and local edits/deletions/untracked files survive reopen.
Concurrent workspaces, queued-IO cancellation, real lease renewal and renewal
blocked behind the completion transaction are exercised. The CLI/HTTP test runs
native import through file preparation and new-process reopening with Git absent.
That file-only test launches no runtime. The separate managed-run harness exercises
bounded volumes and actual runtime execution. Automatic scheduling, volume provisioning
and safe published-tree removal remain unimplemented.

Repository credentials and the [agent command library](agent-commands.md) now check
permission, principal kind, token expiry/revocation and current grants in the
transaction that accepts a write, including receipt replay. Tests exercise
revocation racing with a blocked write, expiry after workspace effects and before
commit, token rotation, scope/identity injection, owner-only reads, node observations,
authenticated ChangeSet acceptance and a populated v3-to-v4 upgrade. Role checks
do not prove that a sandbox ran or that a validation result is true. The local
HTTP/CLI adapter now uses this boundary and provides private credential-file
management plus trusted database administration. Real process tests cover a lost
mutation response followed by server kill/restart and exact receipt replay,
duplicate-server exclusion, stale sockets, revocation, malformed/chunked HTTP,
connection/header deadlines, schema mismatch, pool reconnection and transaction
rollback before pool reuse. See the [local guide](local-service.md).
MCP, remote TLS transport, per-tenant service admission and complete
product workflows remain absent. A Unix socket and private files do not isolate
untrusted code running as the server's OS user.

## Release gate ledger

- [x] Initial core, local store and native history contract tests.
- [ ] PostgreSQL acknowledged-change transactions and replay after process loss.
- [ ] S3 conditional immutable writes and catalog/GC race tests.
- [ ] API authorization, tenant separation, trust boundaries and resource limits.
- [ ] Workspaces, sparse expansion preserving edits, private Git state and drain.
- [ ] Two-agent overlap demo, conflict resolution, rebasing and exact validations.
- [ ] Connected path against real GitHub; required checks cannot be bypassed.
- [ ] Standalone native path with Git absent and host access disabled.
- [ ] Fenced migration between authority modes with one writer.
- [ ] Full-DAG GC, backup/restore and self-contained Git export drills.
- [ ] Contribution-rights process, third-party notices and required GitHub checks.
- [ ] Clean install/update/uninstall; checksummed release artifacts and provenance.
- [ ] Raw tuned-Git and analogue benchmark data, cold/warm and full costs.
- [ ] Pilot onboarding evidence and ten-team recruitment material.

Do not mark unchecked gates complete based on a mock, design paragraph or planned CI job.


## Native Git export increment — 2026-10-22

Added the optional Rust `git-bridge` and `treeharbor git-export --pack ...`.
The library projects complete native DAGs to self-contained SHA-1/SHA-256 bundles;
the CLI consumes completed local source packs. Seven bridge tests and one actual
CLI test cover independent roots, ordered multiple parents, exact binary/mode/link
content, raw path quoting, shared directories, a separately calculated Git object
vector, restored strict fsck, resource failures, killed process groups, concurrent
no-clobber publication and inherited hostile Git configuration. The exporter uses
one generated Git pack (formerly fast-import), without checkout or a process per file.

Local macOS arm64, Rust 1.99.1, Git 2.54.0 (Apple Git-157):
`cargo test --workspace --locked --offline` passed 112 ordinary tests;
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` passed.
Public format/limits/operations are in [git-export.md](git-export.md). This did
not include automated Git import or leased live-server export (both added later),
GitHub delivery, authority migration or full product backup. No performance
superiority, production readiness or complete mode release is claimed.


## Subtree operations increment — 2026-10-22

Canonical ChangeOperations v2 adds typed Directory before/after edges while all
leaf-only and existing object frames retain v1 bytes/IDs. The core computes a
bounded structural snapshot diff and applies file/directory transitions with all
predicates checked before writes. The candidate builder replays the new delta;
its merge profile is explicitly v2. Source graph upload and Git export accept
mixed-version graphs. Public leaf-only proposal HTTP v1 is unchanged.

Independent Python frame/hash vectors, a 49-pair exact-tree replay matrix,
stale-subtree and overlap rejection, kind/version failures, all budget boundaries,
unchanged-subtree reuse, candidate parent/delta checks and real Git mixed-history
restoration are covered. Real PostgreSQL tests retain both directory graphs during
GC, enforce upload-session membership and replay after reconnect; an old pending
merge-profile reservation must be cancelled/requeued rather than recomputed.
See [format and upgrade boundaries](subtree-operations-v2.md). Git bundle import
was added in the later increment below; agent-facing subtree uploads remain open.


Local verification for this increment (macOS arm64, Rust 1.99.1): 119 ordinary
workspace tests passed. The full PostgreSQL/transport suite passed 119 tests,
then the new old-profile upgrade test passed separately (120 total); the extended
actual CLI/HTTP crash/replay test also passed with a server-derived v2 directory
addition and durable full-graph reads. Strict workspace Clippy and fmt passed.
No runtime outcome is fabricated by the new tests; the existing transport fixture
is explicitly separate from actual task/check execution in required gVisor CI.

## Original Git metadata and exact projection

NativeCommit kind5/v2 adds an optional original Git OID and a typed reference to
the raw commit Blob. Existing v1 IDs remain unchanged; subtree kind4/v2 remains
compatible. Graph pins and read holds retain the raw bytes. Scoped uploads require
the raw child in the same session; they do not authenticate the claimed Git identity.

Export independently verifies original hash, full tree, ordered projected parents,
author/date/message and strict Git structure before publishing a bundle. It preserves
committer, signature and extension header bytes; signature validity is not checked.
New native descendants receive new IDs. A single bounded non-delta pack replaces
fast-import to preserve arbitrary headers. Linux/macOS tests compare actual Git
objects and restored histories for SHA-1/SHA-256; independent Python native vectors
and PostgreSQL retention/upload tests cover the format and reachability.

See [format and upgrade](git-origin-v2.md) and [export](git-export.md). This is the
representation/export prerequisite used by the Git bundle importer added below.
No Connected release, GitHub delivery, migration, performance result or complete
backup/restore is implied. Old readers must be upgraded before kind5/v2 writes.

Local verification (macOS arm64, Rust 1.99.1, Git 2.54.0): the full ordinary suite
passed 122 tests, followed by passing the expanded bridge and CLI suites with two
additional tests (124 total). All 122 real PostgreSQL/transport tests passed.
Strict workspace Clippy and formatting checks passed. Independent native vectors
regenerated identically. Actual runtime isolation remains covered by required
Linux gVisor CI, not by a local macOS container claim.


## Complete pinned Git history import

The isolated bridge now imports self-contained bundles without checkout or a
Git process per file. It verifies exact Git IDs, preserves every reachable
ordered parent and raw original commit, and replays each native delta against
the first parent. Import remains an optional interop boundary; it does not
publish native refs or authenticate claimed authors/reviews.

Full-source pack manifest v2 has an independent Python vector. Its whole output
is private, validated and synced before no-replace publication. Selected-directory
pack v1 bytes and fingerprints remain unchanged. Existing scoped source upload
journals accept either pack and retain their immutable retry identity.

Actual SHA-1/SHA-256 Git fixtures cover independent roots, merges, byte names,
file/directory transitions, binaries, executable bits and links; export after
input deletion restores every original commit ID and raw payload. Corrupt,
incremental, absent-tip, unsupported and budget inputs fail. Concurrent output
publication has exactly one winner. Actual CLI/PostgreSQL tests exercise partial
upload plus server loss, completed-receipt replay, source deletion and native
node preparation/reopen with no Git on PATH.

See [import format, limits and operations](git-import.md). Hard resource isolation,
GitHub delivery, network fetch, multi-ref migration and full backup/restore remain
open. Selected-object bounds do not limit Git's earlier unpacking of unrelated
bundle objects; external disk/memory/CPU quotas remain required.

Local verification (macOS arm64, Rust 1.99.1, Git 2.54.0): full ordinary suite
passed 129 tests; the final targeted suite passed with one additional branch-
selection case (130 total). All 124 real PostgreSQL/transport tests passed,
including both new full-history CLI scenarios. Workspace Clippy with warnings
as errors, formatting and unchanged v1 golden vectors passed. Actual Linux
runtime execution remains a separate required gVisor CI gate.

## Scoped live source export

The service now exports one registered native revision and its complete source
closure through authenticated, leased reads. Authorization/authority changes
reject subsequent batches and final publication. A source read hold prevents
GC retirement during construction; anonymous server scratch outlives neither its
request owner nor process. The client downloads to disk, verifies archive v1,
builds full native pack v3 and syncs a no-replace output. v1/v2 packs remain readable.
There is no Git dependency in this download path or PostgreSQL schema change.

Actual service/CLI tests kill and restart the server, remove original source
packs, export newly published two-parent commits, compare every native frame,
restore with real Git and reimport the downloaded pack. A separate imported-Git
case preserves raw original commit metadata. Database tests cover scoped membership,
wrong bound permission, epoch changes, revoked tokens, expired holds and GC
retention. Independent archive/manifest vectors and corruption, exact-closure,
no-clobber/race tests exercise the client. One deliberately fake Unix HTTP peer
tests interrupted/mismatched downloads and exact retry; it is not server-auth
evidence and is separate from actual server tests.

Local verification (macOS arm64, Rust 1.99.1): the full ordinary suite passed
133 tests, followed by four archive tests including one added typed-closure test
(134 ordinary tests total). All 127 explicitly enabled integration tests passed,
including the real PostgreSQL/server suites and the client fault-transport test.
Formatting and strict workspace Clippy passed. Required GitHub Linux/macOS,
PostgreSQL and actual gVisor checks still apply before merge.

See [format, access rules, resource bounds and cleanup](live-source-export.md).
This does not implement full metadata backup/restore, all-ref migration, export
of every retained proposal, automatic cleanup or incremental/long-running export.
No performance claim or complete mode/release claim is made.

## Scoped native history queries

Added `client history` and `/v1/history/read` for show, log, is-ancestor and
merge-bases. The history core orders complete ancestry children-before-parents,
using raw IDs to break ties; ordered parent lists remain unchanged. First-parent
display is explicit. All best bases, including criss-cross bases, are returned.
Missing data and exhausted budgets fail instead of becoming false/empty results.

The service reuses authenticated registered-base reads and graph holds, traverses
commit edges without loading source file blobs, closes all holds even when the
second root fails, and rechecks access before success. Response-size accounting
includes JSON escaping. Wire v1 has independent request/response vectors. No
native object format or schema migration changed; the service now directly uses
the existing workspace history crate.

Local verification (macOS arm64, Rust 1.99.1): 138 ordinary tests and all 128
explicit integration tests passed, as did formatting and strict workspace Clippy.
Actual PostgreSQL/CLI tests compare criss-cross bases and first-parent history
with Git, remove original inputs, restart the server and use a read-only credential.
They cover complete pagination, nonmember roots/targets/cursors, same-tenant other
repository denial, authority epoch changes, token revocation and hold cleanup.
The existing native publication scenario also reads newly landed merge history.
An independent DAG matrix checks complete membership and parent ordering for 64
generated graphs. Linux/macOS and actual gVisor remain required CI gates.

The initial query profile loads bounded complete ancestry on every page. There is
no persisted ancestry index or performance claim. General history stack rebase, complete lifecycle,
Connected GitHub, backup/restore and release requirements remain open.

## Durable single-ChangeSet rebase

Added owner-authorized start/status/cancel and `/v1/changes/rebase` preparation,
with CLI shortcuts. The core verifies the original proposal, merges exact snapshots,
then writes a new delta and single-parent commit against the pinned new base.
Schema 12 records immutable source/result lineage, retains the original AgentRun,
and atomically pins the new version and supersedes older pending candidates.
The protected branch is unchanged and old evidence cannot admit the new version.

Local verification (macOS arm64, Rust 1.99.1): 144 ordinary tests and 135 explicit
PostgreSQL/transport integration tests passed; formatting and strict workspace
Clippy passed. New tests include actual Git rebase as an independent oracle,
aggregate read/write budgets, immutable source/run retention, simultaneous version
submission versus rebase, revoked credentials/epochs, cancellation/expiry, GC
retention, populated schema-11 upgrade, and CLI recovery after server death at
object publication and after database commit. Actual CLI tests also export and
read the new revision with Git absent. Their CI outcomes are explicitly fixtures.

The required Linux gVisor harness now includes rebase during an actual running
check and fresh check/review before publishing the new version. It remains a
required CI gate, separate from local transport tests. See [wire format, limits,
retry and operational behavior](change-rebase.md). General history stack rewriting,
Connected GitHub, full lifecycle/restore and release/benchmark gates remain open.


## Scoped immutable review diffs

Added `/v1/review/diff` and `client diff` for exact accepted versions or registered
native revision pairs. The endpoint requires repository read permission, retains
both roots with atomic holds, checks current access/authority per batch and before
success, and closes all holds on ordinary failures. Results preserve original
run/base/version identity, byte paths, modes and empty directories. Text is bounded
separately from complete path enumeration; oversized escaped JSON fails explicitly
and can be recovered with smaller pages or metadata-only reads. No refs, reviews
or validation are written.

Local verification (macOS arm64, Rust 1.99.1): 147 ordinary tests and all 140
explicit PostgreSQL/materializer/node/service tests passed. Formatting and strict
Clippy passed. Independent stdlib framing/hash/unified-patch vector, real Git
changed-path comparison, text-patch application, actual server restart/removal of
original source packs, older-version reads after rebase/drain, scoped access,
revocation/epoch/lease fences, GC protection and response-budget recovery are
covered. The required Linux gVisor harness additionally reads actual immutable
agent proposals with reviewer credentials before approval. All required CI jobs
must pass on the exact head before merge. See [review diff](review-diff.md) for the
wire contract, limits and explicit distinction from integration-candidate review.

## Exact GitHub branch transport

The trusted Rust bridge now has a separate branch-publication transport. Native
conversion creates a process-local verified artifact; a bounded Linux worker
checks its bytes and updates only a generated service branch with an explicit
expected old Git commit. The App adapter requests Contents-write/Metadata-read
on one pinned repository, verifies current identity/scope/lifetime, reads before
and after one push, and preserves uncertain outcomes without blind retries.
Source access remains read-only. Failed token revocation is reported separately
from a verified branch observation, with the upstream expiry timestamp.

Tests cover scope/identity/lifetime, stale heads, failed observations and revocation.
Linux ignored tests in the required database job use actual Git smart HTTP with
synthetic credentials: concurrent writers, applied update with lost response,
redirect refusal, modified artifact, deadline and parent-SIGKILL cleanup. These
do not establish installed-App or real GitHub queue acceptance. The required five
checks must pass on the exact PR head before merge.

The CLI joins transport to durable owner authorization and persisted reports.
PR/check/queue operations and real installed-App evidence remain unfinished.
See [branch transport](github-publisher-transport.md).

## Durable publication intent and dispatch ledger

The coordinator now retains owner requests for exact ready projections, derives
branch identities per tenant/repository/ChangeSet, pins their full graphs and
accepts verified mappings only from the assigned HostPublish bridge. A fresh begin
commit issues one dispatch flag; replay returns its receipt without another flag.
Uncertain attempts cannot be reclaimed as permission to send again. Cancellation
after dispatch preserves the attempt; scoped observations can record a remote
effect after a projection or binding stops accepting new work. Main and check
authorization remain unchanged.

Schema 18 is additive. Tests cover populated upgrades, retention, foreign scope,
wrong report/mapping identity, concurrent claims, replay, revoked credentials,
version/epoch/cancellation/deadline fences and rollback after delayed receipt/outbox
writes. The actual Unix-socket CLI/service test kills the server before and after
begin commit, then verifies historical recovery without redispatch permission.
Its Git mapping comes from a real self-contained Git export; its upstream reports
are explicitly trusted fixtures, not installed-App evidence.

The trusted `github publish` CLI now connects these records to branch transport.
It binds a private journal to the exact intent, records mapping/attempt before
claim, and invokes the write adapter only for a fresh dispatch. Current eligibility
is checked before App token creation and before push. Replays use read-only branch
observation, including without Git/source packs. Reports are durable before service
submission, and terminal receipts require no App credentials. Unknown write-token
cleanup cannot be cleared by revoking a later read token.

Journal/guard tests and actual CLI/service recovery tests cover retained reports,
cancelled attempts, exact mappings and missing local/App files. The Linux transport
test additionally drops the actual CLI's begin response after commit, kills the
process, restarts the server and checks that a saved observation recovers without
another dispatch. Its host observation is explicitly a fixture. Full installed-App
publication, PR/check/queue, automatic reconciliation, cleanup and cutover remain
required. See [publication requests](github-publications.md).
