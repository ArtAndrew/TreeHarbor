# Host policy and review observations (development)

TreeHarbor can retain GitHub branch protection, applicable rules, their source
rulesets, and PR review/thread state for an exact saved queue candidate. This is
**evidence collection**, not a complete interpretation of GitHub policy or
permission to merge. `host_policy_verified`, `admission_verified` and
`merge_verified` remain false, including when the capture has no diagnostic reasons.

## Collect and read

Start with a stable [queue membership capture](github-queue-groups.md) containing
the selected PR and its exact candidate. The assigned bridge uses the existing
private queue journal and a local `host.merge` credential:

```sh
treeharbor github queue --config /private/treeharbor/github-app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/bridge.json \
  --queue "$QUEUE_ID" --epoch 1 --state /private/treeharbor/queue-state \
  --host-policy-group 4
```

`4` is the retained membership report sequence. An existing queue attempt is
required; this option cannot start one and conflicts with the other evidence
selectors. It does not enqueue, dequeue, change settings or publish checks. Pending
journal records replay before loading App credentials. Keep the journal when
recovering from a lost reply. Upgrade CLI and service together.

The GitHub App must have Contents, Metadata, Pull requests, Administration and
Merge queues **read** on the selected repository. The collector requests only
these read permissions in its temporary installation token and attempts to revoke
it on both success and failure. It does not expand App grants. GitHub can omit
ruleset bypass actors from read-only responses; that remains explicitly unproven.

A repository reader sends this through `treeharbor client ... github --request`:

```json
{"action":"queue_host_policies","epoch":1,"queue":"66666666-6666-4666-8666-666666666666","after":0}
```

The response returns at most one immutable capture, its `capture_digest`,
`capture_reasons`, and `next_after` when another page exists. The assigned bridge
can select the exact source with `queue_host_policy_context` and
`membership_sequence`. The server checks the saved membership, upstream binding,
tenant, repository, queue and authority epoch. A caller cannot substitute a
structurally valid member from another capture or repository.

## Captured information and limits

Two full observation rounds retain:

- Exact queue membership, PR identity/head/base and candidate commit/tree/parents.
- Legacy branch protection, or an explicit HTTP 403/404 with no error body.
- All applicable active branch rules and source-qualified ruleset documents,
  including inherited organization/enterprise sources returned by GitHub.
- Current nullable review decision and the complete visible review-thread
  connection, with resolved/outdated state and exact PR identity on every page.

Unknown rule fields remain in raw documents. Missing bypass actors are different
from an observed empty array. Unknown semantic clauses, review counts, code-owner
policy and bypass behavior are not interpreted by this collector. A stable
`CHANGES_REQUESTED` observation can have no capture-consistency reasons and still
cannot authorize a merge. A null review decision does not mean no reviews are
required.

Malformed identities, missing pages, duplicate rules/threads, changing PR identity
or counts across review pages, and inconsistent ruleset sources reject collection.
Changes between complete rounds remain recorded with diagnostic reasons. HTTP
403/404 ruleset documents, hidden/nonempty bypass lists, changed effective rules
and differing ruleset definitions remain unproven. Failure response bodies and
credentials are not stored in the report.

The collector is bounded to 100 effective rules, 32 source rulesets, 1,000 review
threads, and 48KiB for the complete encoded policy report. The byte cap includes
both rounds and can be reached before the count limits. Per-response and time
limits also apply. Exceeding a limit fails collection; no sample is substituted
for a complete report. Rules and threads are sorted deterministically, and
pagination never follows provider-supplied URLs.

## Retention and meaning

Queue observation version 9 contains only a host-policy report version 1. It
cannot include a dispatch receipt, a `mutation_not_sent` claim, or another evidence
payload. Earlier report versions retain their encoding and meaning. No database
migration beyond the current schema29 is introduced.

The digest is SHA-256 of the UTF-8 domain
`treeharbor-github-host-policy-observation`, a NUL byte, `v1`, another NUL byte,
and compact JSON of the complete report with lexically ordered object keys.
Array order is retained. The digest identifies evidence; it is not a provider
signature or an approval. An independent fixture fixes the encoding.

Reporting uses the existing transaction, idempotency receipt and outbox. A capture
does not replace the latest queue-membership state. Replay after a lost reply
returns the original receipt. Binding retirement does not erase historical facts;
reporting still requires the assigned bridge's current repository authorization.

Before/after reads are not an atomic transaction with GitHub and cannot exclude an
unseen administrator change between requests. Full supported-policy evaluation,
owner-reviewed policy pinning, workflow execution-context proof, final admission
and live installed-App acceptance remain separate release gates. Local transport
fixtures and real database/process-recovery tests do not close those gates.
