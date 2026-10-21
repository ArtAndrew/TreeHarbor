# Implementation status

Updated: 2026-10-21. This file describes implemented behavior; design documents
describe the complete target. A foundation milestone does not complete either mode.

| Component | Evidence | Remaining |
|---|---|---|
| Canonical SHA-256 object format | Versioned framing, independent golden vectors, strict decoder | Compatibility policy for later formats |
| Immutable directory trees and operations | Exact expected-before checks, structural sharing, explicit deletes | Native merge/rebase, Git import/export |
| Local object store | Atomic no-clobber publication, digest verification, file/directory fsync, tenant directories | Metadata publication, GC generations, S3, backup/restore, kill fault campaign |
| Native DAG queries | All-parent traversal, multiple best merge bases, bounds | Native refs, integration, policy and full no-Git end-to-end test |
| PostgreSQL metadata and outbox | Real database transaction/idempotency/lease tests; repository authority/epoch checks | Object catalog/pins, full ChangeSet admission, full server crash/restore |
| Exact validation predicate | Context binding, stale-target rejection, trusted issuer filtering, revocation and conflicting receipt tests | Authenticated collectors, durable evidence, review and atomic publication checks |
| API, CLI, workspaces and runtime | Not implemented | Both complete user journeys and secure runtime binding |
| Connected | Not implemented | Real GitHub PR/checks/merge queue and credential separation |
| Standalone S2 | Not implemented | Durable native admission and publication without Git processes |
| Release and benchmarks | Not released or measured | Clean installation, artifacts, SBOM/provenance, comparison and operating cost |

## Local verification

macOS arm64, Rust 1.99.1, 2026-10-21:

```text
cargo test --workspace --locked --offline
30 tests passed without external services: core 11, local storage 6, native history 3, metadata input 1, validation 9.
7 PostgreSQL integration tests passed explicitly against local PostgreSQL 18.6.
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
Passed.
cargo fmt --all
Applied before the checks above.
Initial published foundation CI passed on Ubuntu 24.04 and macOS 15:
https://github.com/ArtAndrew/TreeHarbor/actions/runs/39575656294
PostgreSQL implementation PR passed Linux, macOS, PostgreSQL contracts and contribution-rights checks:
https://github.com/ArtAndrew/TreeHarbor/pull/1
```

Test stores used for pure model tests are not a server implementation. Local
storage tests use actual files; they have not yet proved power-loss behavior on
every filesystem. No benchmark timing or customer adoption is claimed.

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
