# Agent API over HTTPS

Use the agent HTTPS profile to connect an agent controller on another Unix host.
It is available in the signed [0.1.17-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.17-alpha.1)
Linux packages.

The HTTPS listener is opt-in and runs in the same server process as the Unix
socket. Both use the same repository authorization, source store, PostgreSQL
transactions and version 1 request bodies. The HTTPS listener accepts external
task, source read and ChangeSet operations. Local operators retain GitHub
publication, managed runtime, validation and integration authority.

## Configure the server

Set up the [local service](local-service.md), migrate its database, import the
source and register its profile. Provision a TLS certificate chain with a leaf
that covers the DNS name or IP address agents will use. Store its PEM files and
configuration in an operator-owned directory with mode `0700`. Files must be
regular, owner-only and have a single link. Use absolute PEM paths.

For a server with an assigned private address, use a private `https.json` like
the following.

```json
{
  "version": 1,
  "listen": "10.20.0.12:8443",
  "server_name": "source.example.internal",
  "certificate": "/srv/treeharbor/private/server-chain.pem",
  "private_key": "/srv/treeharbor/private/server-key.pem",
  "max_connections": 16
}
```

Set `listen` to an explicit assigned address or loopback. Wildcards and multicast
addresses are rejected. The listener expects `Host` to match `server_name` and
the bound port (omit the default port 443). It uses HTTP/1.1 over TLS 1.3.
Configuration and certificates are loaded at startup. Restart the service with
the normal operator procedure after changing them. Keep the endpoint on a
controlled network with your normal access rules.

```sh
treeharbor serve --config /srv/treeharbor/private/service.json \
  --https-config /srv/treeharbor/private/https.json
```

The ready message includes the actual `https` address. The existing `socket`
field identifies the local interface. Omit `--https-config` for a local deployment.

## Connect an agent controller

Give the controller the trust CA PEM and a repository-scoped credential through
your private provisioning process. Use `read`, `workspace.write` and
`change.write` for the external task lifecycle. Set both files to mode `0600`
in an owner-private directory. The CLI verifies the server chain and
the name in the URL before sending the bearer token. It uses the configured CA
and a direct TLS connection.

```sh
th() {
  treeharbor client --https https://source.example.internal:8443 \
    --ca-cert /private/controller/treeharbor-ca.pem \
    --credential-file /private/controller/agent.json "$@"
}
th repository-info
th api-run-new --state /private/controller/task.json --authority connected \
  --epoch 1 --base "$BASE" --profile-digest "$SOURCE_PROFILE" \
  --agent my-controller/1 --task fix-parser
```

Use the actual repository mode, epoch, revision and profile supplied by the
operator. Follow [external API tasks](external-api-runs.md) for source reads,
ChangeSet submission and closing the task. Keep the same request keys, IDs and
state file after a lost connection. The original receipt can be returned after
the task closes. Query `api-run-status` for its current state.

| Remote operation | CLI |
| --- | --- |
| Repository and registered profile | `repository-info`, `profile-get` |
| External task | `api-run-new`, `api-run-create`, `api-run-status`, `api-run-close` |
| Exact source and inspection | `source-list`, `source-file`, `history`, `diff` |
| Proposal upload and acceptance | `change-begin`, `change-put`, `change-submit`, `change-status`, `change-abort` |
| Retrieve a previous receipt | `operation-status` |

For a direct HTTP client, use the same version 1 JSON and binary frames as the
local API. Remote requests use the configured origin as `Host`, origin-form
paths and the bearer authorization header. `GET /healthz` reports listener
availability. The additional network boundary rejects local-only command methods
and routes before dispatch. All allowed operations still enforce the existing
scope, permissions, revocation and workspace fences.

The listener has an independent connection limit (default 16, range 1–128), a
five-second TLS handshake deadline, and the existing HTTP header, body and request
limits. Each request opens a new connection. Local and HTTPS clients share the
underlying service and database pool.
The remote controller supplies its own execution and isolation. The external
task record identifies the proposal and exact base. It carries no runtime or
validation observation.
