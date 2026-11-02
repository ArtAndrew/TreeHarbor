# Retain recovery decisions and source

Development API after 0.1.11; not included in the published 0.1.11 package.
After [assessing a restored binding](restore-github-assessment.md), an operator
can save a decision against the exact assessment context. The record survives
service restart and lost responses. A source-preservation decision also creates
an independent permanent root pin in the same transaction as its receipt.

This is part of recovery, **not permission to reconnect GitHub**. The old binding
stays revoked, and the restore journal stays intact. Decisions neither send host
requests nor change old dispatch attempts, observations or checks. Full host
reconciliation is not established by these decisions. The separate
[operator-assisted reconnect protocol](restore-reconnect.md) adds fresh
observations and atomic new-binding admission; live recovery acceptance is open.

## Save a reviewed decision

Use a fresh human credential with `repository.write` to record a decision and
`read` to retrieve it. The assigned bridge's `host.read` is used for assessment;
it cannot record an operator decision. Keep the assessment and request files
private and include them in your independent backup. No command below provisions
credentials or requires new GitHub App permissions.

The following example requires `jq` and `sha256sum` on the operator's Linux host.
Select one operation UUID from a successful assessment's `context.effects`.
Create the request once; keep its bytes and key unchanged on retry. Use new filenames
for a new decision so that shell redirection does not overwrite retained evidence.

```sh
umask 077
: "${TH_OPERATION:?Select the retained operation UUID to review}"
: "${TH_DECISION_KEY:?Choose a new stable key such as restore-review-001}"
assessment=/absolute/private/treeharbor/restore-assessment.json
evidence_sha256=$(sha256sum "$assessment" | cut -d ' ' -f 1)
jq -e --arg operation "$TH_OPERATION" --arg key "$TH_DECISION_KEY" \
  --arg evidence "$evidence_sha256" '
  . as $report |
  [.context.effects[] | select(.id == $operation)] |
  if length != 1 then error("select exactly one retained operation") else .[0] end |
  {action:"restore_decision_record",key:$key,epoch:$report.context.epoch,
   specification:{version:2,binding:$report.context.binding,
     manifest_sha256:$report.context.manifest_sha256,
     context_sha256:$report.context_sha256,evidence_sha256:$evidence,
     subject:{kind:"retained",effect_kind:.kind,operation:.id},
     disposition:{action:"await_evidence"},previous_key:null,
     note:"Inspect host state and preserve post-backup source before proceeding."}}
' "$assessment" > /absolute/private/treeharbor/restore-review-001.json
```

Review the resulting file before sending it. The evidence digest is ordinary
SHA-256 of the complete file bytes; the context digest uses the assessment's
[versioned canonical digest](restore-github-assessment.md#version-2-context-and-limits).
Neither digest proves the external facts or supplies the evidence file to the server.

```sh
cargo run -p treeharbor-service --locked -- client \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-owner.json \
  github --request /absolute/private/treeharbor/restore-review-001.json
```

The server recomputes the context under the same repository lock used to commit
the decision. A changed operation, quarantine identity or binding context cannot
be silently accepted. If the response is lost, resend the exact same request.
An identical key returns the historical receipt, even if state has since changed;
it does not revalidate that decision for current use.

## Decisions and subjects

| Disposition | Effect |
| --- | --- |
| `{"action":"await_evidence"}` | Record that investigation is still required. |
| `{"action":"do_not_replay"}` | Record the operator's decision against replay. Does not assert that an external write never happened or alter the original operation. |
| `{"action":"acquire_source","git_revision":"git:sha1:<40 hex>","max_objects":100000,"max_bytes":4294967296}` | Version 3 only: select an exact revision for the [source acquisition workflow](restore-acquire-source.md). Recording alone downloads nothing and creates no source pin. |
| `{"action":"preserve_source","source_holder":"<existing pin>","native_revision":"native:sha256:<digest>"}` | Copy a live, repository-scoped commit pin to an independent permanent recovery pin. Wrong revision, object type, scope or generation fails atomically. |

Use the source-import/upload result's existing pin holder and exact native revision
for preservation. This command does not fetch or accept new source bytes.
Use [server-side source verification](restore-source-verification.md) afterward
to obtain a separate verified Git/native mapping receipt. Preserve
post-backup code through the source acceptance path first. A successful pin copy
protects that accepted commit and its reachable object graph from ordinary GC;
it does not establish Git/native mapping, prove a host PR's contents, attest physical
storage health or prove all missing source was found. `git_mapping_verified` stays
false. Recovery pins have no expiry and are not automatically released or replaced
by later decisions; account for their disk use.

The subject can be:

- `{"kind":"retained","effect_kind":"branch","operation":"<UUID>"}` for an
  operation present in the current context (`branch`, `pull`, `queue` or `check`).
- `{"kind":"retained_dequeue","operation":"<UUID>"}` for a retained removal.
- `{"kind":"retained_recovery","recovery":"<UUID>"}` for a retained webhook recovery request.
- `{"kind":"retained_redelivery","recovery":"<UUID>","provider_id":41}` for
  exactly one retained provider attempt in that recovery. A shared delivery GUID
  does not collapse separate attempts.
- `{"kind":"external_branch","name":"treeharbor/<branch>"}` for an external
  branch the operator is investigating.
- `{"kind":"external_pull","number":123,"node":"<GitHub node ID>"}` for an
  external PR the operator is investigating.

The three new subject kinds require specification `version:2` or `version:3`; version 1 remains
accepted for its original subjects. Every retained subject must exist in the
current selected binding's context. Use a fresh version-2 context digest even when
reviewing an original subject. Historical receipts and source pins are preserved;
an old context does not silently gain coverage of the newly included operations.

External identities and the evidence digest are operator assertions. They are not
promoted into trusted GitHub observations. There is no `approve`, `resume` or
`clear_quarantine` disposition.

## Revise or retrieve a decision

Each manifest/binding/subject has an append-only chain. For a later decision,
use a new request key, set `previous_key` to that subject's latest decision key,
and supply a newly reviewed current context digest. `previous_key:null` is valid
only for the first decision. Two concurrent submissions against the same predecessor
cannot both succeed. An old decision and its source pin remain intact.

To read a historical receipt, save this private request and use the same CLI:

```json
{"action":"restore_decision_status","key":"restore-review-001"}
```

The receipt contains the scoped context, complete decision specification, author,
sequence and any copied source pin. `external_facts_verified`,
`reconciliation_complete` and `publication_authorized` are always false.
Read permission returns history; it makes no claim that its context is still current.

Version 1 is fixed by the [request and identity vector](../crates/metadata/tests/fixtures/restore-decision-v1.json).
The per-decision pin holder is `restore-decision:` followed by the assessment
version-1 digest of the JSON array `[1, request_key]`, within the repository scope.
Requests use the service's 64 KiB bound; notes are at most 4,096 UTF-8 bytes, keys
at most 128 bytes, and the existing context bounds still apply. No schema migration
is required; records use the durable operation journal.

| Response | Action |
| --- | --- |
| `restore_context_changed` (409) | Reassess and review the changed state; submit a new request. |
| `restore_decision_conflict` (409) | Read the intervening decision before naming it as predecessor. |
| `idempotency_conflict` (409) | The key already names different bytes; retrieve the original receipt. |
| `not_found` / source receipt mismatch | Check subject, source holder, scope and exact native revision; no partial pin/decision is committed. |

Do not clear the quarantine journal or manufacture attempts to bypass these checks.
Full admission still needs all binding/effect families, PR markers, queue/check
outcomes, post-backup source mappings and external-writer fencing. Recording a
review decision does not satisfy those remaining checks by itself.
