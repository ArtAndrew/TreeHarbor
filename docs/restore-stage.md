# Restore staging (unreleased)

`backup restore-stage` prepares a verified cold backup in isolated infrastructure.
It creates a **new database**, copies the backup to a new private directory, checks
schema/storage identities and live objects, and revokes the restored service tokens.
It does **not** start TreeHarbor, issue new credentials, fence the old deployment or
permit publishing to GitHub. This command is not included in 0.1.9-alpha.1 or earlier
packages. Safe activation remains a separate release gate.

## Prerequisites

Use a trusted backup made by [backup create](backup-restore.md), its independently
retained manifest SHA-256, and the same TreeHarbor migration chain. Keep the backup
unchanged during staging. A PostgreSQL dump contains executable SQL: matching a
digest does not make someone else's backup safe to execute.

Provide an isolated local PostgreSQL server of the same major version as the
backup, with `fsync=on`, and a matching trusted `pg_restore`. The connection role
needs `CREATEDB`. The command uses an unused UUID database name and template0;
it never empties an existing database. Roles, extensions and other dependencies
outside the captured schema are not restored. Keep all services, workers and
publishers off this target and block its publishing network access externally.
The acknowledgement flag records this requirement; it does not implement isolation.

Use the service owner account. The connection file must be owner-private and
contain a local Unix-socket PostgreSQL connection string. The output parent must
already exist with mode 0700; the destination must not exist and must not overlap
with the backup or its recorded source paths. Protect all output as plaintext
source code and secrets. Do not use an original service configuration from the
backup to start anything: it still identifies the old deployment.

```sh
: "${TH_BACKUP_SHA256:?Set the separately retained backup manifest digest}"
treeharbor backup restore-stage \
  --backup /srv/treeharbor-backups/snapshot-001 \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  --directory /srv/treeharbor-recovery/stage-001 \
  --database-url-file /srv/treeharbor-recovery/isolated-database.url \
  --pg-restore /usr/lib/postgresql/18/bin/pg_restore \
  --isolated-target
```

## Result and failure handling

The new directory contains:

- `backup/`: the copied backup, verified before its dump is executed. Configuration,
  journals and credential files are retained as evidence; their old tokens are
  revoked in the newly restored database, not in the original deployment.
- `target.json`: private target database/schema identity and admin connection.
  Written before database creation, so it also identifies an uncertain outcome.
- `pg-restore.stdout`, `pg-restore.stderr` and an empty private password file.
- `staged.json`: completion receipt, written and synchronized last, only after
  restoration, object verification and audited token revocation succeed.

The stdout receipt contains `status: staged`, `database`, `schema`,
`manifest_sha256`, `revoked_tokens`, and explicit false flags for `service_started`,
`activation_authorized` and `old_infrastructure_fenced`. It contains no connection
string or bearer secret. No runnable service configuration is generated.

Restoration uses one PostgreSQL restore transaction and omits source ownership/ACLs.
A subsequent transaction revokes all previously unrevoked TreeHarbor tokens and
appends an audit entry for each. Accepted changes, history, job states, generations
and existing audit records remain intact. Revocation does not cancel jobs or turn
an unexecuted check into validation. Public database CONNECT permission is revoked;
this does not exclude the database owner or privileged administrators.

On failure, leave the target isolated. An interrupted restore may have completed
SQL without completing token revocation. Absence of `staged.json` means staging is
incomplete; even its presence is not activation permission. Inspect the private
diagnostics and `target.json`. The command never automatically drops a database,
deletes partial output, reuses a destination or resumes an uncertain restore.
An operator can explicitly remove the identified disposable target after checking
its identity, or retain it for investigation. Retry only to a new directory.

Default budgets match backup: 64 GiB payload and 100,000 entries per verification/
copy pass, plus the bounded manifest; a shared one-hour deadline. Override with
`--max-bytes`, `--max-entries`, `--timeout-seconds`. This is not a PostgreSQL storage
quota: database expansion requires external disk limits. Filesystem calls can block
and individual database statements have their own timeouts.

## Before activation

A separate procedure must fence the original database, nodes and runtimes, review
unfinished tasks and issue selected new credentials. For Connected it must also
reconcile GitHub refs, PRs, checks and uncertain publication attempts before enabling
any publisher. Do not resume copied worker journals or grant fresh publishing
credentials as an automatic consequence of staging. An old backup is a recovery
point, not proof that subsequent changes or external effects never occurred.

The current disposable recovery drill deliberately performs its own explicit
inspection/new-node setup after staging. That test does not supply an operator
activation command or certify live GitHub disaster recovery.
