# Copy-on-Write workspace preview

This opt-in Linux profile reuses one prepared source image across
multiple workspaces. Each workspace has a separate writable layer. It is
available in the signed Linux packages starting with
[0.1.16-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.16-alpha.1).
It remains an experimental, manually configured profile of the
[Connected preview](connected-pr-preview.md).

The measured benefit is faster **repeat file preparation on the larger tested
tree**. The first image still costs more to prepare than Git worktree on that
fixture. See [conditions, results and raw data](benchmark-preparation.md#shared-source-images--9-november-2026).

## What the preview does

- Builds one exact image per local node identity, keyed to the tenant,
  repository, authority epoch, revision, snapshot, source profile and limits.
- Supports native files or full Git-compatible images with private derived
  Git metadata. Different source identities require different nodes today.
- Mounts separate writable layers on registered, bounded ext4 volumes and
  checks the actual lower, upper, work and view identities before readiness.
- Uses a bounded local batch for multiple preparations on one node. Current
  source access and PostgreSQL readiness checks still apply to every request.
- Services exact run, capture and cleanup IDs in a bounded
  [shared-node worker](node-work-selection.md). Owner requests, acceptance and
  drain remain separate authenticated operations.
- Opens the same verified view for execution and capture. Cleanup checks
  runtime removal, records detach intent and unmounts the exact placement
  before deleting workspace files. Uncertain placement does not authorize deletion.
- Optionally keeps the source image on its own registered ext4 filesystem.
  A held descriptor predating staging writes allows filesystem-wide writeback
  on supported kernels. Writeback failure prevents publication. The source
  volume cannot also serve as an agent's writable slot.

The prepared image stays outside the agent's exposed directory. The external
runtime still supplies process, network, credential and resource isolation;
Copy-on-Write alone is not an execution sandbox. This is not a demand-loaded
filesystem: the complete selected source image is materialized before use.

## Deployment profile

Use a dedicated Linux 6.8+ node, PostgreSQL/local source storage, Docker with
gVisor (`runsc`), and separate operator-provisioned ext4 filesystems for writable
slots. Follow [local node setup](local-node.md) and [managed runs](managed-runs.md)
first. Linux x86-64 and ARM64 are the package targets. macOS does not mount these
workspaces. No Kubernetes, S3 account or provider service is required.

The node account is a trusted host operator with runtime access. Agents run
under the external runtime and receive only their own workspace view, never
the lower image, host helper, database/node credentials or runtime socket.
The helper does not contain a hostile host operator and is not a replacement
for gVisor. All source and mount paths must be canonical absolute paths using
letters, digits, `/`, `.`, `_` and `-`, without symlink components.

Keep the node's parent directory private (`0700`). A source image is keyed to
the exact repository, epoch, revision, profile and preparation limits. Reuse
requires those identities to match. Use a separate node identity and fresh slots
for another source identity; automatic image rotation is not implemented.
Keep the configured workspace byte limit at most 256 MiB, and use the same
preparation limits for repeat requests. These are preview resource bounds,
not license restrictions.

## Install the host helper

First verify and install the selected Linux archive using the
[signature and installation instructions](release-packaging.md). The archive
contains `treeharbor`, `treeharbor-runtime-init` and `treeharbor-cow-driver`.
Copy the helper to a root-owned versioned directory outside the user-writable
installation. Replace `VERSION` and the prefix with the actual verified package:

```sh
TH_VERSION=VERSION
TH_PREFIX="$HOME/.local"
TH_COW_DRIVER="/usr/local/libexec/treeharbor/$TH_VERSION/treeharbor-cow-driver"
test ! -e "$TH_COW_DRIVER"
sudo install -d -o root -g root -m 0755 "/usr/local/libexec/treeharbor/$TH_VERSION"
sudo install -o root -g root -m 0755 \
  "$TH_PREFIX/lib/treeharbor/$TH_VERSION/bin/treeharbor-cow-driver" "$TH_COW_DRIVER"
cmp "$TH_PREFIX/lib/treeharbor/$TH_VERSION/bin/treeharbor-cow-driver" "$TH_COW_DRIVER"
"$TH_COW_DRIVER" --version
```

An administrator must separately allow the node account to run this exact
protected helper through noninteractive `/usr/bin/sudo`. Use the host's normal
privilege-review process. The installer grants no access and edits no sudo policy.
A helper left in a user-owned directory refuses elevation, even if its bytes
are correct. Do not grant this permission to the agent's host identity.

The helper accepts only the product's fixed `mount`/`unmount` overlay layout.
It checks directory ownership, source/upper/work/view paths and mount options,
and holds a per-view lock in the privileged process through the syscall.
It does not invoke arbitrary mount utilities, force/lazy-unmount a busy view,
recursively delete files or silently replace a mismatched mount.

## Prepare and run a task

Create the workspace with the existing authenticated client and retain its ID.
Using the registered node and slot from the setup guide:

```sh
treeharbor node prepare-once \
  --config "$TH_STATE/config.json" \
  --credential-file "$TH_STATE/node-credential.json" \
  --root "$TH_STATE/node" --storage "$TH_STORAGE" \
  --workspace "$WORKSPACE_ID" --slot "$SLOT_ID" \
  --max-workspace-bytes 134217728 --cow-driver "$TH_COW_DRIVER"
```

For an explicitly requested full Git-compatible Connected workspace, also use
`--git /usr/bin/git`. Ordinary native workspaces need no Git process for
preparation. Wait for `state: files_ready` and `placement: copy-on-write` before
requesting managed execution. An `idle` result is not proof of completion.
`node prepared` can reopen a ready workspace before a runtime is bound; it
does not reopen a workspace that already has a requested or active run.

For concurrent preparation, `node prepare-batch` takes the same node arguments,
`--cow-driver`, optional `--git`, `--parallelism 4` and a `--selection` file:

```json
[
  {"workspace":"d9480199-f366-4514-ac33-682da953b6bb","slot":"a142f86e-fda1-485b-89d2-a982ccad5f1e"},
  {"workspace":"ddf6fe0d-2d39-4591-a1ed-35c9c8dfe3cf","slot":"fa23eb3d-dc4b-460b-923d-b2bbdef8638e"}
]
```

Use your persisted IDs, unique workspaces and slots, and a private `0600` file
at most 4 KiB. The array contains 1–16 entries and cannot exceed `--parallelism`.
Its preparation ceiling is 256 MiB, reduced to each slot's configured bound;
keep those limits consistent with prior preparations on this node. Inspect each
entry's result. The batch does not create workspaces or start agents.

Use [work-selected](node-work-selection.md) to service exact run/capture/cleanup
IDs concurrently on the same node. Pass `--cow-driver "$TH_COW_DRIVER"` so it can
retire overlay placements. The owner still requests stop/capture, accepts the
immutable proposal and drains the workspace. Single-task cleanup uses the same
helper on `node cleanup-once`. GitHub PR publication uses the existing bridge;
this storage profile changes neither GitHub write permissions nor merge policy.

Before first preparation, an optional dedicated source volume can reduce source
image writeback overhead. Provision a separate bounded, private ext4 mount with
an empty `source` directory owned by the node account, then use
`node cow-source-register` with the same node arguments, `--mount` and
`--limit-bytes` (64 MiB–1 GiB). It cannot also be a writable workspace slot.
The ordinary node-parent source image requires no separate source volume.

## Stop, retry and recover

- Stop an agent through its authenticated run-stop/capture workflow. Killing
  the node worker alone does not stop the agent or authorize file deletion.
- After a worker crash, retain the same node root, mount paths, volumes and
  execution IDs. Prove the old worker exited, let its leases expire, then retry
  the same operation/selection. The runtime journal checks the original attempt;
  it does not launch a second container to resolve an uncertain first one.
- Preparation retries can finish an exact mount left before readiness. Once a
  placement has been acknowledged, a changed mount identity is an error, not
  permission to remount it and continue with the old receipt.
- A locked or busy mount causes refusal and retained state. Inspect the owner
  process/runtime and retry the original cleanup when it is safe. Do not remove
  lock files or use force/lazy unmount to bypass the checks.
- Reboot, remounted/missing disks, a lost node and changed source identities
  require operator recovery. Automatic remount and node replacement are outside
  this alpha. Preserve all original state and independently retained changes.

Back up source/proposals and metadata through the documented stopped-service
procedure. A node-root backup alone does not include the separate source image
or mounted writable volumes. Do not delete those while any workspace is live.
To stop evaluating this profile, capture and accept required changes, explicitly
drain/clean all workspaces and stop their workers. New tasks can use ordinary
placements on another node; retained proposals can still export to Git.

## Evidence and limits

Local Linux tests cover interrupted preparation, reopening the same mounted
view, two isolated scripted agents, stopped-file capture, conflicting native
changes with fresh validation, interrupted cleanup and Git export.

A subsequent [ARM64 development trial](evidence/shared-node-work-20261109.json)
ran two different agent owners on one node with a shared source image. The first
task was stopped, captured and deleted while the second kept writing. The lower
source bytes remained unchanged. Repeated selections did not recreate runs or
captures, and a wrong-node cleanup failed without cancelling the selected tasks.
Both proposals then exercised conflict handling, fresh validation and Git export.
The same scenario passed on ordinary ext4 placements. These are scripted tasks
using actual PostgreSQL and gVisor, not LLMs or a throughput measurement.

The dedicated-source contract checks registration replay, changed ownership,
old writable-slot refusal, altered records and reopening. It runs against a
real disposable ext4 volume in the required database CI job. These component
checks do not certify a production deployment or an installable release.

The preview preparation path requires the explicitly supplied, protected mount
helper and operator-provisioned volumes. No agent chooses mount paths or
receives mount privileges. There is one bounded source image per node, with
no automatic revision rotation or image eviction. The prototype currently
requires workspace limits at most 256 MiB. This is an operational limit,
not a license quota.

Automatic remount after reboot, missing-volume recovery and multi-image
retention are unfinished. A changed or missing mount fails closed and retains
the associated state for operator recovery. Do not adopt this path for
unattended workloads or treat an existing node backup as a complete backup
of the separate source and workspace volumes.

The Rust helper has also passed a local ARM64 development trial with
real PostgreSQL and gVisor: exact mount replay, wrong-source refusal, contention
with an independent lock holder, and refusal to elevate a user-owned executable.
After terminating the shared worker, both original containers were recovered;
the first task was captured and deleted while the second kept writing. Conflicting
proposals required fresh checks/review and exported to ordinary Git. These are
development tests, not a signed release or a fresh live GitHub PR trial.

## Packaged acceptance

Both Linux architectures passed the shared-node/helper/recovery drill using
the installed 0.1.16-alpha.1 packages. The release workflow retained those exact
archives and checked the protected host helper's bytes before signing.
Independent verification matched the signatures, source revision and producing
workflow. A separate installation of the signed ARM64 archive passed the
same-node recovery and overlapping-task scenario with PostgreSQL and gVisor.
See the [release evidence](evidence/cow-workspace-release-20261109.json).
This release does not claim a new live GitHub PR trial or an LLM benchmark.
General cache rotation, broader workloads and full Connected/Standalone
operational certification follow as separate increments.
