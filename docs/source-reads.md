# Read native baseline files

After [importing source](native-import.md) and creating a logical workspace, its
owner can list directories and download files from that workspace's exact native
base. These commands do not read an agent's working directory, materialize a
workspace, launch a runtime or mark files ready for execution.

Use a credential with `read`, owned by the principal that created the workspace.
Supply its current generation, repository authority and epoch. They are explicit
fences: a request for a previous generation or authority epoch fails instead of
silently reading a different context. Requested, ready and in-use workspaces may
serve their baseline; draining, deleted and legacy unresolved workspaces cannot.
The [external API context](external-api-runs.md) uses the distinct
`source_ready` state and serves its exact baseline without materialized files.
This path is available from 0.1.4-alpha.1 and requires schema 032.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-list --workspace "$WORKSPACE" --generation 1 \
  --authority standalone --epoch 1 --path src --limit 128

# Omit --path to list the root. If next_after_hex is non-null, request the next
# page with the same context/path and --after-hex set to that value.

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-file --workspace "$WORKSPACE" --generation 1 \
  --authority standalone --epoch 1 --path src/main.rs \
  --output "$TH_PRIVATE/download-main.rs"
```

The output parent must already be private and owned by the CLI's OS user. Download
verifies the native blob digest and response context before publishing a new
private regular file with file/directory fsync. Existing destinations are never
replaced, including a destination created concurrently. HTTP/context/digest errors
do not publish a file. A local write or fsync error may leave a published file;
inspect that destination before retrying, since publication may have succeeded.
The output mode is 0600; the original mode is reported in JSON, not
automatically applied. A symlink returns its link bytes in a regular file; neither
server nor client follows it or creates a symlink. Paths beneath a symlink are
not traversed. Output is source data, not permission to execute code.

Each response identifies the workspace, generation, authority, epoch, native base,
snapshot and source profile. Directory entries contain `name_hex`, `mode` and
`object`. Names and cursors use lowercase hex of raw name bytes, preserving names
that cannot be represented as UTF-8 on filesystems that support them. Mode strings
are `040000` (directory), `100644` (regular file), `100755` (executable) and `120000`
(link). Listing is nonrecursive and sorted by raw name bytes. `next_after_hex` is
the last returned name when more entries remain, otherwise null. Keep the same
directory and context when following it; a cursor is not an access capability.

A source profile is a delivery selection, not a confidentiality boundary. An
explicit baseline read outside that selection is allowed within the owned
workspace's repository. It does not expand a prepared directory or change the
workspace generation/profile. Credentials and workspace ownership supply access
control. There is no public arbitrary-digest read route.

## HTTP protocol, version 1

`POST /v1/source/read` accepts JSON, a Bearer credential and the local service's
fixed `Host: treeharbor.local`. Query strings, browser Origin headers, ambiguous
headers and unsupported methods are rejected by the shared transport boundary.
The private Unix socket transport is still the only supported deployment; remote
TLS and MCP remain pending.

```json
{
  "version": 1,
  "tenant": "example",
  "repository": "11111111-1111-4111-8111-111111111111",
  "workspace": "22222222-2222-4222-8222-222222222222",
  "generation": 7,
  "authority": "standalone",
  "epoch": 3,
  "operation": {"kind": "file", "path_hex": "7372632f6d61696e2e7273"}
}
```

This [request vector](../crates/service/tests/fixtures/source-read-v1.json) reads
`src/main.rs`. UUIDs use canonical lowercase spelling; paths use canonical
lowercase hex and the core path rules. Empty file paths, traversal components and
reserved metadata names fail. Unknown fields fail, including identity fields
such as `principal`. The token determines the principal.

For a directory page, replace `operation` with:

```json
{"kind":"list","path_hex":"737263","after_hex":null,"limit":128}
```

An empty `path_hex` selects the root only for a list request. `after_hex` is
optional/null or one valid encoded name; `limit` is required and must be 1–256.
The successful response is JSON `{ "version": 1, "result": { ... } }`; `result`
contains the context fields above plus `path_hex`, the directory's `object` ID,
`entries`, and `next_after_hex`.

A successful file response uses `application/octet-stream`. Its body is the
existing canonical **Blob frame**, including the 22-byte header, not just the raw
file content. It provides exactly one of each header below:

| Header | Value |
|---|---|
| `x-treeharbor-version` | `1` |
| `x-treeharbor-workspace` | Workspace UUID |
| `x-treeharbor-generation` | Requested generation |
| `x-treeharbor-authority`, `x-treeharbor-epoch` | Requested authority and epoch |
| `x-treeharbor-revision` | Native base revision, `native:sha256:…` |
| `x-treeharbor-snapshot` | Snapshot ID, `sha256:…` |
| `x-treeharbor-profile` | Delivery profile ID, `sha256:…` |
| `x-treeharbor-object` | Digest of the complete canonical blob frame |
| `x-treeharbor-mode` | `100644`, `100755` or `120000` |

The CLI validates these headers and the frame before exposing content. Listing
metadata and source identity rely on the authenticated server; the response is
not an independent signed proof of the complete repository history. All responses
are `Cache-Control: no-store`. Errors use JSON `{ "error": { "code": "…" } }`.

| Status/code | Meaning |
|---|---|
| 400 `invalid_request` | Invalid shape, version, path, cursor or limits |
| 401/403 | Invalid/revoked/expired credential or missing read permission |
| 404 `not_found` | Workspace not available to this principal/scope |
| 404 `source_path_not_found` | Path absent, or traversal would go through a file/link |
| 409 `repository_fenced` / `execution_state_changed` | Authority, epoch, generation or lifecycle no longer matches |
| 409 `source_path_type` | File requested as a directory or directory as a file |
| 409 `lease_expired` | Protected-read hold expired before further IO |
| 413 `source_read_limit` | Source traversal exceeds the per-request byte budget |
| 503 `source_read_busy` / `database_unavailable` | Retry later under the same context |
| 503 `storage_not_configured` | Enable the server's storage backend first |
| 5xx / connection failure | No successful source response; retry the read, preserving its context |

## Authorization, retention and limits

The server creates a protected root hold in the transaction that checks the
workspace owner, revision and current credential. It follows only verified typed
edges from that root. Each batch rechecks access; the final check runs before
returning either bytes or path-existence results. A revocation or workspace drain
committed before that final check rejects the buffered result. An already
authorized response can still be in flight when a later revocation succeeds;
that revocation cannot retract the response.

File descriptors are opened under the existing catalog/GC protocol and stay valid
across physical retirement. SQL transactions do not span file reading/hashing.
Normal completion, including a rejected final check, releases the temporary hold.
Cancellation/process loss leaves an expiring 120-second hold. The existing base
and workspace pins are separate; a read does not remove them.

Each request may verify at most 128 MiB of encoded source objects, including commit,
snapshot, intermediate directories and the target. A file payload is at most
64 MiB. Paths are bounded by the core's 4,096 bytes, 128 components and 255 bytes
per name. Listing pages contain at most 256 entries; JSON requests are bounded by
the shared command request limit. Request/connection deadlines also apply.

There are two source-read slots per service process, separate from upload slots.
A slot remains owned by blocking IO and the response bytes, including bytes still
queued to a slow receiver. Frames are buffered, and a wide directory still needs
decoding even for a small page. There is no persistent read cache, per-tenant
fairness, remote streaming, range download or automatic materialization yet.
These limits do not establish a performance advantage or a sandbox boundary.
