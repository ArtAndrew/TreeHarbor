# Host policy and review observations (development)

TreeHarbor can retain GitHub branch protection, applicable rules, their source
rulesets, and PR review/thread state for an exact saved queue candidate. This is
**evidence collection and conservative policy analysis**, not permission to merge.
`host_policy_verified`, `admission_verified` and
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
`capture_reasons`, a versioned `evaluation`, and `next_after` when another page exists. The assigned bridge
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
from an observed empty array. A stable
`CHANGES_REQUESTED` observation can have no capture-consistency reasons and still
cannot authorize a merge. A null review decision does not mean no reviews are
required.

## Policy analysis

Each returned capture now includes `evaluation` version 1:

- `before_digest` and `after_digest` identify the branch configuration independently
  of the PR, candidate, review decision and review-thread state.
- `before` and `after` contain the union of required App/name pairs, review
  requirements, configuration diagnostics and conditions needing candidate evidence.
- `reasons` combines capture consistency, configuration problems and unmet candidate
  conditions. It compares the entire required check set to the queue target and
  binds review/thread evaluation to the original PR head.

The analyzer checks legacy protection flags, explicit check issuers, review
requirements, visible bypass lists, complete source rulesets and all seven merge
queue settings against the observed queue configuration. Unknown fields/rules,
missing fields needed by this profile, ambiguous issuers, contradictory check
contexts or redacted bypass information produce named diagnostics. Unsupported
source-selection conditions and restrictions also remain unproven. Raw documents
stay available for inspection; diagnostics do not rewrite the captured evidence.

Review counts, code-owner approval, last-push approval and required teams are
retained as distinct requirements. An `APPROVED` enum does not prove them. Unresolved
required conversations still block diagnostics even when their threads are marked
outdated. Signature, linear-history, HEADGREEN and non-MERGE publication conditions
remain explicit candidate-evidence requirements; they cannot inherit the simpler
MERGE/ALLGREEN proof. Deployment, workflow-rule and other unsupported clauses
remain unproven rather than disappearing from the policy.
Groups with multiple members also need policy/review evidence for the other
members; evaluating the selected PR does not certify the whole group.

For example, `queue_configuration_unproven` means that queue settings are missing,
ambiguous or disagree between the applicable rule and queue observation.
`legacy/required_signatures/flag_unproven` means the legacy response does not prove
the signature setting. A read-only collector may legitimately lack fields needed
for complete evaluation. No extra GitHub permissions are requested automatically.

**Empty evaluation reasons still do not authorize admission.** The owner-reviewed
configuration pin, current generation/trust checks, complete candidate CI evidence,
workflow execution context and final publication are separate predicates. This
increment exposes historical diagnostics; it does not dispatch or certify a merge.

Configuration profile version 1 contains repository ID/name, base branch, complete
legacy protection, applicable rules, source documents and optional queue settings.
It has no PR prerequisite and can be analyzed by the shared contract library before
any candidate exists. The configuration-only command below can collect it before
the first PR. Owner pinning is available through [policy version2](github-admission-policy.md#review-and-pin-complete-configuration-version2).
Current candidate-policy and full admission certification remain incomplete.

Its digest is SHA-256 of `treeharbor-github-host-policy-configuration`, NUL, `v1`,
NUL, then compact JSON with lexically ordered object keys. Top-level effective rules
sort by their compact JSON representation; source documents sort by numeric source
ID. Nested arrays and all raw metadata remain significant. The encoding is bounded
to 48KiB and fixed by an independent Python/hashlib fixture. Changing only PR state
does not rotate this identity; changing a retained policy field does. Observation
digests and configuration digests deliberately have different domains.

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
unseen administrator change between requests. Complete policy support,
owner-reviewed policy pinning, workflow execution-context proof, final admission
and live installed-App acceptance remain separate release gates. Local transport
fixtures and real database/process-recovery tests do not close those gates.


## Inspect configuration before the first PR

Use the existing private GitHub App configuration on a trusted machine:

```sh
treeharbor github policy-inspect \
  --config /private/treeharbor/github-app.json --base main
```

This command needs no PR, queue attempt, local service or service credential. It
reads two complete rounds for the exact branch and numeric repository identity.
The default temporary token requests only Contents, Metadata, Administration and
Merge queues **read**, limited to the configured repository. It requests no
Pull requests, Checks or Actions permissions and never upgrades them automatically.

Queue configuration reads on GitHub can require Contents **write**. To request
that scope explicitly, add `--include-queue-settings`. The App must already have
that grant. This option still performs only configuration reads; it cannot change
App grants, branches, rules or queue entries. Without it, omitted queue settings
remain `queue_configuration_unproven`. GitHub permission errors fail the read;
there is no automatic retry with broader privileges.

The JSON result contains the full versioned `inspection`, its `digest`, both
configuration digests, both analyses and `reasons`. `approved` and
`admission_verified` are always false. A successful exit means collection finished
and token revocation succeeded, **not** that the configuration is supported or
approved. Inspect diagnostics and candidate requirements separately. Provider or
validation errors produce failure; failed revocation returns the collected report
with `token_revocation_unproven` and a failing exit status. Tokens and private
keys are never included in the report. Retrying collects a new observation.

The supplementary GraphQL observation binds the selected ref, owning repository,
branch-protection rule ID, explicit policy flags and complete bypass connections.
Explicitly empty bypass lists and disabled push restrictions can resolve specific
absent REST fields only when overlapping observations agree. Malformed fields,
contradictions, redacted ruleset bypass lists and unknown rules remain unproven.
Required deployments remain a separate candidate condition. Personal repositories
receive `personal_repository_queue_unsupported`; this command does not turn them
into organization merge queues.

The collector rejects incomplete or duplicate bypass connections (maximum 100
allowances each), incomplete rule pages, foreign repository/source identities and
reports over 48KiB. Two rounds are bounded observations, not an atomic transaction
with GitHub. An unchanged hash is not an owner approval or evidence of current CI.

Configuration wrapper version 1 preserves the existing Profile v1 bytes and adds
`visibility`. Its digest domain is `treeharbor-github-host-configuration`, NUL,
`v1`, NUL. Inspection version 1 adds capture times, before/after configurations
and revocation outcome; its domain is
`treeharbor-github-host-configuration-inspection`, NUL, `v1`, NUL. Both hash compact
JSON with lexical object keys. The wrapper retains Profile v1 canonicalization
and sorts supplementary context/environment/allowance lists. Raw nested REST
arrays retain their order. Independent fixture bytes fix both encodings.
