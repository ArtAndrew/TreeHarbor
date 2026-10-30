# Backup and restore (development)

A supported operational restore procedure remains a release gate. The current
restore drill tests the durable source service and explicit replacement-node
execution; it does not certify resuming old workers or publishing to GitHub after
a disaster. Keep independent backups while
evaluating development builds.

## Experimental cold backup commands — 0.1.8-alpha.1

The Rust CLI can create and verify a backup of one local source service, its
unmodified TreeHarbor PostgreSQL schema and one bound local object backend.
These commands are included in
[0.1.8-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.8-alpha.1),
not in earlier packages. They do not stop workers, discover other hosts, fence
old credentials, reconcile GitHub or authorize restoration.

Stop **all** incoming work, services, publishers, nodes, runtimes, GC and
administrator writers first. Keep them stopped until the command finishes.
Use the service owner account and a trusted `pg_dump` matching the server major
version. The service configuration already supplies the private Unix database
connection; no database password belongs on the command line.

```sh
# Example paths: replace them with this deployment's actual paths.
# The backup parent must already exist, be owner-private (0700), and be outside
# every input. The destination itself must NOT exist.
treeharbor backup create \
  --config /srv/treeharbor/config/service.json \
  --pg-dump /usr/lib/postgresql/18/bin/pg_dump \
  --output /srv/treeharbor-backups/snapshot-001 \
  --state /srv/treeharbor/private/bridge-journals \
  --state /srv/treeharbor/private/client-journals \
  --state /srv/treeharbor/private/node-journals \
  --writers-stopped
```

`--writers-stopped` is your acknowledgement of a required cold boundary, not
proof that TreeHarbor stopped anything. The command takes the usual service socket
lock and non-waiting SHARE locks on existing schema tables. These exclude a
cooperating service and conflicting table writes. They do not stop filesystem-only
writers, sequence changes, newly created schema objects or external GitHub actions.
No online backup or point-in-time recovery is supported by this command.

Service configuration and the entire object root are included automatically.
Repeat `--state` for every required private file or directory outside those roots:
publication/import journals, controller request identities, node journals and
deployment configuration/credentials. Inputs cannot overlap. The tool cannot
determine whether an omitted path is needed. Do not include live mounts, sockets,
workspaces or whole parent directories that overlap the automatically copied
configuration/store. Files and directories must be owner-private; symlinks,
hard-linked regular files, special files and non-UTF-8 names are refused.
Unsubmitted edits in materialized workspaces are **not** protected by this source
service backup. Capture and accept them, or export/save them separately, before
the cold boundary. Copying node journals does not copy their referenced volumes.
Secrets in the backup remain plaintext: protect/encrypt it with your backup system.

The new directory receives a PostgreSQL custom-format dump, the complete object
root, copied configuration, enumerated state and private dump diagnostics. Live
object locators, lengths and SHA-256 values must agree with the locked catalog.
Files and directories are synchronized before `manifest.json` is written last.
On failure, preserve the incomplete output for inspection; retry to a **new**
destination. The command never overwrites or automatically resumes a backup.

Successful output includes `complete`, `manifest_sha256`, `entries`, `bytes` and
`restore_authorized: false`. Retain the digest separately in your trusted backup
record. Verify it again after transfer:

```sh
: "${TH_BACKUP_SHA256:?Set this to the digest saved from the successful backup receipt}"
treeharbor backup verify \
  --backup /srv/treeharbor-backups/snapshot-001 \
  --manifest-sha256 "$TH_BACKUP_SHA256"
```

Verification compares the manifest's exact bytes to that digest and then the
entire inventory, including modes, directories, sizes and file hashes. Missing,
extra or changed files fail. A matching digest establishes integrity relative to
your retained record; it is not a signature, encryption or permission to activate
restored credentials. `verify` never connects to PostgreSQL or executes dump SQL.

Default copy budgets are 64 GiB of file content, 100,000 entries and one hour;
`--max-bytes`, `--max-entries` and `--timeout-seconds` can adjust them. The manifest
has a separate size limit, so the same payload budgets work for verification.
Depth is capped at 64, relative paths at
4096 UTF-8 bytes and the manifest at 64 MiB. These are operational limits, not
license restrictions or replacements for filesystem quotas; external dump output
is checked periodically and an OS filesystem operation may block.

The [v1 format](cold-backup-format.md) records the source-to-backup path mapping.
PostgreSQL roles, tablespaces, extensions/dependencies outside the selected schema,
OS/runtime configuration not enumerated above and external GitHub state are not
captured. Use the restoration boundary below. Never point an ordinary live server
at the backup directory or resume publishers merely because verification passed.

## Restore staging — 0.1.10-alpha.1

The new [restore-stage command](restore-stage.md) replaces manual database/file
restoration with a verified copy into a new isolated database and audited revocation
of old service tokens. It is available in
[0.1.10-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.10-alpha.1)
and never authorizes activation. Installed runtime acceptance passed on
[Linux x86-64](https://github.com/ArtAndrew/TreeHarbor/actions/runs/41097779078/job/124207992670)
and [ARM64](https://github.com/ArtAndrew/TreeHarbor/actions/runs/41097779078/job/124207992386).

## Reference backup boundary

The initial single-host deployment uses a cold backup:

1. Stop incoming work, bridge/publisher processes, workers and the service. Confirm
   every task runtime has stopped. Keep GC and administrator writers stopped.
2. Use `pg_dump` matching the PostgreSQL server version to save the complete
   TreeHarbor schema, including constraints, sequences, audit and recovery records.
3. While writers remain stopped, preserve the complete configured object-store
   directory, private bridge/import journals, node state and private configuration.
   Preserve ownership and permissions. Treat the backup as containing credentials
   and customer source; it must remain private and protected.
4. Keep the database dump and filesystem copy together with a recorded snapshot
   time and verified checksums. The recovery point is that completed backup.

Copying metadata and objects independently while work or GC continues does not
establish a consistent backup. An exported Git bundle is useful for exit but does
not preserve proposal receipts, policies, credentials or runtime recovery journals.
No online backup, PITR interval or zero-data-loss disaster recovery is promised.

## Restore verification

Restore into isolated infrastructure with publishing and worker execution disabled.
Restore the dump into a fresh database with matching PostgreSQL, then restore the
matching objects and private state. Keep the original writers stopped. Verify the
expected schema/migration digest and compare retained metadata and object hashes.
Use the service to export the expected revisions and full history; inspect the
export independently with ordinary Git. Check accepted proposal versions as well
as the published branch. Missing or corrupt objects must produce failure.

Before enabling Connected writes, reconcile published branches, PRs, queue entries
and check-publication attempts with GitHub. Do not discard old journals or infer
that a timed-out request never reached the provider. Verify/fence old worker and
node identities before allowing execution against restored state. These operational
resume steps remain incomplete; a passing source-export drill does not authorize
restarting publishers or workers.

## Automated drill

The [managed native acceptance workload](../scripts/test-native-checks.py) invokes
[a cold restore drill](../scripts/cold_restore.py) after real agent changes have
been checked, reviewed and published through the native engine. In a disposable
Linux/PostgreSQL/gVisor environment it:

- stops writers and uses actual `pg_dump` / `pg_restore`;
- starting with 0.1.8-alpha.1, invokes installed `backup create` / `verify`
  with explicitly selected credentials, import/native request journals and node
  state; verifies their inventory and refuses destination reuse before restore;
- starting with 0.1.10-alpha.1, calls actual `backup restore-stage`, compares all
  retained tables/sequences except the explicitly asserted token revocations and
  appended audit, and verifies that the original database remains unchanged;
- starts the real service with copied objects and the original object path absent;
- exports published history into ordinary Git and reads accepted agent changes;
- removes/corrupts a restored object, requires export failure, then verifies recovery
  after restoring its exact bytes;
- retains a requested but unexecuted check without converting it to validation;
- starting with 0.1.10-alpha.1, requires staging to revoke all old tokens, then
  deliberately issues selected new operator/agent credentials in the disposable
  harness; it keeps old node credentials revoked, restarts the service and requires
  an explicit node authentication failure and refusal to execute the old check with Engine available;
- registers a fresh node with new local state and volume, runs a fresh approved
  check through gVisor, publishes its exact native candidate, verifies the result
  in ordinary Git and cleans up the new check workspace.

This is a shared storage and deliberate replacement-node recovery test with
synthetic agent programs. The operator keeps all original writers stopped and
checks old token revocation before allowing a new node to work. It does
not discover or fence other infrastructure automatically. It exercises no live
GitHub reconciliation and resumes no old task or copied node journal. Run it only
through the disposable runtime CI harness; it is not an operator backup utility.

The previously released drill, including operator backup creation and replacement-node
execution, passed with installed static binaries on
[Linux x86-64](https://github.com/ArtAndrew/TreeHarbor/actions/runs/41052176043/job/124050740230)
and [ARM64](https://github.com/ArtAndrew/TreeHarbor/actions/runs/41052176043/job/124050740228)
on 2026-10-30. These results cover the scoped recovery scenario above, not automatic
restore activation or live GitHub recovery.
