# File preparation versus Git worktree

This opt-in probe measures one narrow task: prepare all files from already loaded
source, read one pinned file, verify the tree, then clean up. It uses the real
TreeHarbor CLI, local service, PostgreSQL and object store, and compares them with
`git worktree add --detach` from a private bare mirror. No agent code runs.

**The first small-workload sample favors Git worktree.** There is no demonstrated
TreeHarbor speed advantage. The useful question is whether retaining proposals and
explicit task/source identities helps your workflow enough to justify the service.

## Reproduce

Requirements: Unix, Python 3.9+, Git, an optimized TreeHarbor binary, and an isolated
local PostgreSQL database with matching `psql`. Signed product packages are Linux;
the sample below used a local optimized macOS build. Do not use a debug build for
performance conclusions. Do not point the script at a production database.

The script creates its own uniquely named schema, repository and two temporary
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
Put your disposable local database connection in a mode-0600 file; use local socket
authentication or `PGPASSFILE`, not a password in the connection string/arguments.

```sh
python3 scripts/benchmark-workspace-preparation.py \
  --binary /absolute/path/to/treeharbor \
  --psql /absolute/path/to/psql \
  --database-url-file /absolute/path/to/private-database.url \
  --repository /absolute/path/to/new-fixture \
  --revision FULL_COMMIT_ID_FROM_THE_GENERATOR \
  --iterations 5 --seed 42 --git-checkout-workers 0 \
  --output /absolute/path/to/new-result.json
```

An existing result is never replaced. `checkout.workers=0` lets Git choose parallel
checkout workers; its own size threshold still applies. Set the flag explicitly
if your usual Git configuration differs. Full UTF-8 trees are supported within the
64 MiB input limit; submodules are rejected. Filters that change checkout bytes
will fail the comparison rather than silently compare different trees.

## Measurement boundary

- Concurrency is one. Each wave randomizes which system runs first.
- TreeHarbor timing starts before the authenticated workspace request and includes
  node process startup, preparation, durable readiness and the first useful read.
  Git timing starts before worktree creation and includes the same file read.
- All file bytes, executable bits, relative symlink targets and the complete leaf
  path set are checked afterwards. Private permission bits differ by design.
- Cleanup is timed separately: TreeHarbor drain/physical cleanup and Git worktree
  removal. No mutable agent work is generated or discarded.
- Source preparation is recorded separately. TreeHarbor imports a snapshot while
  the Git mirror retains history, so those setup phases are not equivalent work.
- Objects are already present and no prepared tree is reused. OS caches are
  uncontrolled; this is **not** a cold-cache measurement. Verification reads can
  warm later waves for either system.
- Disk observations use allocated blocks under the measured paths. Workspace
  counts exclude Git metadata. PostgreSQL storage, whole-system CPU/RAM, network,
  runtime/build/CI cost and daily retention are not measured.

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
