# ADR 011: Agent interfaces and advanced feature priorities

Date: 2026-10-21. Status: accepted direction and implementation plan; features
below are not claimed shipped. Rust, standard ELv2 and both authority modes remain.

## Decision

Add an MCP adapter to the first usable agent integration, alongside the CLI and
HTTP API. All three call the same authenticated service commands. Prioritize the
working two-agent journey, installation and runtime boundary. Keep FUSE, structural
merge and database distribution behind explicit experiments rather than making
them prerequisites for the first usable pilot. The full Connected/Standalone
release goal remains intact; this ADR does not declare either mode complete.

| Capability | Current evidence | Planned action |
|---|---|---|
| Agent-facing API/CLI/MCP | Internal native intake exists; none of these external interfaces is implemented | Shared service layer, scoped API/CLI, then a Rust MCP adapter in the first usable agent package |
| Copy-on-write and rollback | Copy/reflink and private Git state are specified; no materializer exists yet | Immutable source cache, private writable copies, capability-tested reflink with copy fallback; explicit checkpoints after stopping writers |
| Runtime isolation | External runtime boundary is specified; no integration exists yet | Bind a real isolated runtime, enforce process/network/resource/credential boundaries and test them before untrusted execution |
| Lazy VFS | Explicitly outside the initial pilot | Measure sparse/full materialization first; evaluate a Linux-only optional VFS when workload evidence supports it |
| Structural merge | No merge engine or structural resolver yet | Correct native merge/rebase first; evaluate syntax-aware resolution as a versioned, opt-in candidate producer |
| Distributed metadata | Single PostgreSQL with tenant/repository scopes | Preserve explicit boundaries; benchmark contention and design placement/migration before adding distribution |

## MCP contract

MCP is an additional client protocol, not an authorization or merge-policy engine.
The intended command is `treeharbor mcp` (planned, not currently runnable). Start
with stdio so an MCP-capable host can launch the Rust adapter without a public
listening port. Use the official `rmcp` SDK, pin a published release after checking
its dependency/license and interoperability requirements. Do not depend on an
unversioned main branch. Negotiate supported protocol versions rather than claiming
compatibility with every model or agent host.

The adapter uses the same source/change service as HTTP and CLI. Local credentials
come from protected host configuration; models do not supply tenant identities,
principals or privileged credentials as tool arguments. An authenticated remote
Streamable HTTP endpoint can follow with its own OAuth/audience/Origin tests;
stdio support alone is not evidence of remote authentication.

Initial proposed tool surface, finalized with versioned JSON schemas during implementation:

| Tool | Purpose and authority |
|---|---|
| `repository_info` | Read authorized repository authority, features and revision identities |
| `workspace_create` | Request a workspace from an exact revision and declared source profile |
| `workspace_read_files` | Bounded/paginated source reads bound to the workspace snapshot; binary paths supported |
| `changeset_submit` | Submit explicit operations/blob references for the caller's authorized run and generation; stable idempotency key |
| `changeset_status` | Retrieve exact version, base, run, result snapshot and lifecycle evidence |
| `validation_status` | Read required checks and their exact candidate/context; never assert that CI passed |
| `queue_request` | Request inclusion; the policy engine or GitHub still decides admission and publication |
| `operation_status` | Observe a long-running operation by its durable ID, including retry/conflict/fenced outcomes |
| `workspace_delete` | Request drain; no immediate deletion beneath a running agent |

History/path-history and merge-base tools follow when the corresponding authorized
service operations exist. No arbitrary shell, SQL, force-publish, mark-runtime-ready
or trusted-validation-registration tool is exposed to an agent. CI collectors and
runtime controllers use separate authenticated roles.

Structured outputs include exact identities and machine-readable error codes.
Long work returns an operation handle. File data and diffs are untrusted content,
not instructions. Large blobs stay in the data plane instead of expanding model
context unnecessarily. Requests, quotas, audit records, idempotency and policy
evaluation are identical across CLI, HTTP and MCP.

Acceptance: run the two-agent workflow through two actual MCP-capable clients;
compare results with direct API calls; verify identical-request replay, stale-run
rejection, tenant access, size limits, cancellation semantics and denial of agent
attempts to forge validation or bypass the queue. No command is advertised before
it works against the actual service.

## Files, isolation and rollback

Copy-on-write isolates file modifications only under the chosen mount/filesystem
contract. It does not isolate processes, stop CPU exhaustion, revoke secrets or
undo external network/database effects. Exact validation predicates protect merge
decisions, not operating-system isolation.

TreeHarbor will keep immutable cached source separate from writable workspaces;
writable hardlinks are prohibited. Checkpoint/reset first stops or freezes writers,
pins the checkpoint, restores a private tree, increments the workspace generation
and invalidates observations for the old generation. Runtime confirmation is
required before cleanup. Use an existing external sandbox/VM runtime; do not build
a new hypervisor. Millisecond rollback is a benchmark hypothesis, not a guarantee.

## FUSE experiment gate

Mount latency and time to useful agent work are different metrics. Lazy reads can
move cost to metadata traversal, first reads, mmap or a build that touches most of
the tree. Test cold/warm caches, read amplification, offline/error behavior,
watchers, rename/unlink, symlinks, copy-up, daemon restart and resource consumption.
Keep the ordinary filesystem backend available.

Start a VFS implementation only after a pilot spends material time in source
materialization despite sparse selection, batching, caching and reflink. A proposed
adoption gate is at least 30% lower p95 time to useful work on that pilot workload,
without correctness regressions or worse full cost; this is an unmeasured target.
Neither 10 ms mounts nor 100 GB trees becoming available in 10 ms is promised.

## Structural merge experiment gate

A syntax tree is not a proof of semantic compatibility. Successful parsing can
coexist with changed initialization order, duplicated symbols, incompatible APIs,
macro/configuration effects or failing tests. Scion's article does not establish
the claim that its merge engine is limited to line-based merging.

First establish native merge/rebase correctness and a conflict corpus. Evaluate
existing syntax-aware tools such as Mergiraf, including licensing before any
dependency or distribution decision; no such component is currently adopted.
A resolver must output a new candidate/version with base, inputs, engine version
and diagnostics. It must never carry old green checks onto changed output. Parsing,
typechecking, CI and review requirements still apply; inconclusive cases remain
conflicts. Measure manual interventions and incorrect automatic resolutions against
the same baseline corpus, not merely the count of conflict markers removed.

## Database placement

The current database is **not shard-ready**. Repository operations are scoped by
tenant/repository, but the object catalog and deduplication are tenant-wide; pins
bridge those domains. The outbox has a database-wide bigint identity. Repository
row locks, advisory migration locks, triggers, foreign keys and SKIP LOCKED semantics
must be tested on any alternative database. PostgreSQL wire compatibility is not
proof of equivalent transaction behavior.

A possible first distribution boundary is an entire tenant's metadata/catalog,
keeping proposal acceptance and pins on one PostgreSQL authority. Sharding separate
repositories inside one tenant would require redesigning shared-object retention
or its transaction protocol. Large single-tenant deployments need a separate plan.
Placement requires routing, fencing epochs, online migration, backup/restore and
failure tests; no distributed database or transparent YDB/Yugabyte migration is
promised for the first release. Optimize and measure the current repository lock
before choosing a replacement.

## Sources checked on 2026-10-21

- [Anthropic Scion](https://www.anthropic.com/research/redesigning-source-control),
  published 2026-10-13, updated 2026-10-15: report of an internal deployed system,
  not a public source release. Describes source-control APIs and sparse/virtualized
  checkouts; it does not support an assertion of a uniquely line-based merge engine.
- [MCP tools specification 2026-07-28](https://modelcontextprotocol.io/specification/2026-07-28/server/tools)
  and [authorization](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization):
  published protocol specifications, not implemented TreeHarbor features.
- [Official Rust MCP SDK](https://github.com/modelcontextprotocol/rust-sdk): public
  implementation with published releases; dependency version is not yet selected.
- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/): published parser library;
  produces concrete syntax trees and can represent erroneous input.
- [Mergiraf limitations](https://mergiraf.org/conflicts.html): published syntax-aware
  merge tool and documented limits; not a semantic correctness guarantee.
- [EdenFS overview](https://github.com/facebook/sapling/blob/main/eden/fs/docs/Overview.md):
  public implementation/documentation of lazy source files, including Linux FUSE.
- [Citus 13 distribution design](https://docs.citusdata.com/en/stable/sharding/data_modeling.html):
  released-product documentation explaining distribution columns and colocation.

Installation and MCP can lower adoption friction; demand, retention and measured
total-cost advantage still determine whether the product is successful. None of
these features proves superiority to Scion or product-market fit by itself.
