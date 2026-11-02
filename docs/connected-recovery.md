# Connected recovery — 0.1.12-alpha.1

Available in [signed 0.1.12-alpha.1 packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.12-alpha.1).
Earlier packages do not contain the complete reconnection protocol described here.

Recover retained source and prepare a new GitHub connection after restoring a
trusted cold backup. This is an **operator-assisted alpha workflow**, not automatic
failover. GitHub stays authoritative for published branches. Old connections stay
disabled; no old PR, queue or check operation is automatically sent again.

The [ARM64 acceptance receipt](evidence/connected-restore-live-20261102.json)
identifies the exact development archive used for a real backup/restore, live
GitHub reads, source preservation, reconnection and retained/new proposal export.
It did not dispatch host writes or execute managed agents. Unknown post-backup
mutations, full queue certification, old-node cleanup and unattended production
recovery remain unverified.

## Before upgrading or recovering

Verify the selected package's [signatures and source](release-packaging.md) and
install it into a new prefix. Stop writers before taking an independent backup of
the current database, object store and private client/worker journals. Keep the
original backup unchanged. Do not run old and recovered services as concurrent
writers to one repository.

There is no schema migration change from 0.1.11, but upgrade recovery clients and
server together: restore context is version 2. Version 1 context/decision hashes
cannot authorize new work against it. Older receipts remain historical evidence.
The new server enforces a durable reconnect restriction even for quarantine
journals created by 0.1.11. An old binary does not implement that restriction;
downgrading is not a way to resume publishing safely.

Before reconnecting, stop old writers and runtimes, fence their App keys, revoke
or wait out issued tokens, investigate in-flight host mutations, control relevant
host writes and review branch policy. The product records these operator
assertions; it cannot independently prove them. If they cannot be established,
keep publishing disabled and export retained source for manual investigation.

## Follow the recovery sequence

| Step | Action and evidence | Who performs it |
| --- | --- | --- |
| 1 | [Verify and stage the backup](restore-stage.md) into a new isolated database. Keep incomplete destinations for inspection. | Local service operator with database administration access |
| 2 | [Quarantine old work](restore-quarantine.md), then [configure independent live storage](restore-configure.md). Provision only explicitly selected fresh credentials. | Local operator; commands do not issue credentials or start services |
| 3 | [Review retained contexts and host inventory](restore-github-assessment.md). If no binding survived, [register one discovery-only target](restore-first-target.md). | Assigned recovery bridge (`host.read`); target registration requires owner `repository.write` |
| 4 | Inspect retained [PR](restore-pull-evidence.md), [queue/removal](restore-queue-evidence.md), [check](restore-check-evidence.md) and [delivery](restore-delivery-evidence.md) identities as applicable. Preserve private reports and investigate uncertain outcomes. | Assigned recovery bridge with read-only App access |
| 5 | [Acquire missing exact source](restore-acquire-source.md), verify mappings and retain independent pins. [Record decisions](restore-decisions.md) for every retained operation. `do_not_replay` is intent, not proof that an old request never ran. | Recovery owner; App read access is needed for acquisition |
| 6 | [Plan reconnection](restore-reconnect.md), then obtain a fresh prospective-bridge probe and [sealed inventory](restore-inventory-coverage.md) for every old binding. Run [repository-wide review](restore-review.md). | Owner plus separately scoped recovery bridges |
| 7 | Activate once with the exact plan/probe/review identities. Check current status and export retained source before starting explicitly chosen new work. | Owner; ordinary publication/queue permissions remain separate |

Use the packaged `treeharbor` command in the linked examples. Store credentials,
App configuration and recovery journals in private directories outside agent
workspaces. Read-only host observations require the assigned bridge; an agent
cannot submit them as trusted evidence. These commands do not add GitHub App
permissions or configure host protection.

Source preservation may take longer than the observation freshness window.
Complete it before final capture. Each selected probe/capture must satisfy the
reconnect plan's cutoff and be fresh at activation; replaying an old capture never
refreshes its timestamp. A changed context, missing pin or changed review blocks
activation instead of carrying forward an old success.

## Retries and a safe stopping point

Retain the original request key, unchanged JSON and private state directory for an
uncertain write. Status/history can recover a committed response. Do not create a
new operation just because the previous response was lost. A new expired-plan
replacement or a fresh inventory uses a new explicitly chosen intent/directory;
retain its predecessor as evidence.

An activation receipt does not grant merge permission, transfer old checks/policy,
or prove that the new binding is still active. Inspect current reconnect status.
Old event channels, node identities and task journals must not be bulk-resumed.
The same App/client/installation/repository identity is required; switching to a
different App is outside this protocol.

You can stop after preservation and keep GitHub publishing disabled. Registered source history can be downloaded through [source export](live-source-export.md)
and converted with [Git export](git-export.md). Unqueued proposals are not registered
source-history roots: preserve their backup and journals, and use the ordinary
[proposal projection/export](github-projection.md) workflow after controlled
reconnection. Do not claim that a base-history export includes every proposal.
Independent backups and ordinary Git remain the exit path; a successful local
review alone never authorizes a writer.
