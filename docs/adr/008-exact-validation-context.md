# ADR 008: Exact validation context

Date: 2026-10-21. Status: accepted implementation refinement.

A validation-context digest is distinct from source ObjectId and Git OID. Its
versioned, length-prefixed SHA-256 input binds repository scope, authority epoch,
candidate revision and snapshot, target ref, expected head and generation, policy
digest, check name, scheduler run ID, definition/environment/toolchain/dependency
digests and the sorted trusted-validator set. A changed field requires new
evidence. Equal trees do not make distinct commits interchangeable.

The current scheduled run is authoritative: a pending rerun cannot inherit a
previous green result. Receipts are immutable; identical redeliveries deduplicate,
and contradictory or competing authoritative receipts for one run fail closed.
Revocation is separate state. The check predicate requires every configured check
to have one trusted, nonrevoked successful receipt for the exact context. An empty
check set fails closed in this implementation.

The pure evaluator only decides whether checks are satisfied. It does not verify
a candidate's actual tree/parents, authenticate an agent or collector, determine
review approval, issue a merge permission, or detect all semantic conflicts.
The application must derive principal identity from authorization and retrieve
current policy/evidence under the publication transaction's consistency protocol.
The final head/generation/epoch CAS and review/permissions checks remain mandatory.

Structured blockers support actionable API responses without converting stale or
ambiguous evidence to success. No wall-clock sorting of client-reported results
and no "any previous green run" shortcut are permitted.
