# Native history queries

The development service and CLI can read commits, page through history, test
ancestry and find all best common ancestors. They read verified durable native
objects through PostgreSQL-backed scoped read holds. No Git process, GitHub
credential or workspace checkout is needed. This is a read interface; native
rebase, complete repository lifecycle and a production release remain unfinished.

## CLI

Start the [local service](local-service.md) with configured object storage. Import
a source base or [publish a native candidate](native-integration.md), and use its
exact `native:sha256:…` revision. The credential needs only repository `read`.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  history --authority standalone --epoch 1 \
  log --revision "$TH_HEAD" --limit 100

# Continue with next_after from the previous page and the same pinned head.
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  history --authority standalone --epoch 1 \
  log --revision "$TH_HEAD" --limit 100 --after "$TH_CURSOR"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  history --authority standalone --epoch 1 \
  show --revision "$TH_HEAD" --commit "$TH_COMMIT"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  history --authority standalone --epoch 1 \
  is-ancestor --ancestor "$TH_LEFT" --head "$TH_RIGHT"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  history --authority standalone --epoch 1 \
  merge-bases --left "$TH_LEFT" --right "$TH_RIGHT"
```

Set the variables to real paths and native revision strings. `repository-info`
returns the current authority/epoch. These reads also accept `connected` for a
registered native base in a Connected repository; that does not implement the
GitHub adapter. Use the actual repository authority, not a mode chosen per query.

`log` defaults to complete ancestry, with children before all their parents. When
several nodes are ready, ascending raw native IDs break ties. Timestamps and parent
list order do not determine this display order. Every returned commit still has
its original **ordered** parent list. `--first-parent` explicitly selects only the
first-parent chain for display; ancestry and merge-base queries always use every
parent. Neither view is a substitute for full-graph GC, source export or backup.

The JSON result is under `result.history`. A log page contains `commits`, `order`,
`total_commits` for the selected view, and `next_after`. `null` means the end; an
empty page after the final commit is also valid. Keep the exact root and view when
paging. The cursor is the last returned native revision and must belong to that
view. No moving ref is silently resolved between pages.

`is-ancestor` returns `is_ancestor: true` or `false`; both are successful requests
with CLI exit code zero. Read the Boolean, not a Git-style exit-code convention.
`merge-bases` returns all best common ancestors in ascending native-ID order. A
disconnected history produces an empty array, and criss-cross histories can
produce multiple results. Do not choose one arbitrarily for a later merge.

## Access and consistency

Each input root (`revision`, or both roots of a two-root query) must be a registered
native base in this repository, with its scoped durable pin. Imported bases and
published integration commits qualify. An arbitrary digest or an object in another
repository of the same tenant does not. An ancestor need not be separately
registered for `show` or as a pagination cursor: it must be reached from the
explicit authorized root. For a two-root query, register both roots first; it
does not accept an unrelated digest as a second operand.

The service checks read permission, active repository state, authority and epoch
before opening holds, before each object batch and again before success. Every
opened hold is closed on ordinary success/error, including a failed second-root
lookup. Cancellation/process loss leaves a hold expiring after 120 seconds.
Holds retain the source graph against GC while verified commit-parent edges are
read. Source files, directory contents and raw Git blobs are not loaded by these
queries. Metadata is never inferred from timestamps or filesystem ordering.

Commit fields include revision, ordered parents, snapshot, changes, author name
and email, timestamp/offset, message and optional `git_origin` (claimed Git
revision and raw-commit Blob ID). Author text and origin metadata are stored data,
not proof of identity, signature validity, review, CI success or merge permission.
History queries do not validate the raw Git origin against a Git graph; the
[Git interop boundary](git-origin-v2.md) performs that check.

Credentials and transport use the existing [private Unix HTTP interface](local-service.md).
Revocation cannot recall response bytes already authorized. No source metadata or
credentials are placed in routine logs. Remote TLS and MCP remain separate work.

## HTTP v1

`POST /v1/history/read`, `Content-Type: application/json`, the usual Bearer header
and `Host: treeharbor.local`. Unknown fields and noncanonical IDs are rejected.

```json
{
  "version": 1,
  "tenant": "example-team",
  "repository": "11111111-1111-4111-8111-111111111111",
  "authority": "standalone",
  "epoch": 1,
  "query": {
    "kind": "log",
    "revision": "native:sha256:COMMIT_DIGEST",
    "limit": 100,
    "after": null,
    "first_parent": false
  }
}
```

Replace the digest placeholder; the CLI fills tenant/repository from credentials.
Supported query objects:

| `kind` | Required fields | Optional fields |
|---|---|---|
| `log` | `revision`, `limit` (1–256), `first_parent` | `after` (native revision or null) |
| `show` | `revision` (registered anchor), `commit` (reachable target) | None |
| `is_ancestor` | `ancestor`, `head` (both registered) | None |
| `merge_bases` | `left`, `right` (both registered) | None |

Success is `{"version":1,"result":{"authority":…, "epoch":…, "roots":[…],
"history":…}}`; `roots` preserves the operand order, including equal operands.
JSON field ordering is not significant. The independent
[request](../crates/service/tests/fixtures/history-read-v1.json) and
[response](../crates/service/tests/fixtures/history-response-v1.json) vectors come
from [a Python reference](../scripts/generate-history-wire-vector.py) using the
existing independent canonical commit fixture, without invoking Rust or Git.
No object-format or PostgreSQL schema upgrade is required.

## Limits and failures

One request permits at most 10,000 unique commits across its roots, 64 MiB of
canonical commit frames and 80,000 parent edges. JSON output is capped at 1 MiB,
including escaping. A page has at most 256 commits; very large metadata can still
exceed the response limit. Use [source export](live-source-export.md) to obtain
the complete canonical source graph subject to that command's separate bounds.
These are operating limits, not license quotas.

This first implementation loads the complete commit ancestry for each request,
including each page and first-parent view. It then computes the answer in bounded
memory; the persistent source of truth remains the object store/PostgreSQL.
No ancestry index or retained page cache is implemented. Graph memory, repeated
reads, object verification, JSON encoding and cleanup must be counted in future
benchmarks. No performance advantage is claimed.

Limits, missing/corrupt history or expired holds return errors; they never turn
into `false`, an empty base set or a silently truncated complete history. An
invalid cursor is 400, a missing anchor/target is 404, revoked/missing permissions
use 401/403, stale authority/epoch is 409, and traversal/output bounds are 413.
The existing server request deadline (15 seconds by default, up to 60 seconds)
and connection deadline still apply. Source-read capacity is held through blocking
verification/computation and queued response bytes. Resource quotas and sandboxing
remain external; these reads do not execute code from commit messages or files.
