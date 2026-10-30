# Initialize local service files

Available in [0.1.9-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.9-alpha.1)
and subsequent packages. Earlier packages do not include `treeharbor init`.
The command creates private configuration and storage directories for
an operator-owned local service. It does not connect to PostgreSQL or provision
the database, repositories, credentials, GitHub App or execution environment.
Both repository authority modes use the same service configuration.

## Create a new instance directory

Choose a short, persistent absolute path with an existing owner-private parent.
Use the service owner account. The following example prepares a **new** parent
and a connection file for a local database using the operator's existing
authentication. Adjust the database/socket names to the database you provisioned:

```sh
TH_PARENT="$HOME/th-preview"
(
set -eu
umask 077
mkdir -m 700 "$TH_PARENT"
cat > "$TH_PARENT/database.url" <<'EOF'
host=/var/run/postgresql dbname=treeharbor
EOF

treeharbor init --directory "$TH_PARENT/service" \
  --database-url-file "$TH_PARENT/database.url"
)
```

If authentication requires a password, prepare the connection file in a protected
editor or secret provisioning system; do not put the password in command arguments,
shell history or a public example. The input is a UTF-8 PostgreSQL connection
string, not JSON, at most 8 KiB. It must be a private regular single-link file
under an owner-private parent. The connection must use one local Unix socket,
as required by the existing [service configuration](local-service.md).

The command creates:

| Path under the new directory | Purpose |
| --- | --- |
| `service.json` | Mode-0600 service configuration, including the connection string |
| `objects/` | Empty mode-0700 local object-storage directory |
| `private/` | Empty mode-0700 directory for operator credentials and request journals |
| `service.sock` | Configured socket path; no socket is created by `init` |

The instance directory itself is mode 0700. Schema defaults to `public`; use
`--schema NAME` for another valid namespace and `--backend LABEL` to change the
default local storage label `local`. The schema must be created explicitly if
absent. Normal service limits remain at their existing defaults. The generated
configuration may be edited privately before startup; serving still validates it.
Use a short path: the resolved socket pathname is limited to 100 bytes. Parent
ancestors must be controlled by trusted administrators; this is not isolation
from an adversary using the same OS account.

## Continue explicitly

Success prints JSON with `version: 1`, `scope: local-service-files`, `status: created`
and absolute `config`, `socket`, `storage` and `private_state` paths. It reports
`database_initialized`, `repositories_created`, `credentials_issued` and
`service_started` as false. It does not print the connection string or its password.
Success establishes file creation, not database availability or service readiness.

After explicitly provisioning the intended database/schema, inspect and migrate:

```sh
treeharbor doctor --config "$TH_PARENT/service/service.json"
# For a fresh unmigrated schema, doctor reports schema_unverified. After inspecting
# that result and confirming the intended database, apply migrations explicitly:
treeharbor admin --config "$TH_PARENT/service/service.json" migrate
treeharbor doctor --config "$TH_PARENT/service/service.json"
```

Run these next steps individually: the initial doctor failure is expected for a
fresh schema. Register the intended repository and separate scoped identities
using the [service guide](local-service.md#credentials-and-administration), then
start the service with `treeharbor serve --config "$TH_PARENT/service/service.json"`.
For GitHub PRs follow the [Connected setup](connected-pr-preview.md) and explicitly
choose Connected authority when registering the repository.

## Existing or interrupted output

Any existing destination is refused, including empty directories and symlinks.
Concurrent initializers cannot replace each other's configuration. Directory
entries and files are synchronized, with `service.json` published last. A crash
or IO error can leave partial output; even an error after configuration publication
does not authorize deleting or overwriting it. Inspect the directory and preserve
it. Use a new destination for a new attempt. There is no automatic cleanup or resume.

`init` is for a new local instance, not upgrade, restore or adopting an existing
database/store. Keep the private connection file protected; the generated config
also contains its connection credentials. Use the separate [backup procedure](backup-restore.md)
and preserve request journals when updating an existing deployment.
