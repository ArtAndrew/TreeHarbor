# Diagnose local service setup

Available in the signed [0.1.3-alpha.1 preview](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.3-alpha.1).
Follow that release's source, signature and checksum instructions before running
the package. Earlier preview packages do not include this command.

Before starting the source service, run as the trusted operator:

```sh
treeharbor doctor --config /private/operator/service.json
```

Doctor reads the same private configuration as `serve`. It prints JSON to stdout
and exits with status 0 when its listed prerequisites pass, or status 1 when any
fail. A configuration error stops dependent checks. Other failures are collected
so that one run can identify both an unsafe directory and a database problem.
Connection strings, credentials, configured paths, schema names and raw underlying
errors are omitted from the report. Keep the configuration outside agent files.

| Check | What it establishes | Typical repair |
| --- | --- | --- |
| `configuration` | Existing service private-file, JSON and connection/limit rules | Use an owner-private directory and regular single-link mode-0600 config; configure local PostgreSQL through a Unix socket |
| `socket_directory` | Existing parent directory meets private-directory rules | Create the intended directory explicitly; review ownership and permissions |
| `socket_path` | Path is absent or an owner-private Unix socket | Inspect an unexpected file or symlink; do not delete a running service's socket |
| `storage_directory` | Configured source directory meets private-directory rules | Configure an existing private durable source directory; metadata-only setup is insufficient for workspaces and PRs |
| `database` | Connection, schema resolution and `fsync=on` using the normal service path | Check PostgreSQL, local socket access, authentication and schema `USAGE`; create the intended namespace explicitly if absent |
| `schema` | Schema version and migration digest match the binary | Check permissions and installed version; back up, then explicitly apply a supported migration using `admin migrate` |

The response has `version: 1`,
`scope: "local-source-service-prerequisites"`, a `passed` boolean, a `checks`
array with `name`, `passed`, `code`, `advice`, and a `not_checked` list. For
example, `database/schema_unavailable` identifies an absent or inaccessible
namespace; `schema/schema_unverified` means the schema check could not pass,
including missing tables, a version/digest mismatch or insufficient privileges.
It is not an instruction to migrate blindly.

The command does not migrate, create a namespace, change permissions, initialize
storage, bind/delete a socket or start workers. Database session settings are
applied only to its temporary connection. Database connection and schema-check
stages are separately bounded by the configuration's request timeout.

**Passing this check does not establish a working agent deployment.** It does not
write or fsync probe data, measure free space, verify storage identity binding or
object integrity, check the socket lock or running service, inspect runtime
isolation, or validate Git/GitHub permissions and backup/restore. These are listed
as unchecked in the output. Permissions and database state can also change after
inspection. Continue with the actual [PR Preview setup](connected-pr-preview.md)
and its runtime, credentials and installed workflow verification.
