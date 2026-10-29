# Local server and CLI

This development transport works on Linux/macOS. It exposes the existing
authenticated commands over HTTP/1.1 on an owner-private Unix socket. It does not
bind a TCP port. The [Connected PR Preview](connected-pr-preview.md) provides
signed Linux packages and one operator-assisted workflow; the full Connected and
Standalone releases remain incomplete. A remote command TLS connector is not
implemented. The experimental
[MCP stdio adapter](mcp.md) uses this same scoped local service. A separate
experimental [managed-run worker](managed-runs.md) supports Linux/gVisor execution.

For an installed package, use its resolved `treeharbor` executable in place of
`./target/debug/treeharbor` below; compiling from source is optional. The example
below creates a metadata-only **Standalone** repository. For the Connected PR path,
create it with `--authority connected` and follow the [setup path](connected-pr-preview.md).
An authority mismatch is not fixed by changing a helper config.

## Build and exercise repository access

Prerequisites: the pinned Rust toolchain and an existing PostgreSQL 18 database
with `fsync=on`, reachable through a trusted local Unix socket. The database role
must own the configured schema and be allowed to create its objects. Use a separate
development database. Adjust the PostgreSQL socket path/user below to your machine;
do not put passwords on the command line. Connection credentials, if needed, belong
only in the protected server configuration file.

```sh
cargo build -p treeharbor-service --locked
umask 077
TH_STATE=$(mktemp -d /tmp/treeharbor.XXXXXX)
TH_REPOSITORY=00000000-0000-0000-0000-000000000001
cat > "$TH_STATE/config.json" <<EOF
{
  "database_url": "host=/var/run/postgresql dbname=treeharbor",
  "socket": "$TH_STATE/service.sock"
}
EOF

./target/debug/treeharbor admin --config "$TH_STATE/config.json" migrate
./target/debug/treeharbor admin --config "$TH_STATE/config.json" repository-create \
  --tenant example-team --repository "$TH_REPOSITORY" --authority standalone
./target/debug/treeharbor admin --config "$TH_STATE/config.json" principal-create \
  --tenant example-team --principal agent-a --kind agent
./target/debug/treeharbor admin --config "$TH_STATE/config.json" access-set \
  --tenant example-team --repository "$TH_REPOSITORY" --principal agent-a \
  --permission read --permission workspace.write
./target/debug/treeharbor admin --config "$TH_STATE/config.json" token-issue \
  --tenant example-team --repository "$TH_REPOSITORY" --principal agent-a \
  --permission read --permission workspace.write --ttl-seconds 3600 \
  --output "$TH_STATE/agent.json"
./target/debug/treeharbor serve --config "$TH_STATE/config.json"
```

The last command runs in the foreground. In another terminal, set `TH_STATE` to the
same printed socket's parent directory, then run:

```sh
./target/debug/treeharbor client --socket "$TH_STATE/service.sock" \
  --credential-file "$TH_STATE/agent.json" repository-info
```

The response contains authority, state and epoch. The agent client needs the scoped
credential file and socket, not the server's database configuration. This example
provisions metadata only: it does not import files, create native history or start
an agent. `standalone` selects repository authority, not a claim of S2 readiness.
The short temporary directory avoids Unix socket pathname limits. For persistent
use, choose a short owner-private directory that survives reboots.

## Credentials and administration

The development-source [`doctor` command](doctor.md) checks local configuration,
directory rules and PostgreSQL/schema prerequisites before startup. It does not
change setup or certify runtime/GitHub readiness, and is not in the published
0.1.2-alpha.1 package.

`admin` is a trusted local operator interface using database privileges directly;
it is not available through HTTP. It provides migrate, repository-create,
principal-create, access-set, token-issue, token-revoke, principal-disable and node-register.
Use `--help` for exact arguments. Principal roles and permissions are separate;
for example, an agent cannot be granted validator or host-publisher power.
GitHub connection verification uses a Bridge's separate `host.read` grant and
credential; it does not require `host.publish`. A Human's `repository.write`
configures or revokes the [repository binding](github-connection.md).
Experimental [GitHub queue delivery](github-queues.md) requires an explicit Human
`host.queue` grant and an assigned Bridge's `host.merge` grant. Existing access is
not expanded automatically during schema upgrades.
Experimental [event recovery](github-event-recovery.md) uses a Human's
`repository.write` request and the assigned Bridge's separate `host.recover`
grant. Read, publish and merge credentials do not acquire recovery permission.
Experimental [managed check publication](github-check-publications.md) uses a
Human's `host.queue` request and an assigned Bridge's explicit `host.check` grant.
It derives the result from protected execution; an agent cannot supply success.
Schema28 does not add this permission to any existing credential.

`token-issue` writes a versioned JSON credential to a new mode-0600 file. It never
overwrites an existing file or prints the secret. Stdout reports the token ID and
destination. Configuration and credentials must be regular files, owned by the
current OS user, without group/other access or hard links; their parent directory
must be owner-private and not a symlink. The socket and its lock are also private.
Parent ancestors and the configured PostgreSQL socket location must be controlled
by trusted administrators. These checks are not a sandbox against the same OS uid.

Rotate by issuing a new file, switching the client and revoking the old ID:

```sh
./target/debug/treeharbor admin --config "$TH_STATE/config.json" token-revoke \
  --tenant example-team --repository "$TH_REPOSITORY" --token-id TOKEN_UUID
```

The default lifetime is one hour; the maximum is 90 days. To revoke all repository
permissions, use `access-set --revoke-all` for the principal. These are access
controls, not usage/licensing limits. The audit operator label records the local
OS uid; it does not prove a human identity.

Issuing a database token and saving its file are not one distributed transaction.
If file publication fails, the CLI attempts revocation and reports the token ID
and outcome. An uncertain failure or process loss may require administrator
reconciliation using the token audit. Do not treat a failed command as proof that
no token was issued. Secrets are not promised to be zeroized from process memory.

## Workspace commands and retry

The CLI supports profile-put/profile-get, workspace-create, workspace-status,
workspace-delete and operation-status. Register full selection or explicit paths:

```sh
./target/debug/treeharbor client --socket "$TH_STATE/service.sock" \
  --credential-file "$TH_STATE/agent.json" profile-put \
  --key full-selection-v1 --authority standalone --epoch 1 --full
./target/debug/treeharbor client --socket "$TH_STATE/service.sock" \
  --credential-file "$TH_STATE/agent.json" profile-put \
  --key src-selection-v1 --authority standalone --epoch 1 --path src --path tests
```

Each response returns a computed `profile_digest`. Use it with `profile-get` or
workspace-create. [Profiles](source-profile-v1.md) are immutable and repository-scoped;
they select delivery, not permissions. Creation also requires an already registered
native commit. [Native graph uploads](source-uploads.md) now register a base through
authenticated binary HTTP/CLI commands when a storage backend is configured.
For ordinary files, [native directory import](native-import.md) now captures
explicit paths and resumes uploads using a durable local pack and journal.
The [local node](local-node.md) now materializes and acknowledges native workspace
files with durable placement and recovery. [Managed runs](managed-runs.md) add
operator-approved bounded runtime execution. [Git bundle import](git-import.md)
adds complete pinned history. Automatic scheduling remains pending.
The [source-export command](live-source-export.md) downloads a registered native
revision and its complete source history with read permission only.
[History queries](native-history.md) add commit display, paginated logs, ancestry
and all best merge bases through the same read-only boundary.
After creation, [source-list and source-file](source-reads.md) read the exact
baseline using the owner credential, workspace generation and repository epoch.

```sh
./target/debug/treeharbor client --socket "$TH_STATE/service.sock" \
  --credential-file "$TH_STATE/agent.json" workspace-create \
  --key prepare-agent-a-1 --authority standalone --epoch 1 \
  --workspace WORKSPACE_UUID --base native:sha256:COMMIT_DIGEST \
  --profile-digest sha256:PROFILE_DIGEST
./target/debug/treeharbor client --socket "$TH_STATE/service.sock" \
  --credential-file "$TH_STATE/agent.json" operation-status --key prepare-agent-a-1
```

Replace the uppercase placeholders with real IDs. A successful create means
`requested` with a durable preparation event. Delete begins `draining`; it does
not yet remove files or release persistent pins. No command here marks READY,
accepts validation or authorizes publication.

A timeout, disconnected socket or server error may leave the outcome unknown.
Retry mutations with the **same key and identical arguments**, including workspace
ID, base, authority and epoch. A missing operation status is not proof of failure.
Successful responses are JSON on stdout; errors produce a nonzero exit status.
Revoked credentials cannot read an old receipt. A replacement credential for the
same principal can, while permission and ownership remain valid.

## HTTP and operating bounds

`POST /v1/commands` accepts the [versioned command envelope](agent-commands.md),
`Content-Type: application/json`, exactly one `Authorization: Bearer …` header,
and `Host: treeharbor.local`. Credentials do not belong in command JSON. Browser
Origin headers are rejected. `GET /healthz` reports process liveness only; it is
not a database readiness or durability check. Responses use `Cache-Control: no-store`.

| Limit | Default / behavior |
|---|---|
| Command body | 64 KiB, including streamed/chunked bodies |
| Headers | 32 fields, 8 KiB, five-second read deadline |
| Body read | Five seconds, also bounded by request deadline |
| Concurrent connections | 64; excess connections closed; configurable 1–1,024 |
| Database pool | 8; configurable 1–64; bounded checkout/create/recycle waits |
| Request deadline | 15 seconds; configurable 100–60,000 milliseconds |
| Connection lifetime | One request; bounded response wait; no keep-alive |
| Client JSON response limit | 1 MiB + 64 KiB; source files and archives have separate binary bounds |

Optional config keys are `schema` (default `public`), `pool_size`,
`max_connections` and `request_timeout_ms`. Schema names are restricted identifiers;
the schema must already exist (see [creation before migration](connected-pr-preview.md#3-prepare-the-service-storage-and-authority)). Serving requires the exact migration checksum/version;
startup never migrates implicitly. Database TCP/fallback hosts and custom connection
options are rejected. Remote deployment needs a future verified transport.

From 0.1.1-alpha.1, the configured search path must resolve
to that exact schema before migration or startup. An unavailable schema produces
a bounded hint to create it or grant the database role USAGE; connection strings
and schema names are not printed. Creating the namespace does not install the
metadata schema: explicitly run `admin migrate` before serving. These changes
do not alter the already published alpha.1 package.

HTTP mappings: malformed requests 400, missing/invalid credentials 401, forbidden
scope/permission 403, absent resources 404, conflicting/fenced commands 409, oversized
body 413, unsupported content type 415, unavailable pool 503, request deadline 504.
Database internals are not returned. Per-tenant rate limits and production admission
scheduling remain future work; connection bounds alone do not provide them.

Stop with Ctrl-C or SIGTERM. The server stops accepting connections, gives active
requests a bounded grace period and removes only its own socket inode. A process
lock prevents a second server taking over the same socket. After a crash, startup
reclaims a stale socket only after confirming no live listener while holding that
lock. The lock file stays in place and is reusable. Pool recycling completes any
queued transaction rollback before lending the connection again, or discards a
failed connection.

Do not run untrusted agent code as the account that owns server configuration or
database access. This transport is a local operator/development interface. Runtime
isolation, resource limits for executed code, secret/network policy and multi-host
deployment remain separate release work.
