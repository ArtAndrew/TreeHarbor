# Experimental Copy-on-Write workspaces

This Linux source-code prototype reuses one prepared source image across
multiple workspaces. Each workspace has a separate writable layer. It is
not included in 0.1.15-alpha.1 and has no supported installer yet. Use the
[Connected preview](connected-pr-preview.md) for the released workflow.

The measured benefit is faster **repeat file preparation on the larger tested
tree**. The first image still costs more to prepare than Git worktree on that
fixture. See [conditions, results and raw data](benchmark-preparation.md#shared-source-images--9-november-2026).

## What the prototype does

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

The hidden preparation path requires an explicitly supplied, trusted mount
driver and operator-provisioned volumes. No agent chooses mount paths or
receives mount privileges. There is one bounded source image per node, with
no automatic revision rotation or image eviction. The prototype currently
requires workspace limits at most 256 MiB. This is an operational limit,
not a license quota.

Automatic remount after reboot, missing-volume recovery and multi-image
retention are unfinished. A changed or missing mount fails closed and retains
the associated state for operator recovery. Do not adopt this path for
unattended workloads or treat an existing node backup as a complete backup
of the separate source and workspace volumes.

## Before a packaged alpha

The next gates are one documented installation and mount-driver profile,
explicit stop/recovery behavior, and the shared-node tests on signed installed
artifacts. The source trial above does not replace those installation checks.
General cache rotation, broader workloads and full Connected/Standalone
operational certification follow as separate increments.
