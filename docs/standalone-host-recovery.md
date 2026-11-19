# Restore a Standalone repository on another Linux host

Use a cold backup to recover native history, accepted changes, checks and
policies. A Git bundle lets you independently compare the source history.
The signed Linux package supplies the TreeHarbor commands in this procedure.

## On the source

For a disposable repository, the [two-agent example](../examples/standalone/README.md#keep-a-native-backup)
can produce both outputs. Keep its printed manifest SHA-256, tenant and
repository ID separately. The example runs at authority epoch 1.

For an existing service, export all branches with the
[live Git export command](git-export.md#export-all-branches-from-a-live-standalone-repository).
Stop every writer, select the operator and node state that belongs to the
deployment, then [create and verify the cold backup](backup-restore.md).
Record the authority epoch and branch tips at the checkpoint. Keep the
backup private and fence the original database, services and workers before
any replacement can accept writes.

## On the recovery host

Use a fresh PostgreSQL cluster of the same major version, with `fsync` enabled
and a local role allowed to create databases. Use a trusted, matching
`pg_restore`. Transfer the complete backup and check its manifest against the
digest retained on the source. The database dump will execute SQL during restore.

The commands below use an existing, owner-only `TH_RECOVERY` directory, the
absolute backup path in `TH_BACKUP`, and the recorded digest in `TH_MANIFEST`.
`TH_ADMIN_URL` is the path of a mode-0600 file containing the target's Unix
socket PostgreSQL connection string. Set `TH_PG_RESTORE` to the absolute
path of the target's `pg_restore`.

```sh
umask 077
TH_STAGE="$TH_RECOVERY/stage"
treeharbor backup verify --backup "$TH_BACKUP" --manifest-sha256 "$TH_MANIFEST"
treeharbor backup restore-stage --backup "$TH_BACKUP" --manifest-sha256 "$TH_MANIFEST" \
  --directory "$TH_STAGE" --database-url-file "$TH_ADMIN_URL" \
  --pg-restore "$TH_PG_RESTORE" --isolated-target
treeharbor backup restore-quarantine --stage "$TH_STAGE" \
  --manifest-sha256 "$TH_MANIFEST" --writers-stopped
treeharbor backup restore-configure --stage "$TH_STAGE" --manifest-sha256 "$TH_MANIFEST" \
  --writers-stopped --old-infrastructure-fenced
```

Staging creates a new database and revokes the old tokens there. Quarantine
disables old nodes. Configuration prepares a new service and a separate live
object store. The operator is responsible for fencing the source and for
deciding when the new service can start. [Staging](restore-stage.md) ·
[Quarantine](restore-quarantine.md) · [Configuration and reader credentials](restore-configure.md#start-with-a-new-reader).

Issue a fresh repository-scoped reader on the target using the recovered tenant
and repository ID. Start `treeharbor serve --config "$TH_STAGE/active/service.json"`
under your supervisor. Set `TH_READER` to the reader's private credential
file and `TH_EPOCH` to the recorded epoch. Then export from the running service.

```sh
treeharbor client --socket "$TH_STAGE/active/service.sock" --credential-file "$TH_READER" repository-info
treeharbor client --socket "$TH_STAGE/active/service.sock" --credential-file "$TH_READER" \
  git-export --epoch "$TH_EPOCH" --output "$TH_RECOVERY/git"
git clone --bare "$TH_RECOVERY/git/history.bundle" "$TH_RECOVERY/review.git"
git --git-dir="$TH_RECOVERY/review.git" fsck --full --strict
git --git-dir="$TH_RECOVERY/review.git" rev-list --parents --all
```

Compare the `refs` and `commits` maps in both Git export manifests. For
change and check evidence, read the saved ChangeSet versions and check IDs
through TreeHarbor. The validator that ran a check can read its execution
record with a fresh token scoped to `read`. Provision a replacement worker
separately when you are ready to resume agent execution.

## Recorded recovery

On November 19, 2026, two Linux ARM64 VMs on the same physical machine ran the
signed 0.1.35 package. The source VM was off before activation on the target.
The restored service returned all three branch tips, seven reachable commits,
four accepted change versions and three managed check records. A new branch
created on the target survived a service restart and an identical retry.

On this 490,466-byte checkpoint, the first authenticated repository read took
1.224 s from the start of verification on the provisioned target. The full
source comparison and Git export finished at 1.696 s. VM preparation, transfer
and PostgreSQL startup took place before timing started. The exercise covered
a stopped Standalone service on two VMs. Recovery across physical sites and
Connected reconciliation need their own trials. [Recorded inputs and results](evidence/standalone-host-recovery-20261119.json).
