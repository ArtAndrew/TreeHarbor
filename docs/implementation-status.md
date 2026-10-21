# Implementation status

Updated: 2026-10-21. This file describes implemented behavior and remaining release
gates. A foundation milestone does not complete either mode.

Development increment: [managed AgentRun](managed-runs.md) requests, owner-bound
status/cancellation, immutable runtime policy/placement records and terminal receipt
replay now have real PostgreSQL tests. Schema 8 preserves existing runs and reserved
preparations. The node execution adapter and CLI are present, but their complete
service-to-gVisor flow is still awaiting Linux integration evidence. Do not infer
that evidence from the earlier standalone controller tests below.

| Component | Evidence | Remaining |
|---|---|---|
| Canonical SHA-256 object format | Versioned framing, independent golden vectors, strict decoder | Compatibility policy for later formats |
| Immutable directory trees and operations | Exact expected-before checks, structural sharing, explicit deletes | Native merge/rebase, Git import/export |
| Local object store | Atomic no-clobber writes, digest verification, file/directory fsync, tenant directories, physical generations and upload receipts | S3, backup/restore, kill fault campaign |
| Native DAG queries | All-parent traversal, multiple best merge bases, bounds | Native refs, integration, policy and full no-Git end-to-end test |
| PostgreSQL metadata and outbox | Real database transaction/idempotency/lease tests; repository authority/epoch checks; populated schema upgrades through v8; local service pool and crash/replay | Remote service integration and full server restore |
| Native ChangeSet intake | Computed immutable versions, base/run lineage, atomic graph pins and replay; worker process-kill test | Git base import, remote API, review/validation/merge admission and retention |
| Protected incremental source reads | Pinned sessions, verified frontiers, bounded prefix batches and descriptors surviving local GC; authenticated workspace-root sessions | Per-tenant service quotas, bulk node delivery and S3 |
| Native file materialization | Private copies from exact snapshots, full/sparse selection, staged expansion preserving edits/deletions, bounded IO and filesystem checks; exact baseline loading and guarded atomic node publication | Runtime-proven quiescence, cache/reflink, changed-file sealing and Git-compatible format |
| Object catalog and retention | Receipt-bound publication, typed edges, atomic persistent pins, read holds, generation-safe retirement/retry; independent reachability model | Automatic GC/reconciliation/dry-run, abandoned-upload cleanup, S3 and full restore |
| Exact validation predicate | Context binding, stale-target rejection, trusted issuer filtering, revocation and conflicting receipt tests | Authenticated collectors, durable evidence, review and atomic publication checks |
| Credentials and agent commands | Repository-scoped opaque tokens, transactional revocation, distinct actor classes, owner-bound reads and strict command dispatch | Remote transport, MCP, production identity/admission integration |
| Local HTTP service and CLI | Actual Unix-socket server/client, trusted admin and file-preparation node CLI, private credentials, bounded HTTP/pool, strict startup schema, SIGKILL replay, cancellation/rollback, reconnect and graceful shutdown tests | Git import, runtime integration, TLS/remote access, packaged installation |
| Workspace baseline API | Owner/generation/epoch-bound directory pages and verified blob download, repeated authorization, final revocation/drain rejection, bounded response ownership, actual CLI/HTTP tests | Bulk delivery to nodes, remote streaming, persistent cache and runtime binding |
| Source graph uploads | Authenticated scoped sessions, child membership, fixed deadlines/budgets, backend identity, binary HTTP/CLI, concurrent retry, restart and schema-v5 upgrade tests | Git import, incremental reuse, streaming/batching and automatic cleanup |
| Native directory import | Explicit selection, descriptor-relative capture, durable pack/journal v1, independent vector, actual CLI-to-server partial-upload recovery and completed replay with Git absent from PATH | Git history import, incremental reuse, automatic spool cleanup and runtime-proven snapshots |
| Source delivery profiles | Canonical full/sparse format with independent vectors, immutable scoped definitions, workspace foreign-key binding, v4 upgrade, authenticated CLI/HTTP and exact node use | Profile retention and generation-aware expansion |
| Workspaces and runtime | Logical workspace/run state, drain fencing and separate filesystem preparation implemented; node identity checks | Real node/runtime observation, sandbox binding and lifecycle recovery |
| Runtime task supervisor | Linux namespace init, privilege guards, environment clearing, guest process limit, child reaping and deadline; real gVisor CI harness | Durable node binding, trusted results and complete lifecycle |
| Local runtime controller | Versioned immutable profiles, restricted Engine API, bounded ext4 identity, exclusive durable volume assignment and create/start/remove journal; real-runtime fault harness | Service-managed AgentRun/epoch binding, provisioning, host/engine replacement recovery and cleanup integration |
| Workspace preparation reservations | Operator-registered node storage, scoped job claims, immutable source/slot/attempt identity, renewal/reclaim, exact file-ready observation and drain retention; real PostgreSQL race/upgrade tests | Separate runtime attempts, actual drain/removal and resource release |
| Local preparation node | Private physical root identity/OS lock, durable placement intent, exact no-replace publication, independent concurrent slots, current authorization and file-ready acknowledgment; actual process-kill recovery and import-to-files CLI tests | Scheduler/daemon, runtime binding, disk quotas, remote workers, fenced volume migration and safe published-tree cleanup |
| MCP adapter | Planned; not implemented | Shared-service adapter, scoped tools and real client interoperability tests |
| Connected | Not implemented | Real GitHub PR/checks/merge queue and credential separation |
| Standalone S2 | Not implemented | Durable native admission and publication without Git processes |
| Release and benchmarks | Not released or measured | Clean installation, artifacts, SBOM/provenance, comparison and operating cost |

## Local verification

macOS arm64, Rust 1.99.1, 2026-10-21:

```text
cargo test --workspace --locked --offline
80 tests passed without external services: core 15, local storage 8, native history 3, metadata input/auth/protocol/run-binding 4, validation 9, materializer 14, node identity/format/IO lifetime 5, service configuration/files/CLI/pack/read transport 12, runtime argument/host guards and controller profile/journal/mount contracts 10.
98 PostgreSQL integration tests passed explicitly against local PostgreSQL 18.6: transactions 7, object catalog 12, intake 9, protected reads 9, materializer 10, access/commands/source sessions 10, source profiles/upgrade 2, source uploads/upgrade 7, preparation/run ownership/upgrade 15, node preparation/recovery 7, service process/transport 10.
The combined run uses cargo test --workspace --locked --offline -- --include-ignored with both test database URLs set.
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
persistent disk capacity. This is not yet a service-managed workspace or a proof
of restart/DB reconciliation. No READY or Validation receipt is produced by the
supervisor tests.

The [local runtime controller](runtime-controller.md) now has a separate real-engine
fault harness in the same required job. It delays actual Engine traffic and kills
the Rust controller around create/start/remove, checks independent task volumes,
retains uncertain starts and refuses configuration drift. Local tests verify its
canonical profile/intent vectors, journal exclusion and invalid recovery records.
Linux runtime evidence comes from the required CI job, not macOS tests. This controller is not
yet proven through the new authoritative AgentRun/node-lease adapter. Source sealing
and workspace deletion remain absent; it cannot grant repository admission or trusted Validation.

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
the same receipt after a worker process is killed. ChangeSet submission remains a
library operation; local HTTP/CLI exposes repository/workspace commands only.
Remote transport, physical workspace/runtime binding, full application
failure injection, fenced restore and automatic maintenance remain release gates.

Protected local reads now resolve only requested object frontiers and retain opened
file descriptors across GC without holding a database transaction during content
verification. Tests include expired sessions, complete physical collection, generation
reuse, corrupt bytes, resource limits and selective reads without loading commit
parents. Authenticated workspace baseline reads now expose directory pages and verified file
bytes through the local CLI/HTTP service. Bulk node delivery, scheduling and runtime
isolation remain absent. See [source reads](source-reads.md) for the wire format,
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
No runtime is launched. Production runtime binding, scheduling, physical disk
quotas and safe published-tree removal remain unimplemented.

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
MCP, remote TLS transport, per-tenant service admission, runtime binding and complete
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
