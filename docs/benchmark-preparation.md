# File preparation versus Git worktree and clone

This opt-in probe measures one narrow task: prepare all files from already loaded
source, read one pinned file, verify the tree, then clean up. It uses the real
TreeHarbor CLI, local service, PostgreSQL and object store. The Git cases use the
same private bare mirror and exact commit. No agent code runs.

**The first small-workload sample favors Git worktree.** There is no demonstrated
TreeHarbor speed advantage. The useful question is whether retaining proposals and
explicit task/source identities helps your workflow enough to justify the service.

## Reproduce

Requirements: Unix, Python 3.9+, Git, an optimized TreeHarbor binary, and an isolated
local PostgreSQL database with matching `psql`. Signed product packages are Linux;
the sample below used a local optimized macOS build. Do not use a debug build for
performance conclusions. Do not point the script at a production database.

The script creates its own uniquely named schema, repository and temporary
local service credentials, starts a local service, and removes that schema and
its temporary files afterwards. It does not change the input Git repository or
contact GitHub. Unexpected process/machine termination can leave test resources;
inspect its `th_test_*` schema and `th-import-*` temporary directory before cleanup.
It executes no repository hooks or build commands. Use a trusted/shareable fixture.

Create the supplied deterministic fixture in a **new** directory:

```sh
python3 scripts/create-preparation-fixture.py /absolute/path/to/new-fixture
```

Save the printed full commit ID. The fixture contains 131 tracked entries, including
an executable and a relative symlink, with about 512 KiB of logical file content.
For the 2,051-entry variant, add `--modules 16 --files-per-module 128` and use
a different output directory. Each generated data file is 4 KiB.
Put your disposable local database connection in a mode-0600 file; use local socket
authentication or `PGPASSFILE`, not a password in the connection string/arguments.

```sh
python3 scripts/benchmark-workspace-preparation.py \
  --binary /absolute/path/to/treeharbor \
  --psql /absolute/path/to/psql \
  --database-url-file /absolute/path/to/private-database.url \
  --repository /absolute/path/to/new-fixture \
  --revision FULL_COMMIT_ID_FROM_THE_GENERATOR \
  --iterations 5 --concurrency 1 --seed 42 --git-checkout-workers 1 \
  --output /absolute/path/to/new-result.json
```

An existing result is never replaced. `--concurrency` accepts 1–16 simultaneous
preparations in this probe. `checkout.workers=1` fixes one checkout worker per
Git task; `0` lets Git choose parallel checkout workers. Full UTF-8 trees are
supported within the 64 MiB default input limit. Submodules are rejected and
checkout filters that change the bytes fail verification.

On Linux, repeat `--workspace-volume /path/to/mount` once per concurrent task
to measure preparations on dedicated ext4 volumes. Provision disposable,
private volumes as described in [managed runs](managed-runs.md). Each volume
must fit the limit set by `--volume-limit-bytes`, which defaults to 128 MiB.
The probe registers separate node placements, runs each Git checkout on the
same per-task volume and removes the private benchmark directories afterward.
It does not provision or mount volumes. The raw report identifies the placement
as `guarded-ext4`. The default `node-directory` placement uses ordinary node
directories and requires no volumes.

| Case | Measured preparation |
| --- | --- |
| TreeHarbor | Request an exact full native workspace and wait for its durable readiness on a selected node. |
| Git worktree | `git worktree add --detach` against the loaded mirror. |
| Git shared clone | `git clone --shared --no-checkout` from the mirror, then a detached checkout. Each clone has its own metadata and borrows the mirror's objects. |
| Git full clone | `git clone --no-local --no-checkout` from the mirror, then a detached checkout. The local Git transfer creates an independent object database. |

## Measurement boundary

- Each wave randomizes the order of systems. For each system, all lanes start as
  one batch and finish the first useful read before verification or cleanup
  starts. TreeHarbor gives each lane a different node/storage identity on the same
  host. Git cases use the same mirror and different destination directories.
- TreeHarbor timing starts before the authenticated workspace request and includes
  node process startup, preparation, durable readiness and the first useful read.
  Git timing starts before worktree creation or clone and includes the same file
  read. Version 3 also records time until all useful reads in each batch finish.
- Version 2 observations split TreeHarbor preparation into the workspace request
  and the `node prepare-once` command. The latter includes process startup,
  database connections, job claim, file preparation and readiness publication.
  Materialization counters are recorded when the binary reports them. A missing
  measurement is `null`, and encoded object bytes are local reads in this probe.
- All file bytes, executable bits, relative symlink targets and the complete leaf
  path set are checked afterwards. Private permission bits differ by design.
- Cleanup is timed separately: TreeHarbor drain/physical cleanup, Git worktree
  removal or clone directory deletion. The probe verifies the directories are
  gone. No mutable agent work is generated or discarded.
- Source preparation is recorded separately. TreeHarbor imports a snapshot while
  the Git mirror retains history, so those setup phases are not equivalent work.
- Objects are already present and no prepared tree is reused. OS caches are
  uncontrolled; this is **not** a cold-cache measurement. Verification reads can
  warm later waves for either system.
- Disk observations use allocated blocks under the measured paths. Version 3
  records per-task Git metadata separately, including the private clone object
  database. Shared mirror and TreeHarbor object-store blocks are recorded once.
  PostgreSQL storage, whole-system CPU/RAM, actual transport bytes, runtime/build/CI
  cost and daily retention are outside this measurement.
- Version 3 reports sample medians and nearest-rank p95 when at least 20 individual
  observations exist for a system. Concurrent observations within one batch
  share host resources, so these sample p95s do not establish latency SLOs.

## Initial sample — 2 November 2026

[Raw observations, hashes and environment](benchmarks/preparation-macos-arm64-sample.json).
Five randomized waves per system, macOS ARM64, 131 tracked entries. All ten
preparations passed file verification and cleanup. These are sample medians,
not an SLO or a p95 estimate.

| Median | TreeHarbor | Git worktree |
| --- | ---: | ---: |
| Files ready | 410 ms | 44 ms |
| First useful file read | 410 ms | 44 ms |
| Cleanup | 183 ms | 26 ms |

For this small full-tree task, Git worktree reaches the first read about 9.3 times
faster. TreeHarbor's additional service/authorization/readiness work is included;
this probe does not establish which component dominates the difference. No
performance conclusion is justified for sparse tasks, large repositories, higher
concurrency or complete agent-to-merge workflows.

The local optimized binary's Rust sources match release revision
`ae3fe17f1b5dd92211c9ecbdb500267bb6832690`; this is not a signed-release binary
benchmark. The raw report notes the build's optional debug-stripping warning.
Filesystem/device configuration and background load were not captured, which
limits reproducibility across machines. Repeat on your intended host and publish
its full configuration before making purchasing or capacity decisions.

The broader cold/warm, concurrency, server-cost and analogue matrix remains open.
[Share a workload](../.github/ISSUE_TEMPLATE/workload.md) or report an unfair baseline;
please retain raw failed observations as well as successful ones.

## Preparation phases — 8 November 2026

Five randomized waves per fixture and system on macOS ARM64. Each preparation
passed byte, mode, path and cleanup verification. The source objects were
already present, no prepared tree was reused, and OS caches were uncontrolled.

| Fixture | TreeHarbor request | TreeHarbor node command | TreeHarbor first read | Git worktree first read |
| --- | ---: | ---: | ---: | ---: |
| [131 entries](benchmarks/preparation-phases-macos-arm64-small.json) | 14 ms | 411 ms | 428 ms | 42 ms |
| [2,051 entries](benchmarks/preparation-phases-macos-arm64-2k.json) | 16 ms | 5,519 ms | 5,535 ms | 146 ms |

The node command accounts for most of the measured delay, and grows with the
larger fixture. Its timing includes process startup, database operations and
file preparation. Isolating those costs requires a finer measurement. These
sample medians describe one local setup with no agent execution or concurrent jobs.

## Clone baselines and concurrent preparations — 8 November 2026

Four runs used the same optimized macOS ARM64 binary as the phase probe and the
same two deterministic fixtures. Each Git task used one checkout worker. All 260
preparations passed file verification and cleanup. The raw reports record separate
setup, per-task and batch timings, disk blocks and available materialization counters.

| Fixture and concurrency | TreeHarbor | Git worktree | Shared clone | Full clone |
| --- | ---: | ---: | ---: | ---: |
| [131 entries, 1 task](benchmarks/preparation-clone-macos-arm64-small-c1.json) | 409 ms | 38 ms | 62 ms | 82 ms |
| [131 entries, 4 tasks](benchmarks/preparation-clone-macos-arm64-small-c4.json) | 1,443 ms | 58 ms | 89 ms | 116 ms |
| [2,051 entries, 1 task](benchmarks/preparation-clone-macos-arm64-2k-c1.json) | 5,190 ms | 190 ms | 219 ms | 233 ms |
| [2,051 entries, 4 tasks](benchmarks/preparation-clone-macos-arm64-2k-c4.json) | 18,979 ms | 366 ms | 389 ms | 374 ms |

The table shows the median time to the first file read per task. At four tasks
on the larger fixture, TreeHarbor's sample p95 was 20,414 ms. The Git cases
ranged from 454 to 649 ms. Each of those p95s uses 20 observations across five
batches. The larger fixture with one task uses five observations per system and
reports no p95.

On the larger fixture with four tasks, median cleanup took 717 ms for TreeHarbor
and 187–265 ms for Git. The measured working files occupied 8,396,800 allocated
bytes per task for each system. The per-task Git metadata medians were 204,800
bytes for worktree, 221,184 for shared clone and 421,888 for full clone. The
shared mirror and TreeHarbor object store are recorded separately. In that run,
the TreeHarbor snapshot pack and import took 19.2 s and 38.2 s before any timed
preparation. The local mirror setup took 62 ms and covers different work.

The TreeHarbor results cover native file workspaces. They show where to profile
the node preparation path under concurrent load. Comparisons for Git-compatible
workspaces, sparse/partial delivery, cold caches, measured network traffic,
server cost and the complete agent-to-integration path remain open.

## Within the node preparation command — 8 November 2026

A separate optimized development build adds monotonic phase timings to
[`preparation_statistics`](local-node.md). The timings cover the same 2,051-entry
native fixture on macOS ARM64, using three waves at each concurrency. All 60
TreeHarbor and Git preparations passed file verification and cleanup.

| Median per TreeHarbor task | [1 concurrent task](benchmarks/preparation-timing-macos-arm64-2k-c1.json) | [4 concurrent tasks](benchmarks/preparation-timing-macos-arm64-2k-c4.json) |
| --- | ---: | ---: |
| Node prepare command | 5.152 s | 18.392 s |
| Catalog checks and opening object descriptors | 0.273 s | 0.177 s |
| Verify, decode and materialize batches | 4.689 s | 17.703 s |
| Regular-file sync wait, included in materialization and finalization | 4.209 s | 6.190 s |
| Finalize files, manifest and directories | 0.100 s | 0.286 s |
| Publish the tree | 0.009 s | 0.022 s |

The median task spent 81% of node command time waiting for explicit file syncs
at concurrency one and 34% at concurrency four. The batch phase grew more than
the measured sync wait, while descriptor opening stayed below 0.3 s in both
samples. The next measurement should separate source verification from writing
files inside that phase. Individual medians need not sum to the median command
time. These small samples give direction for profiling this macOS setup; the
Linux runtime and filesystem still need separate measurements.

## Dedicated ext4 volumes — 9 November 2026

The same 2,051-entry synthetic commit ran on a Linux 6.8 ARM64 VM with two
CPUs and about 4 GB of RAM. The four concurrent tasks used independent
64 MiB ext4 volumes on the same host. The TreeHarbor and Git paths each
wrote their working files onto those volumes. Source objects were already
loaded. Setup and cleanup were recorded separately and OS caches were
uncontrolled. The probe used separate node processes and did not start agents.

The baseline was the optimized ARM64 binary from the [tested PR151 build](https://github.com/ArtAndrew/TreeHarbor/actions/runs/42514447643)
at source `dd8c37bfebd98e6e71960d7b1691f76e1173b680`. The candidate used the
guarded-volume synchronization change, with Rust source tree
`63a5c415e87a63759f6bd97eab839e3cb6b112a2` at `crates/` and the same Rust
toolchain and optimized ARM64 musl profile. The order was
[baseline](benchmarks/preparation-ext4-linux-arm64-2k-c4-baseline-1.json),
[candidate](benchmarks/preparation-ext4-linux-arm64-2k-c4-candidate-1.json),
[candidate](benchmarks/preparation-ext4-linux-arm64-2k-c4-candidate-2.json),
[baseline](benchmarks/preparation-ext4-linux-arm64-2k-c4-baseline-2.json).
Each run contained five waves. All 320 preparations passed file content, mode,
path and cleanup checks. Each column pools 40 observations per system across
two runs. Tasks in one wave share the host and are correlated.

| Time to first file read | Baseline runs p50 / sample p95 | Candidate runs p50 / sample p95 |
| --- | ---: | ---: |
| TreeHarbor | 1,690 / 2,159 ms | 371 / 444 ms |
| Git worktree | 131 / 161 ms | 117 / 136 ms |
| Git shared clone | 144 / 212 ms | 134 / 293 ms |
| Git full clone | 313 / 804 ms | 273 / 318 ms |

The median recorded source sync wait changed from 1,306 ms across per-file
batches to 37 ms in one volume sync. The preparation still writes and
synchronizes its manifest and publication receipt. Git worktree remains
faster on this fixture. These observations cover native source files and
readiness. Git-compatible metadata, sandbox startup, real repository sizes,
cold starts and total task cost require separate measurements.
