# Review an immutable change

The development service and CLI show changes between the base and result of an
exact accepted ChangeSet version, or between two registered native revisions.
This reads retained server objects; it needs neither a live workspace nor Git.
Only repository `read` permission is required, including when a reviewer reads
another agent's proposal in that repository. Path-level confidentiality is not
implemented.

## CLI

Start the [local service](local-service.md) with object storage, then use actual
credential paths, ChangeSet IDs, native revisions and the current repository epoch:

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  diff --authority standalone --epoch 1 \
  change --changeset "$TH_CHANGE" --version 1

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  diff --authority standalone --epoch 1 --limit 20 \
  revisions --before "$TH_BEFORE" --after "$TH_AFTER"

# Continue the same comparison using next_after from the previous response.
# The cursor belongs before the revisions subcommand; its --after is a revision.
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  diff --authority standalone --epoch 1 --limit 20 --after "$TH_CURSOR" \
  revisions --before "$TH_BEFORE" --after "$TH_AFTER"

treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  diff --authority standalone --epoch 1 --metadata-only \
  change --changeset "$TH_CHANGE" --version 1
```

`repository-info` supplies the authority and epoch. Use the repository's actual
authority; `connected` is accepted for registered native inputs but does not
implement the GitHub adapter. A revision must be a complete `native:sha256:…`
identifier registered in this repository. A change requires its exact positive
version; a newer version or a [rebase](change-rebase.md) never silently replaces it.

Results appear under `result`. `identity` identifies either the immutable
ChangeSet/version/original run/base revision, or the exact before/after revision
pair. `before_snapshot` and `after_snapshot` identify the compared trees.
`total_changes` counts rows in the complete bounded comparison; `next_after` is
the last row's path in lowercase hex if more rows remain, otherwise `null`.
Keep both inputs unchanged while paging. A nonmember cursor is an error; a cursor
at the last changed path produces an empty final page.

## Entries and text

Each row has `path_hex`, `kind`, `before`, `after` and `text`. Paths are encoded
losslessly from repository bytes, sorted lexicographically by those bytes. Before
and after values are either `null` or `{ "mode": NUMBER, "object": "sha256:…" }`.
Modes are the canonical numeric directory/file/executable/symlink modes. Changes
are `add`, `delete`, `modify`, `mode` (regular/executable transition) or `type`.
Mode/type changes can also change content: always inspect both object IDs.

Directory creation, deletion and type changes include the directory row and its
changed descendants. Empty-directory changes are preserved. Equal subtrees are
skipped. Directory metadata rows are not equivalent to Git's leaf-only path list.
There is no rename detection, structural language analysis or semantic judgement.

`text.status` is one of:

- `patch`: a unified patch with three context lines and fixed `before`/`after`
  headers. The real path is only in `path_hex`, so a hostile filename cannot inject
  patch headers. Symlink patches describe link-target bytes, not followed files.
- `unchanged`: eligible text bytes are identical, including a mode-only change or
  an empty file being added/deleted. The metadata still describes that change.
- `omitted`: `reason` is `not_requested`, `directory`, `binary` (invalid UTF-8 or
  NUL), `size_limit` or `work_limit`. This is explicit missing text, not a claim
  that content is unchanged. Object IDs and metadata remain available.

The display profile is `treeharbor-review-diff-v1/diffy-0.5.2/context-3`. Text
patches omit mode changes and directory operations and are not a complete native
ChangeSet or an import format. This endpoint never applies patches, moves refs,
approves candidates or carries validation to another revision. A proposal diff
does not by itself show its later merged integration candidate; reviews and
checks still bind to that exact candidate via [native admission](native-integration.md).

## HTTP v1

`POST /v1/review/diff`, the usual Bearer header, `Host: treeharbor.local` and
`Content-Type: application/json`. The CLI reads scope from its credential file.

```json
{
  "version": 1,
  "tenant": "example-team",
  "repository": "11111111-1111-4111-8111-111111111111",
  "authority": "standalone",
  "epoch": 1,
  "query": {
    "kind": "change",
    "changeset": "22222222-2222-4222-8222-222222222222",
    "version": 1
  },
  "limit": 20,
  "after": null,
  "text": true
}
```

Alternatively `query` is `{ "kind": "revisions", "before": "native:sha256:…",
"after": "native:sha256:…" }`, with full real IDs. Unknown fields, duplicate
fields and invalid identifiers are rejected. Caller-supplied snapshot digests,
storage locators and pin names are not accepted. Successful responses use
`{ "version": 1, "result": { … } }` and `Cache-Control: no-store`.
Error responses use `{ "error": { "code": "…" } }`; CLI exits nonzero and writes
the server error JSON to stdout, while local transport errors use stderr.

## Limits and recovery

Each request computes the complete bounded changed-path comparison before
selecting a page. Limits are 20,000 directory visits, 200,000 directory entries,
32 MiB of traversed path bytes, 128 MiB of verified source frames, 16,000 changed
rows and 4,096 bytes per path. Pages contain 1–100 rows (CLI default 20). These
limits and existing service timeouts protect this development endpoint; there is
no persisted diff index and each page repeats bounded traversal.

Text is loaded only for the selected page, at most 64 KiB and 4,000 lines per
side. The aggregate line-count product budget is 16 million per page. A row that
cannot fit the remaining text-work budget has `work_limit`; smaller pages may
include its patch. Serialized JSON is capped at 1 MiB **including escaping**.
An oversized response fails entirely with HTTP 413 `review_limit`; retry smaller
pages or `--metadata-only`. Changed-path traversal limits require narrower source
changes, not smaller pages. Limits never return a silently incomplete path list.
HTTP 400 `review_cursor` means the cursor is not a changed path for these inputs.
Other access, context, read and busy errors follow [source reads](source-reads.md).

The service verifies repository membership, current permission, active state and
authority/epoch at acquisition, before source batches and before success. Both
source roots receive scoped GC holds atomically. Ordinary success/error closes
all holds, including after revocation; cancellation or process loss leaves holds
that expire after 120 seconds. A stale epoch or revoked credential requires an
explicit fresh authorized request. Already delivered bytes cannot be recalled.
No source objects, ChangeSet versions or validation records are mutated.

## Verification

The [independent vector generator](../scripts/generate-review-wire-vector.py)
uses Python's standard library for canonical blob hashes and unified diff; the
fixture pins wire/profile/output bytes. Actual PostgreSQL/CLI tests compare file
paths with Git, apply returned text patches, exercise binary/mode/symlink/empty
directory changes, restart the server and remove original inputs before paging.
Linux additionally tests non-UTF8 filenames. Tests also cover retained old
versions after rebase/drain, repository boundaries, revoked access, changed epoch,
expired holds, GC protection and oversized escaped responses with complete
paginated recovery. Linux gVisor CI reads actual agent proposals before review.
These checks do not establish performance superiority or a complete release.
