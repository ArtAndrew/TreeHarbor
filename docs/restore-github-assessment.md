# Compare GitHub with retained restore state

Development command after 0.1.11. It is not in the published 0.1.11 package.
Use this after [local restore configuration](restore-configure.md), with old
publishers still fenced. It extends [host inventory](restore-github-inventory.md)
by comparing live observations with the exact operations retained for one binding.
It does not resolve discrepancies, reconnect the service or replay any write.

You need the independently retained backup manifest SHA-256, the old binding UUID
and current repository epoch. Use the original private GitHub App configuration
and a **fresh TreeHarbor `host.read` credential for that binding's assigned bridge**.
The App configuration must match the stored App/client/repository identities.
Old backup credentials remain revoked; this command never issues replacements.
An ordinary agent, read-only human or a different bridge cannot obtain this context.

```sh
umask 077
: "${TH_BACKUP_SHA256:?Set the retained backup manifest digest}"
: "${TH_BINDING:?Set the old revoked binding UUID}"
: "${TH_EPOCH:?Set the current repository epoch}"
cargo run -p treeharbor-service --locked -- github restore-assess \
  --config /absolute/private/treeharbor/github-app.json \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-bridge.json \
  --binding "$TH_BINDING" --epoch "$TH_EPOCH" \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  > /absolute/private/treeharbor/restore-assessment.json
```

Use a new private output filename and check the exit status. Output includes
repository metadata and retained PR text. A file created by shell redirection
does not establish success. A failed token revocation produces a nonzero exit,
even if an observation report was printed; inspect `inventory.token_revoked`.

The server requires a current Connected repository, revoked bindings, and the
durable quarantine receipt for this manifest **and this database**. A receipt
copied from a previous recovery is insufficient. Under a repository lock it reads
retained branch publications, PR publications, queue/check operations, removals
from the queue and webhook recovery requests/attempts. It changes none of their
attempts, observations or source pins.
The CLI collects a fresh host inventory, then rereads the context. A changed
context causes failure instead of returning a comparison against stale input.

## Reading findings

| Finding | Meaning |
| --- | --- |
| `branch_absent` | The named branch was absent during inventory; this does not prove it was never published. |
| `head_matches_prepared` / `head_matches_expected` | Current branch content matches a retained OID; this alone does not prove which operation produced it. |
| `head_differs` | Current content differs from the retained before/after mapping. |
| `known_pull_code_matches` | Saved number/node, repository, head commit and base branch name match. The base commit may have moved; combined code, PR body, marker and review/check state remain unverified. |
| `known_pull_absent` / `known_pull_identity_differs` / `known_pull_code_differs` | Inspect the missing or changed identity/code before deciding how to resolve it. |
| `pull_identity_unresolved` | No saved PR number exists. Matching repo/head/base PRs are candidates only, including closed PRs; none is adopted automatically. |
| `external_inspection_required` | A retained queue/check/removal or webhook recovery operation still needs external inspection. |

`unmapped_to_selected_binding` lists service-named branches/PRs without a mapping
in this context. They may belong to another historical binding; the context also
lists other binding UUIDs. Do not delete or automatically adopt these objects.
The report always has `reconciliation_complete: false` and
`publication_authorized: false`, even when no differences are found.

## Version 2 context and limits

The authenticated GitHub request is `{"action":"restore_context","epoch":1,
"binding":"<UUID>","manifest_sha256":"<64 lowercase hex characters>"}`.
It returns a version-2 context containing tenant/repository/epoch/binding, the
stored binding specification, manifest/database identity, quarantine digest,
other bindings and retained effects. Each effect identifies its family, operation,
available source/PR identities, attempt/cancellation/state and `row_sha256`.

Version 2 also requires `dequeues`, `recoveries`, `recovery_pages` and
`redeliveries`. Dequeues refer to their original queue. A redelivery is identified
by both recovery UUID and provider attempt ID; a delivery GUID alone is not unique.
The full row hashes cover details, dispatches and reports. Recovery entries also
hash their event channel. Cancelled, expired and never-dispatched entries remain
visible. A cancellation or missing attempt is not proof that no remote call happened
since the backup. All queries are scoped to the selected tenant/repository/binding.

Version 1 clients must be upgraded together with the server. Missing version-2
lists are rejected, not treated as empty. Old decision receipts remain immutable
and readable, but their old context digest cannot authorize a fresh decision or
source verification: review the current context and record a successor. The
[version-2 fixture](../crates/metadata/tests/fixtures/restore-context-v2.json) fixes
these identities and their canonical digest. Assessment reports also use version 2.

At most 256 bindings, 256 effects per original family and 256 entries in each new
list across the selected binding are accepted, with a 512 KiB
serialized-context limit. HTTP 409 / `restore_context_limit` means the context
cannot be assessed by this bounded command; no partial result is accepted.
The independent host inventory limits still apply. These are operational limits.

Digests use SHA-256 over `treeharbor-restore-assessment`, a NUL byte, `v1`, another
NUL byte, then compact UTF-8 JSON with recursively sorted object keys. Array order
is retained. Structs are normalized to JSON before hashing. The
[independent digest vector](../crates/metadata/tests/fixtures/restore-digest-v1.json)
fixes the version-1 encoding. Digests compare data; they are not signatures or
authorization. A row digest covers the retained database row, not host execution.

Two matching local contexts do not create an atomic GitHub snapshot or prove that
old external writers are fenced. This command does not preserve post-backup source
objects, inspect PR markers/queue/check outcomes or by itself write a durable resolution
decision. Use the separate [operator decision API](restore-decisions.md) to retain
review decisions and already accepted source. Full reconciliation remains required
before supported reconnection. Never manufacture
a missing attempt or clear the quarantine journal to make an assessment pass.
