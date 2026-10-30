# Cold backup format v1

Introduced in [0.1.8-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.8-alpha.1).

This is the private local artifact produced by `backup create`, not an agent
upload format or Git export. All members belong to the service owner and have no
group/other permissions. Treat the entire directory as source and credentials.
No archive extraction or automatic restore is performed by `backup verify`.

| Member | Meaning |
| --- | --- |
| `metadata.dump` | Actual PostgreSQL custom-format dump of the configured schema using the exported metadata snapshot |
| `objects/` | Complete configured local backend, including `backend-id`, live, in-progress and orphan files |
| `service-config.json` | Exact private configuration bytes read for this backup |
| `private-state/00`, `01`, … | Explicit `--state` inputs in argument order; each can be a file or directory |
| `pg-dump.stderr` | Diagnostic output; nonempty diagnostics prevent completion |
| `pgpass.empty` | Empty private password file used to prevent implicit use of a different default password file |
| `manifest.json` | Completion marker, published last using no-clobber file and directory synchronization |

Manifest fields, in serializer order: `version` (1), `format`
(`treeharbor-cold-backup`), `treeharbor_version`, `created_unix_seconds`, `schema`,
`schema_version`, `migration_digest` (lowercase SHA-256 hex), `postgres_version_num`,
`backend`, `backend_identity` (UUID), `inputs`, `inventory`. Unknown fields or
versions are refused by the current reader. `inputs` lists `source` absolute paths
and `target` relative paths; they are descriptive data, never commands.

`inventory` is ordered by relative UTF-8 path. The empty path is the backup root.
Each value has `mode` (numeric owner permission bits), `sha256` (lowercase hex for
regular file bytes, null for directories) and `bytes` (file length, zero for
directories). Empty files have the SHA-256 of zero bytes. Symlinks, hard-linked
regular files and special files are unsupported. The inventory includes all
members except `manifest.json` itself. Root and `private-state` use mode 0700;
copied private input modes are preserved.

The receipt's `manifest_sha256` hashes the exact UTF-8 manifest bytes; do not
reformat JSON after recording that digest. There is no JSON re-canonicalization
at verification time. The [manifest vector](../crates/service/tests/fixtures/cold-backup-v1.json)
and [digest](../crates/service/tests/fixtures/cold-backup-v1.sha256) fix encoding;
they are a synthetic codec example, not a restorable PostgreSQL backup.
Actual pg_dump/pg_restore acceptance is separate.

A directory without the completion marker is incomplete. File/metadata errors
can occur after publication, so a failed command must not trigger deletion,
replacement or automatic resume of its output. Keep the directory and inspect
it. Successful verification proves retained inventory bytes against the supplied
digest, not all-input selection, the original operator's stopped-writer assertion,
successful SQL restore or safe runtime/GitHub activation.

Format changes require an explicit new version and reader compatibility policy.
Backup creation does not migrate the database, revoke credentials, reset checks,
change authority or discard recovery journals. See [backup and restore](backup-restore.md).
