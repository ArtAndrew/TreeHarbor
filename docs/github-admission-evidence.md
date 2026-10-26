# Candidate check-set evidence (development)

TreeHarbor can evaluate every clause in a pinned [admission policy](github-admission-policy.md)
for one retained GitHub queue candidate. The result distinguishes historical
evidence from current local eligibility. It is a read operation, not permission
to dispatch a check, enqueue a PR or merge code.

## Inspect a candidate

Use a repository credential with `read`. Select a `sequence` from
[`queue_checks`](github-group-checks.md); that capture already identifies the exact
membership record and candidate. Optionally select a final-publication sequence
from [`queue_publications`](github-publication-proof.md).

```json
{
  "action": "queue_admission_evidence",
  "epoch": 1,
  "queue": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "check_sequence": 12,
  "publication_sequence": 16,
  "host_policy_sequence": 15
}
```

Replace these example identifiers/sequences with your retained records. Omit
`publication_sequence` or set it to `null` to inspect checks without linking a final
publication. Optionally select `host_policy_sequence` from `queue_host_policies`
to assess a recent candidate-bound complete configuration against the owner pin.
Omitting it preserves the version 1 historical check-set response. Save the request as `evidence.json` and run:

```sh
treeharbor client --socket "$SOCKET" --credential-file "$READER" \
  github --request evidence.json
```

The response contains:

| Field | Meaning |
| --- | --- |
| `evidence` | Selected queue, policy generation, membership, full candidate commit/tree/parents, requirement results and immutable receipt references |
| `evidence.complete` | Every policy clause has sufficient historical evidence and the retained membership/check snapshots are consistent |
| `evidence.requirements` | Per-clause identities, `verified` result and missing/failed/ambiguous evidence reasons |
| `evidence_digest` | Versioned digest of the evidence object; not a signature or an authorization token |
| `current_locally` | Source, policy and trust remain eligible under current local records, with no later selected check capture or unresolved same-context publication |
| `checks_verified` | Historical evidence is complete **and** current local eligibility holds |
| `evidence.publication` | Optional exact candidate-publication link; its checked-content fact does not establish historical merge authorization |

With `host_policy_sequence`, evidence version 2 includes `host_policy` and
`capture_sources_trusted`. `host_policy_verified` can be true for a supported,
current configuration matching the owner's complete pin. `admission_verified`
also requires complete checks and current local eligibility. `merge_verified`
remains false. These fields describe a bounded assessment at read time, not a
permission to merge or proof that a merge was authorized. Unsupported review
requirements, multi-member policy coverage and workflow runtime context remain
explicitly unproven.

## What must match

For each managed requirement the evaluator requires one unambiguous successful
provider run in stable complete check snapshots, its exact retained publication
receipt, the original collected execution and the pinned program/allowed validator.
Binding, repository, authority epoch, queue, membership, full candidate and policy
generation must agree. A passing PR check cannot cover the combined candidate.
Another check with the same name and App cannot substitute for the selected run.

Publication Report version 2 retains both the numeric REST check ID and opaque
GraphQL node ID from the same authenticated GitHub response. Creation, rereading
and recovery verify the pair. The evaluator joins the node ID to the exact retained
group run and rechecks App, name, SHA, terminal conclusion, external ID and execution
evidence. It does not decode node IDs or guess aliases. A different legacy/new ID
string fails closed until an authenticated mapping is supported.

Existing Report version 1 records remain readable and retain their original bytes.
They contain no node linkage and cannot establish the new aggregate proof. Upgrade
the service, CLI and bridge together. Schema 30 records the authenticated reporter and credential for new queue reports.
Upgrade migrates existing records with unknown attribution; it does not invent
trust or change their payloads. Old observations remain readable, but fresh
membership, check and policy captures are required to establish attributable
sources for the new assessment. Replaying an old report does not backfill trust.

Missing or duplicate checks, non-successful outcomes, inconsistent captures,
unlinked or ambiguous publications and unproven execution produce explicit reasons.
The whole result remains incomplete when any required clause is missing or fails.
Individual failed checks can still be published normally; publishing one result
does not depend on the other checks already passing.

## Workflows and trust lifetime

Workflow clauses expose retained attempt linkage and root-file matching when those
observations exist. They still return `workflow_context_unproven`: neither a matching
file nor a successful workflow proves the configured environment, toolchain and
transitive dependency hashes. The evaluator does not accept those hashes as proof
from an agent. A policy containing such a clause cannot yet produce a complete set.

Changing/revoking a policy, changing the proposal or retained candidate, revoking
the managed check, runtime profile, node/validator access, publication-owner approval
or reporting bridge trust invalidates current eligibility. A cancelled publication
recovered later remains historical. A pending or unknown publication under the same
host context also blocks current use. Choosing an older capture after a later check
capture does not make the older result current again.

Version 1 preserves historical evidence and its digest when its selected facts
remain unchanged. Version 2 also includes the current policy assessment and source
trust in the evidence; its digest can change as those facts change. Expiry of the original dispatch window or credential does not
erase completed facts; expiry still prevents new authenticated submissions.
Explicit revocation is different and invalidates current trust. A recovery report
is bound to the credential that actually authenticated that immutable observation.

For version 2, the latest host-policy capture must match the exact candidate,
latest membership and current owner configuration. Its first read must be no more
than 120 seconds old and no earlier than the selected checks finished; its last
read permits at most 15 seconds of future clock skew. The queue must remain current
and unexpired. All selected policy/check/membership sources and the latest
membership require attributable, currently trusted bridge credentials and recorded
provider-token revocation. Revoked trust or missing legacy attribution fails closed.

Current eligibility is based on retained provider observations. This read does not
contact GitHub and is not an atomic transaction with the host. Final dispatch still
needs the normal live provider checks; this endpoint grants none.

## Encoding and recovery

The digest is SHA-256 over `treeharbor-connected-check-set\0v1\0` followed by compact
UTF-8 JSON of `evidence`, with object keys in lexical order. Arrays retain their
order: candidate parents are ordered; managed requirements follow canonical policy
program order, followed by canonical workflow order. IDs and integer values retain
their exact representation. Current eligibility fields are outside this historical
digest for version 1. Version 2 uses `treeharbor-connected-check-set\0v2\0`
and includes the host-policy assessment, its reasons and source trust. Independent
encoding vectors cover both versions.

The evaluator writes no receipt, approval or outbox event. Identical retained inputs
produce the same evidence after restart. Newly available receipt/file observations
may add evidence and change its digest; previous returned digests are observations,
not persistent grants. A final-publication link requires the same membership and
full candidate commit/tree/ordered parents; omitting the link proves no publication.
