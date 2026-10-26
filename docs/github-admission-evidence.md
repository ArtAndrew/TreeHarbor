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
  "publication_sequence": 16
}
```

Replace these example identifiers/sequences with your retained records. Omit
`publication_sequence` or set it to `null` to inspect checks without linking a final
publication. Save the request as `evidence.json` and run:

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

`host_policy_verified`, `admission_verified` and `merge_verified` remain false.
Complete effective host/review-policy coverage, workflow runtime context and final
admission certification are separate release requirements.

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
the service, CLI and bridge together. No database migration or credential expansion
is introduced by this feature; schema 29 remains current.

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

These changes preserve historical evidence and its digest when its selected facts
remain unchanged. Expiry of the original dispatch window or credential does not
erase completed facts; expiry still prevents new authenticated submissions.
Explicit revocation is different and invalidates current trust. A recovery report
is bound to the credential that actually authenticated that immutable observation.

Current eligibility is based on retained provider observations. This read does not
contact GitHub and is not an atomic transaction with the host. Final dispatch still
needs the normal live provider checks; this endpoint grants none.

## Encoding and recovery

The digest is SHA-256 over `treeharbor-connected-check-set\0v1\0` followed by compact
UTF-8 JSON of `evidence`, with object keys in lexical order. Arrays retain their
order: candidate parents are ordered; managed requirements follow canonical policy
program order, followed by canonical workflow order. IDs and integer values retain
their exact representation. Current eligibility fields are outside this historical
digest. An independent encoding vector is included in the tests.

The evaluator writes no receipt, approval or outbox event. Identical retained inputs
produce the same evidence after restart. Newly available receipt/file observations
may add evidence and change its digest; previous returned digests are observations,
not persistent grants. A final-publication link requires the same membership and
full candidate commit/tree/ordered parents; omitting the link proves no publication.
