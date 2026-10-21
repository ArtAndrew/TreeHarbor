# Implementation status

Updated: 2026-10-21. This file describes implemented behavior; design documents
describe the complete target. A foundation milestone does not complete either mode.

| Component | Evidence | Remaining |
|---|---|---|
| Canonical SHA-256 object format | Versioned framing, independent golden vectors, strict decoder | Compatibility policy for later formats |
| Immutable directory trees and operations | Exact expected-before checks, structural sharing, explicit deletes | Native merge/rebase, Git import/export |
| Local object store | Atomic no-clobber writes, digest verification, file/directory fsync, tenant directories, physical generations and upload receipts | S3, backend configuration validation, backup/restore, kill fault campaign |
| Native DAG queries | All-parent traversal, multiple best merge bases, bounds | Native refs, integration, policy and full no-Git end-to-end test |
| PostgreSQL metadata and outbox | Real database transaction/idempotency/lease tests; repository authority/epoch checks; populated schema upgrade | Authenticated service integration and full server crash/restore |
| Native ChangeSet intake | Computed immutable versions, base/run lineage, atomic graph pins and replay; worker process-kill test | Git base import, remote API, review/validation/merge admission and retention |
| Protected incremental source reads | Pinned sessions, verified frontiers, bounded batch lookup and descriptors surviving local GC | Authenticated API, IO/concurrency budgets, S3 and physical materialization |
| Object catalog and retention | Receipt-bound publication, typed edges, atomic persistent pins, read holds, generation-safe retirement/retry; independent reachability model | Automatic GC/reconciliation/dry-run, abandoned-upload cleanup, S3 and full restore |
| Exact validation predicate | Context binding, stale-target rejection, trusted issuer filtering, revocation and conflicting receipt tests | Authenticated collectors, durable evidence, review and atomic publication checks |
| API, CLI, workspaces and runtime | Logical workspace/run state and drain fencing implemented; no filesystem/runtime yet | Authenticated coordinator, API/CLI, materialization and secure runtime binding |
| MCP adapter | Planned in ADR 011; not implemented | Shared-service adapter, scoped tools and real client interoperability tests |
| Connected | Not implemented | Real GitHub PR/checks/merge queue and credential separation |
| Standalone S2 | Not implemented | Durable native admission and publication without Git processes |
| Release and benchmarks | Not released or measured | Clean installation, artifacts, SBOM/provenance, comparison and operating cost |

## Local verification

macOS arm64, Rust 1.99.1, 2026-10-21:

```text
cargo test --workspace --locked --offline
33 tests passed without external services: core 12, local storage 8, native history 3, metadata input 1, validation 9.
35 PostgreSQL integration tests passed explicitly against local PostgreSQL 18.6: transactions 7, object catalog 12, intake 8, protected reads 8.
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
Passed.
cargo fmt --all
Applied before the checks above.
Initial published foundation CI passed on Ubuntu 24.04 and macOS 15:
https://github.com/ArtAndrew/TreeHarbor/actions/runs/39575656294
PostgreSQL implementation PR passed Linux, macOS, PostgreSQL contracts and contribution-rights checks:
https://github.com/ArtAndrew/TreeHarbor/pull/1
Exact-validation and object-catalog PRs passed the same four required checks:
https://github.com/ArtAndrew/TreeHarbor/pull/2
https://github.com/ArtAndrew/TreeHarbor/pull/3
```

Test stores used for pure model tests are not a server implementation. Local
storage tests use actual files; they have not yet proved power-loss behavior on
every filesystem. No benchmark timing or customer adoption is claimed.

The catalog tests use real files and PostgreSQL, including simultaneous pin/GC,
duplicate hold tokens, deletion retries, a crash boundary between deletion and
database completion, and published-v1 schema upgrade. See
[ADR 009](adr/009-object-catalog-and-generations.md). Native intake now commits a
ChangeSet version, lineage, graph pins, response and outbox atomically, and recovers
the same receipt after a worker process is killed. See
[ADR 010](adr/010-change-intake-and-execution-lineage.md). These commands are internal:
coordinator authentication, physical workspace/runtime binding, full application
failure injection, fenced restore and automatic maintenance remain release gates.

Protected local reads now resolve only requested object frontiers and retain opened
file descriptors across GC without holding a database transaction during content
verification. Tests include expired sessions, complete physical collection, generation
reuse, corrupt bytes, resource limits and selective reads without loading commit
parents. See [ADR 012](adr/012-protected-incremental-local-reads.md). Per-service
authentication, scheduling, runtime isolation and file materialization are still absent.

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
