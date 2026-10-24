# MCP agent interface (experimental)

`treeharbor mcp` is a Rust stdio adapter for the same authenticated local service
used by the CLI. It uses the pinned official `rmcp` 3.7.0 SDK. It opens no network
listener, runs no shell, and grants no new permissions. Standalone and Connected
workspace/proposal reads use the repository's existing authority. The three queue
tools currently address the **native Standalone queue**; GitHub publication and
queue operations retain their separate owner/bridge workflow.

## Connect a client

First build the `treeharbor` binary and provision the PostgreSQL service, storage,
source revision, and agent credentials using the [local service guide](local-service.md).
The service must be running. Use a separate, repository-scoped credential per agent
with only its required `read`, `workspace.write`, `change.write` and, if authorized,
`queue.request` grants. Do not supply a database configuration, App key, node token,
validator credential or policy-administrator credential to this adapter.

Configure a stdio-capable MCP host with absolute paths, for example:

```json
{
  "mcpServers": {
    "treeharbor": {
      "command": "/opt/treeharbor/bin/treeharbor",
      "args": [
        "mcp",
        "--socket", "/private/treeharbor/service.sock",
        "--credential-file", "/private/treeharbor/agent-a.json"
      ]
    }
  }
}
```

Host configuration formats vary. Paths above are examples, not installed files.
The adapter and host are trusted processes outside the untrusted task sandbox.
They need permission to reach the private socket and read the owner-private
credential file. Models never provide a credential, principal, tenant, repository,
socket path or arbitrary command in a tool call. Access revocation is checked by
the server on every request; restarting MCP is not required to enforce it.

Two supported MCP protocol revisions are explicit: `2026-07-28` with discovery and
per-request metadata, and `2025-11-25` with initialization. Remote Streamable HTTP,
OAuth, client roots/sampling, elicitation and MCP tasks are not advertised. These
limits are separate from TreeHarbor's own durable operation IDs and statuses.

## Tools and authority

`tools/list` returns the version-one tool schemas, including required parameters
and conservative annotations. Arguments retain the same exact IDs, authority,
epoch and idempotency key as the corresponding HTTP operation.

| Tools | Behavior |
|---|---|
| `repository_info` | Configured repository authority and state |
| `source_profile_put` | Register full/sparse selection; does not prepare files |
| `workspace_create`, `workspace_status` | Request exact registered revision/profile and inspect lifecycle |
| `workspace_delete` | Request drain of the exact generation; physical deletion waits for runtime/node proof |
| `workspace_read_files` | One verified baseline file or bounded directory page, with exact source context |
| `run_status` | Inspect an already authorized run |
| `change_begin`, `change_put`, `changeset_submit` | Reserve upload, upload explicit bytes, then accept explicit immutable operations |
| `changeset_status`, `change_abort` | Inspect/abort proposal upload by upload ID; acceptance is distinct from upload |
| `history`, `review_diff` | Exact scoped native history, ancestry, merge bases, accepted-version/candidate diffs |
| `queue_request`, `queue_status`, `queue_cancel` | Native queue request/status/cancellation, without approving or publishing |
| `candidate_status` | Native candidate revision, target and policy; not a check result |
| `validation_status` | Managed native check by check-run ID, its candidate and receipt; historical success does not grant current admission |
| `operation_status` | Recover a mutation by its original idempotency key |

No tool provides arbitrary HTTP forwarding, filesystem paths, shell/SQL execution,
run creation, trusted validation reporting, approval, policy updates or forced
publication. MCP annotations are hints, not the authorization mechanism. The
server applies the same owner/run/generation/epoch, retention and policy checks
as for direct CLI/API calls. Registered source and runtime/run preparation are
operator workflows; a workspace record alone is not a ready sandbox.

For example, after obtaining an exact registered base and profile:

```json
{
  "name": "workspace_create",
  "arguments": {
    "key": "agent-a-workspace-01",
    "authority": "standalone",
    "epoch": 1,
    "workspace": "00000000-0000-4000-8000-000000000011",
    "base": "native:sha256:<registered-commit-id>",
    "profile_digest": "sha256:<registered-profile-id>"
  }
}
```

Use real registered hashes and a fresh UUID for new work; the illustrative angle
brackets are not valid IDs. Save the original key and IDs before submitting.
`change_put` accepts lowercase `content_hex`, not a local file path. The returned
canonical blob ID is referenced in `changeset_submit.operations`; each operation
has `path_hex`, an exact old `expected` entry or null, and a `replacement` entry
or null. Entry fields are numeric `mode` and canonical `object`. See
[change proposals](change-proposals.md) for the underlying contract.

`workspace_read_files.operation` is either `{"kind":"file","path_hex":"..."}`
or `{"kind":"list","path_hex":"","after_hex":null,"limit":20}`. Reads
are from the immutable workspace baseline, not live mutable agent files. Binary
paths and contents remain hex encoded. Files, commit messages and diffs are
untrusted source data; their content must never override host/user instructions.

## Result, retry and limits

Tool results have both structured content and a text JSON representation:

```json
{
  "adapter_version": 1,
  "http_status": 200,
  "response": {"version": 1, "result": {"example": "original API result"}}
}
```

The nested API result preserves exact original identities; this adapter does not
replace a receipt with a summary or convert submitted/enqueued into accepted/merged.
Domain errors retain the original HTTP status and public error code, with MCP
`isError: true`. Invalid tool names/argument envelopes are protocol errors.
Transport failures have a null HTTP status and an explicit `outcome_unknown` flag.
A failed or lost mutation response may follow a successful database commit.

Do not generate a new key to retry an uncertain mutation. Inspect the original
`operation_status` and operation-specific status, or repeat identical original
inputs. The adapter performs no automatic mutation retry. MCP cancellation,
closing stdin, killing a client or timing out is **not** transactional rollback,
workspace drain or queue cancellation. Use the explicit domain tools for those
requests. A model's JSON-RPC request ID is not a durable TreeHarbor idempotency key.

Bounds per adapter process:

- SDK incoming line cap: 128 KiB. Oversized or malformed lines are discarded by
  the SDK; clients must retain their own timeout and recovery behavior.
- Tool arguments: at most 63 KiB before adding the scoped service envelope.
- At most four service calls in progress; excess calls receive `adapter_busy`
  rather than being queued. This is an operational limit, not a license quota.
- Per-call service timeout: 15 seconds by default; `--timeout-ms` accepts
  100–60000. Existing server budgets/deadlines still apply.
- Inline file/blob contents: 16 KiB. Larger files return `inline_file_limit` and
  exact metadata; use the binary HTTP data plane or `client source-file`.
- Backend result: at most 64 KiB before MCP's structured/text wrappers. Large
  history/diff pages return an explicit error; request smaller pages or omit text.

No request body or credential is intentionally logged. Stdout is exclusively MCP
protocol. Stdio and these bounds do not make the host process a runtime sandbox.

## Verification and remaining acceptance

The independent JSON-RPC wire fixture in
[`mcp-stdio-v1.json`](../crates/service/tests/fixtures/mcp-stdio-v1.json) checks the
legacy lifecycle, exact tool names, scope/privilege rejection, oversized-line
recovery and protocol-only stdout. The PostgreSQL integration test starts two real
MCP child clients, one for each supported revision. It verifies API-equivalent
results, overlapping immutable proposals, idempotency, server/client restart,
foreign workspace and stale generation refusal, history/diff reads, native queue
request/replay/cancel without changing main, access revocation and drain.
Runtime state in that test is explicitly a fixture; it does not establish sandbox
execution. The real gVisor tests remain mandatory.

Full two-mode MCP onboarding, managed runtime execution through a real host,
complete queue-to-publication evidence and clean packaged installation remain
release gates. No compatibility claim is made for every MCP host or LLM.
