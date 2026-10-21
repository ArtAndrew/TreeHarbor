# Agent command library

The Rust entry point `Metadata::execute_agent_command` accepts a credential and a
versioned JSON request. It currently supports repository information, workspace
requests/status/drain and operation status. This is a **library interface**;
there is no HTTP listener, CLI or MCP server yet, and neither product mode is released.

Requests are limited to 64 KiB. Unknown fields, duplicate fields, unsupported
versions and commands are rejected. UUIDs use lowercase hyphenated form. A tenant
and repository select the requested scope; permission comes from the credential.
Never put credentials, principal identities or trusted runtime observations in
agent-controlled JSON.

```json
{
  "version": 1,
  "tenant": "example-team",
  "repository": "00000000-0000-0000-0000-000000000001",
  "command": {"method": "repository_info"}
}
```

| Method | Additional command fields | Required permission | Current result |
|---|---|---|---|
| `repository_info` | None | `read` | Authority, state and epoch |
| `workspace_create` | `key`, `authority`, `epoch`, `workspace`, `base`, `profile_digest` | `workspace.write` | Logical `requested` workspace and durable preparation event |
| `workspace_status` | `workspace` | `read` | Caller-owned workspace state and generation |
| `workspace_delete` | `key`, `authority`, `epoch`, `workspace`, `generation` | `workspace.write` | Caller-owned workspace enters `draining`; files and pins are not deleted |
| `operation_status` | `key` | `read` | Caller-owned committed command receipt |

`base` must currently be a registered `native:sha256:<64 lowercase hex>` commit.
`profile_digest` is `sha256:<64 lowercase hex>` and binds the declared source profile;
profile registration and automatic worker preparation are not implemented. The
command currently requests the native filesystem format. A requested workspace
is not a prepared directory or a running sandbox. Missing operation status does
not prove an uncertain request failed: retry with the same key and identical input.

Success has `{"version":1,"result":...}`. Return only `CommandError::response()`
to a client on failure: `{"error":{"code":"..."}}`. Raw database errors and
credential values must not be returned or logged. No transport status code is
assigned by this library.

## Credentials and trusted controllers

Bootstrap administration uses a trusted database connection to provision a
principal, replace its repository permissions and issue an opaque token. Tokens
contain 32 random bytes; PostgreSQL stores a SHA-256 fingerprint. Their lifetime
is explicitly chosen from 1 second to 90 days. Issuance returns the credential
once. Rotation uses a new token followed by revocation of the old token; these
are security lifetimes, not licensing limits.

Human/agent identities may receive workspace/change/queue-request permissions.
Runtime control, validation reporting and host publication belong to separate
node, validator and bridge identities respectively. A role does not implicitly
grant a permission. A token is limited to one repository and a subset of the
principal's current grant. Some permission classes reserve future operations;
possessing them does not mean those product features are implemented.

Authenticated writes check the binding inside the mutation transaction, before
replay and again at the final authorization decision. Revocation and grant
changes wait for previously admitted writes before returning success. Expired
credentials fail the final check and roll back effects, receipts, pins and events.
Retries using a replacement token for the same principal can recover an existing
receipt; revoked credentials cannot recover it. These checks do not terminate
already running code or erase data previously returned to a client.

Node observations bind the node identity to its workspace assignment; agent-run
ownership remains with the workspace owner. Actual runtime observation, sandbox
isolation, credential-file management, secure network transport and service-wide
rate limits still need integration. Administrative and `CommandContext::trusted`
methods are for trusted in-process code only and must never be routed from agent
requests.
