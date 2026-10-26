# Backup and restore (development)

A supported operational restore procedure remains a release gate. The current
restore drill tests the durable source service and explicit replacement-node
execution; it does not certify resuming old workers or publishing to GitHub after
a disaster. Keep independent backups while
evaluating development builds.

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
- compares all retained tables and sequence values in a fresh database;
- starts the real service with copied objects and the original object path absent;
- exports published history into ordinary Git and reads accepted agent changes;
- removes/corrupts a restored object, requires export failure, then verifies recovery
  after restoring its exact bytes;
- retains a requested but unexecuted check without converting it to validation;
- explicitly revokes old node credentials, restarts the restored service and requires
  an explicit node authentication failure and refusal to execute the old check with Engine available;
- registers a fresh node with new local state and volume, runs a fresh approved
  check through gVisor, publishes its exact native candidate, verifies the result
  in ordinary Git and cleans up the new check workspace.

This is a shared storage and deliberate replacement-node recovery test with
synthetic agent programs. The operator keeps all original writers stopped and
explicitly revokes old node tokens before allowing a new node to work. It does
not discover or fence other infrastructure automatically. It exercises no live
GitHub reconciliation and resumes no old task or copied node journal. Run it only
through the disposable runtime CI harness; it is not an operator backup utility.

The drill passed with installed static binaries on
[Linux x86-64](https://github.com/ArtAndrew/TreeHarbor/actions/runs/40317254806/job/121553337315)
and [ARM64](https://github.com/ArtAndrew/TreeHarbor/actions/runs/40317254806/job/121553336736)
on 2026-10-26. These earlier job results cover source recovery, before the additional
replacement-node scenario; the full
candidate run still needed its Linux lint correction before merge.
