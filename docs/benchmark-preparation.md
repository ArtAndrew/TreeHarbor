# File preparation versus Git worktree and clone

This opt-in probe measures one narrow task: prepare all files from already loaded
source, read one pinned file, verify the tree, then clean up. It uses the real
TreeHarbor CLI, local service, PostgreSQL and object store. The Git cases use the
same private bare mirror and exact commit. No agent code runs.

**Results depend on the workload and build.** Early small-workload samples favor
Git worktree. The [unreleased shared-image experiment](#shared-source-images--9-november-2026)
is faster on repeat preparation of the larger tested tree; its first preparation
still loses to Git. These measurements do not establish a general performance or
infrastructure-cost advantage.

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

The TreeHarbor results cover native file workspaces. The Connected Git sample
below measures private Git metadata separately. Sparse/partial delivery,
cold caches, network traffic, server cost and the complete
agent-to-integration path remain open.

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
readiness. Sandbox startup, representative repository sizes, cold starts and
total task cost require separate measurements.

## Connected Git-compatible metadata — 9 November 2026

The same 2,051-entry synthetic repository with one Git commit ran on two CPUs
and about 4 GB RAM under Linux 6.8 ARM64. Each of four concurrent lanes used
its own guarded ext4 mount. An exact Connected import into local PostgreSQL
and the object store finished before measurement. Each preparation used a new
workspace. Source objects were already present and OS caches were uncontrolled.
Timing covers the workspace request, node startup, preparation and first file
read. Verification and cleanup are recorded separately.

The baseline used the guarded-volume synchronization build from the preceding
section. The candidate changes the temporary source copy for Git projection
to one private file. The measured candidate used an owner-only process umask.
The implementation also applies mode `0600` before copying source frames.
The Rust toolchain and optimized ARM64 musl profile are the same.
Four runs alternated baseline and candidate. Every run had six
waves, with randomized system order. All 288 preparations verified file
bytes, modes, paths, symlinks and, where applicable, Git history. Each column
pools 48 observations per system. The records were produced by a local driver
with the same preparation and verification boundaries as the recipe below.
[Baseline 1](benchmarks/connected-git-linux-arm64-2k-c4-baseline-1.json) ·
[Candidate 1](benchmarks/connected-git-linux-arm64-2k-c4-candidate-1.json) ·
[Baseline 2](benchmarks/connected-git-linux-arm64-2k-c4-baseline-2.json) ·
[Candidate 2](benchmarks/connected-git-linux-arm64-2k-c4-candidate-2.json).

| Time to first file read | Baseline p50 / sample p95 | Candidate p50 / sample p95 |
| --- | ---: | ---: |
| Connected, file-only | 423 / 958 ms | 350 / 802 ms |
| Connected, private Git metadata | 7,929 / 12,889 ms | 1,185 / 2,616 ms |
| Git worktree | 131 / 341 ms | 123 / 421 ms |

The two baseline Git-compatible medians were 5.46 and 10.27 s. The two
candidate medians were 1.04 and 1.29 s. A separate single-lane trace recorded
6,788 successful `fsync` calls before the change and 52 after it, including
the syncs required for final publication. The table uses untraced runs.
Git worktree remains the fastest preparation in this local fixture. The
measurements cover an already imported one-commit history and exclude runtime,
agent execution, network transfer and integration cost.

### Reproduce Connected preparation

Use a disposable PostgreSQL database over a local socket and four private
ext4 mounts as in the [managed-run volume setup](managed-runs.md). The
transport test creates a temporary schema and a synthetic Connected binding
locally. It uses the same authentication and import protocol as the service.
The operator owns the mount lifecycle.

```sh
umask 077
root=$(mktemp -d)
revision=$(python3 scripts/create-preparation-fixture.py "$root/fixture" \
  --modules 16 --files-per-module 128)
git -C "$root/fixture" bundle create "$root/fixture.bundle" refs/heads/main

python3 - "$root" "$revision" /mnt/bench0 /mnt/bench1 /mnt/bench2 /mnt/bench3 <<'PY'
import json, sys
from pathlib import Path
root, revision, *volumes = sys.argv[1:]
root = Path(root)
arguments = ["--repository", str(root / "fixture"), "--revision", revision,
             "--concurrency", "4", "--iterations", "6", "--seed", "421",
             "--git-checkout-workers", "1", "--volume-limit-bytes", "67108864",
             "--output", str(root / "result.json")]
for volume in volumes:
    arguments.extend(["--workspace-volume", volume])
with (root / "spec.json").open("x") as handle:
    json.dump({"bundle": str(root / "fixture.bundle"),
               "git_revision": "git:sha1:" + revision, "arguments": arguments}, handle)
PY

export TH_SERVICE_TEST_DATABASE_URL='host=/absolute/test/socket user=treeharbor_test dbname=treeharbor_test'
export TH_TEST_GIT_WORKSPACE_BINARY=/absolute/path/to/optimized/treeharbor
TH_TEST_CONNECTED_PREPARATION="$root/spec.json" \
  cargo test -p treeharbor-service --test transport --locked \
  github_source_http_preserves_exact_git_history_after_partial_upload_and_server_death \
  -- --ignored --nocapture
```

Read `result.json` in the temporary directory. Build each compared binary
with the same release profile and toolchain, use a fresh output path per run,
and alternate their order on one host. The probe checks the detached Git HEAD,
reachable history and worktree bytes, and verifies that the Git-compatible
placement has an independent object database. Use the Standalone command
above for the clone baselines.

## Repeated Git-compatible preparation — 9 November 2026

Two optimized Linux ARM64 builds used the same one-commit, 2,051-entry
fixture on a two-CPU host. Each run prepared eight waves of four fresh
workspaces on dedicated ext4 volumes. A new node started each lane, and each
lane kept that node identity across waves. The first wave started with an
empty node seed cache. Source objects were already present and OS caches
were uncontrolled. All 192 placements verified their file bytes, modes,
paths, applicable Git history and cleanup.

| Time to first useful read | First wave p50 | Repeat waves p50 / sample p95 |
| --- | ---: | ---: |
| Connected, private Git metadata before reuse | 965 ms | 970 / 1,069 ms |
| Connected, private Git metadata with reuse | 968 ms | 301 / 343 ms |
| Connected, file-only, same candidate run | 370 ms | 315 / 380 ms |
| Git worktree, same candidate run | 119 ms | 99 / 148 ms |

There were four first-wave and 28 repeat-wave observations per system in each
run. The repeat-wave reduction covers the node's Git metadata cache, which
does not retain prepared working files. It leaves the first-wave time and
the roughly threefold local worktree gap visible. This sample excludes
agent execution and network or GitHub costs.
[Previous build observations](benchmarks/connected-git-seed-linux-arm64-2k-c4-before.json) ·
[Seed reuse observations](benchmarks/connected-git-seed-linux-arm64-2k-c4-cache.json).

## Protected reads and manifest writing — 9 November 2026

The same fixture and guarded volumes ran two builds in the order seed reuse,
faster materialization, faster materialization, seed reuse. Each run had
eight four-task waves. All 384 placements verified and cleaned up.
Each repeat column pools 56 observations per system. The first-wave column
pools eight initially empty node seed caches per build, with source objects
already present.

| Time to first useful read | First wave p50, before → after | Repeat p50 / sample p95, before → after |
| --- | ---: | ---: |
| Connected, private Git metadata | 1,095 → 1,052 ms | 317 / 588 → 283 / 358 ms |
| Connected, file-only | 419 → 296 ms | 308 / 434 → 258 / 332 ms |
| Git worktree | 209 → 103 ms | 96 / 116 → 101 / 137 ms |

For repeated Git-compatible preparations, median time opening protected read
batches fell from 98 to 68 ms. Median finalization, which includes the
guarded-volume sync and manifest, fell from 46 to 33 ms. The two candidate
Git-compatible repeat medians were about 307 and 260 ms. The Git worktree
measurements show variation between runs on this host. OS caches were
uncontrolled. These timings apply to locally loaded source and exclude
agent execution, network and GitHub work.
[All four runs and per-placement timings](benchmarks/connected-materialization-linux-arm64-2k-c4.json).

## Verified source file reuse — 9 November 2026

The source file seed comparison uses separate nodes for file-only and
Git-compatible preparations. Both start with empty node caches and use the
same volumes in separate waves. Source objects are loaded before timing.
The host's OS cache remains uncontrolled. Four runs in A/B/B/A order
verified all 384 placements and cleanup.

| Time to first useful read | First wave p50, before → after | Repeat p50 / sample p95, before → after |
| --- | ---: | ---: |
| Connected, private Git metadata | 1,069 → 1,136 ms | 354 / 556 → 247 / 317 ms |
| Connected, file-only | 294 → 347 ms | 302 / 657 → 215 / 261 ms |
| Git worktree | 119 → 113 ms | 113 / 184 → 110 / 128 ms |

The first-wave medians use eight preparations per system and build.
The repeat statistics use 56. The candidate read source objects on
every first wave and reused local verified files on every repeat.
The first-wave cost includes creating the file seed. The worktree
rows show host variation between runs. The probe includes no agent
execution or remote GitHub or network activity.
[All four raw runs](benchmarks/connected-source-seed-linux-arm64-2k-c4.json).

## Sequential source seed reads — 9 November 2026

Four paired runs compared the directory seed with a packed leaf file in
A/B/B/A order. Both builds used the same 2,051-entry commit, guarded ext4
volumes, PostgreSQL, and four concurrent tasks on Linux ARM64. Source
objects were present before timing. The comparison uses eight first-wave
and 56 repeat placements per system and build, with uncontrolled OS caches.
All 384 placements passed exact file verification and cleanup.

| Time to first useful read | First wave p50, directory → packed | Repeat p50 / sample p95, directory → packed |
| --- | ---: | ---: |
| Connected, private Git metadata | 1,100 → 913 ms | 207 / 234 → 194 / 213 ms |
| Connected, file-only | 326 → 295 ms | 188 / 222 → 171 / 186 ms |
| Git worktree | 97 → 92 ms | 95 / 99 → 94 / 101 ms |

The node verifies every packed payload against its expected object digest
and creates independent writable files. Repeat placements used zero source
objects from the durable store. First-wave timing includes building the
cache. The sample measures local preparation through the first file read.
It excludes execution runtime, network transfer and GitHub activity.
[Raw observations](benchmarks/connected-packed-source-linux-arm64-2k-c4.json).

## Shared source images — 9 November 2026

An unreleased Linux Copy-on-Write prototype reuses a verified source image and
prepares four independent writable layers. The batch returns after all four
preparations; the table measures time through the **last of four first reads**.
These group statistics differ from per-workspace timings in earlier sections.
No coding-agent process or CI job executes in this benchmark.

All runs used a two-CPU ARM64 Linux 6.8 VM, PostgreSQL 18.6 and Git 2.43.0.
Source import and a private Git mirror were already complete. OS caches were
uncontrolled. Both formats used four guarded ext4 workspace volumes, including
the Git worktree destinations. Fixture contents, paths, modes, Git HEAD/history,
clean status, object consistency and acknowledged cleanup were checked.
The [raw reports](benchmarks/connected-cow-linux-arm64-20261109.json) retain
456 verified placements, source commits, executable hashes and phase counters.

| Profile | Files | Git checkout workers per task | First group: TreeHarbor / Git | Repeat group median: TreeHarbor / Git |
| --- | ---: | ---: | ---: | ---: |
| Shared image in node-parent filesystem; two runs | 2,051 | 1 | 1,132 / 119 ms | 127 / 116 ms |
| Shared image in node-parent filesystem | 8,195 | 1 | 2,752 / 397 ms | 152 / 315 ms |
| Shared image in node-parent filesystem, separate run | 8,195 | 2 | 2,831 / 797 ms | 339 / 1,156 ms |
| Dedicated source filesystem, final build | 8,195 | 1 | 2,068 / 572 ms | 161 / 335 ms |

The first row combines two runs, each with one initial and seven repeat groups.
The next two rows each contain one initial and seven repeat groups. The final
row has one initial and five repeat groups. Each group contains four workspaces.
The first-column numbers for a single run are individual observations, not
reliable cold-start percentiles. Checkout workers are Git's internal setting;
**all rows have four concurrent workspace tasks**.

The final build's repeat group median was about 2.1x faster than Git worktree
in that run. Git remained faster for initial preparation and for the smaller
shared-image fixture. The two-worker run was substantially slower for both
systems and does not isolate the effect of worker count. Different source
placements and builds also prevent treating this table as a scaling curve.
There is no broad p95, total-cost or production-throughput claim.

The dedicated source image used a held filesystem descriptor opened before
staging writes. Its measured source-file writeback took 23.4 ms in the
Git-compatible builder. That phase is only part of the 2,068 ms preparation;
read authorization, source construction, Git metadata, mounting, publication
and database acknowledgement still contribute. Provisioning and retiring test
filesystems are outside the timed interval. Physical upper-layer allocation,
server CPU/RAM, network transfer, builds and CI are not included.

Earlier rows correspond to source `dc304236f129757b9e189e8d9e8b6355e7632685`
and executable SHA-256
`e72f9920599e95daacf04cfd17847fefebeecb8d98836510d10f6810bae0f59f`.
The final row corresponds to source `888fe37970fb671b73063dafd3d5540db12f7a58`
and executable SHA-256
`b35088198ecf34c7fa704c63cc25e1d9d8aed5228b937c8d934263ae1f20034b`.
These are local optimized prototype builds, **not signed release packages**.
The signed 0.1.15-alpha.1 package used ordinary materialization. The experimental
Copy-on-Write profile first shipped in [0.1.16-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.16-alpha.1).

### Reproducing the experimental path

Generate the 8,195-entry fixture with `--modules 64 --files-per-module 128`.
Its commit is `d2784632f690f8e2a226f0f4e084f7bd9f44bcc1`; each generated data
file is 4 KiB. Create a bundle of its main ref. Use a disposable Linux host,
matching optimized CLI, PostgreSQL and four private 64 MiB ext4 workspace
mounts. The dedicated-source variant additionally needs two distinct empty
64 MiB ext4 mounts, each with a private `source` directory, one per format.

The existing Connected transport fixture accepts a private JSON specification
through `TH_TEST_CONNECTED_PREPARATION`: `bundle` is the absolute bundle path,
`git_revision` is `git:sha1:` followed by the fixture commit, and `arguments`
is an array of benchmark-script arguments. Set `TH_TEST_GIT_WORKSPACE_BINARY`
to the optimized CLI and `TH_SERVICE_TEST_DATABASE_URL` to the disposable
PostgreSQL connection. Invoke:

```sh
cargo test -p treeharbor-service --test transport \
  github::github_source_http_preserves_exact_git_history_after_partial_upload_and_server_death \
  -- --exact --ignored --nocapture
```

Use the ordinary `--repository`, `--revision`, `--output`, `--concurrency 4`,
`--iterations 6`, `--seed 421`, `--git-checkout-workers 1` and
`--volume-limit-bytes 67108864` arguments. Supply four `--workspace-volume`
paths, `--cow-shared-node`, and an absolute `--cow-driver` executable.
For the dedicated-source variant add two `--cow-source-volume` paths in
native/Git-compatible order. Registration and an empty source image precede
the timed preparation. The fixture imports exact source through the real
local service and uses disposable authority evidence; it does not contact GitHub.

A trusted operator must currently provide the driver. It receives
`mount|unmount lower upper work view`, restricts every path to the approved
private experiment roots, and manages an OverlayFS mount with
`nodev,nosuid,xino=on,metacopy=off,redirect_dir=nofollow,index=off` and private
mount propagation. The node independently verifies the actual placement.
The driver must detach the exact view and remove the kernel's empty work
subdirectory after unmount. Provisioning and teardown are operator work;
there is no supported general-purpose driver or automatic installer yet.
See [prototype scope](experimental-cow.md) before using these hidden flags.

To recompute a row from the raw bundle, select its named run(s), filter
`report.batches` to `system == "treeharbor-git"` or `"git-worktree"`, and
partition by `wave == 0`. Take the median `all_first_reads_seconds` in each
partition and multiply by 1,000. Do not pool group timings with per-workspace
observations or combine different executable hashes without labeling them.

## Adjacent revisions on signed 0.1.16 — 10 November 2026

The signed ARM64 CLI ran the same four-task Connected workload on the
8,195-entry fixture and a child commit that edits eight 4 KiB files. Each
revision used fresh guarded slots and an empty dedicated source volume per
format. The Git worktree baseline used two checkout workers on the same
workspace filesystems. Import was already complete and OS caches were
uncontrolled. Readiness includes the last of four tasks reading its first file.

| Revision | First batch, TreeHarbor / Git | Median of five repeat batches | Sum of six batches |
| --- | ---: | ---: | ---: |
| Original | 1,851.4 / 534.4 ms | 153.4 / 395.2 ms | 2,633.3 / 2,491.5 ms |
| Eight edits | 1,763.1 / 509.5 ms | 143.9 / 355.2 ms | 2,498.8 / 2,286.9 ms |

Git worktree had lower total preparation time over the six batches for
each revision. This is a baseline for the cost of building an image for a
new revision. It uses separate nodes per revision and does not measure
coexisting revisions or incremental construction. Cleanup timings are in
the raw data and excluded from the table. Neither agent execution nor
checks ran in these measurements.

To recreate the child of the fixture above, sort files with
`git ls-files -z` and select the first eight regular, non-symlink files of
exactly 4,096 bytes. XOR the first byte of each with 255. Commit just those
files as `Benchmark Fixture <fixture@example.invalid>` on
`2026-11-10T07:00:00Z` (author and committer) with subject
`Edit eight files for the revision change fixture`. The child is
`43c4be83b4145f8d1dee7fbd1b6f195162d5162f`. Follow the dedicated-source
setup above with `--git-checkout-workers 2` and `--seed 721` for both
revisions. Use the verified packaged driver and fresh dedicated volumes
for each run. [Original raw report](benchmarks/connected-cow-signed-arm64-adjacent-base-20261110.json)
· [Child raw report](benchmarks/connected-cow-signed-arm64-adjacent-update-20261110.json)
· [Signed release evidence](evidence/cow-workspace-release-20261109.json)

## First-build Git projection — 10 November 2026

On the same 8,195-file child commit, compare an optimized development build
from `bcb676c` with the candidate that projects checked leaves as it walks
the DAG and buffers pack writes. Both use the same collision checks, strict
Git index/graph validation and durable finalization. Phase timers measure
Git-source collection, projection and Git commands separately.

Each run imports into fresh disposable storage before timing. It uses four
concurrent tasks, two Git checkout workers, one initially empty dedicated
source volume per format and two batches. The second pair reverses the order
of the builds. Each run verifies paths, bytes and modes, and independently
checks the exact Git tip, history and private metadata. The second pair also
checks pack SHA-256 after timing.

| Comparison | Project Git pack, base / candidate | First Git-compatible batch, base / candidate | Git worktree first batch in the same runs |
| --- | ---: | ---: | ---: |
| Base first | 556.6 / 475.0 ms | 1,888.0 / 1,914.4 ms | 500.9 / 634.9 ms |
| Candidate first | 608.9 / 463.3 ms | 2,102.3 / 1,826.4 ms | 518.3 / 526.1 ms |

Pack projection time fell by 15% and 24%. All sixteen Git-compatible
placements in the second pair produced the same pack bytes across the two
builds. The first-batch results vary in direction. Git worktree remained
faster for a first batch in all four runs. OS caches were uncontrolled.
These are development measurements, separate from the signed release results.

Reproduce the batch measurements with the adjacent-revision fixture and
dedicated-source setup above. Use two iterations and add `--cow-max-images 2`
with a build that supports multiple revision images. The option selects the
volume layout. Each format still starts from an empty volume and prepares
only the selected revision. Use the same optimized profile for both binaries.
[Raw observations, executable hashes and phase logs](benchmarks/cold-git-projection-linux-arm64-20261110.json)
