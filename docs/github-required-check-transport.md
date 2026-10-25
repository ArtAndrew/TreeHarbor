# Required GitHub check transport

Development library boundary, not an enabled product command. This transport can
create and recover a check on an exact PR head or retained merge-group candidate.
It does not derive execution evidence, authorize a publication or persist a
dispatch ledger. Durable owner authorization, service/CLI wiring and live managed
check acceptance remain required before using it as a product admission gate.

## Identity and outcome

`treeharbor-github-contract::check_publication` defines version 1 of `Target` and
`Report`. The independent [wire vector](../crates/github-contract/tests/fixtures/check-publication-v1.json)
contains a PR target, raw protection/rule responses, their fingerprints and the
exact expected receipt. Neither record is a native Validation or merge proof.

A target pins the publication UUID, authority epoch, App ID, approved program
digest, execution-evidence digest and terminal conclusion. Its source is either
the exact PR identity/head or the queue target plus retained membership and
candidate parents. Same-tree commits are not interchangeable. The caller must
derive this target from trusted execution evidence; it must not accept an agent's
assertion of success.

The context is `TreeHarbor/e<epoch>/<full program SHA-256 hex>`. Host protection
must require this exact name and App ID. Changing the program, runtime image
included in the program, or authority epoch requires explicit host-rule rotation.
There is no automatic mutation of branch protection. Changing only the evidence
changes the external ID, not the program's context name.

`external_id` is `treeharbor:v1:<publication UUID>:<evidence SHA-256 hex>`. It is a
correlation field, **not a GitHub idempotency key**. Outcomes are success, failure,
cancelled and action_required. Neutral/skipped are not supported because they can
satisfy required-check policies. An unknown execution must never become success.

## Dispatch

`Connection::publish_check` narrows a temporary installation token to one numeric
repository with Contents:read, Metadata:read, Pull requests:read, Merge queues:read,
Administration:read and Checks:write. It receives no branch, PR or queue write
permission. Existing read operations keep their prior permissions.

After verifying installation/repository identity, it reads the source and policy
twice. Both observations must agree. A PR must remain open on the exact head;
draft preflight is allowed. A group must retain the selected member, exact
candidate/parents and expected effective rules; draft or removed members stop
dispatch. This is a bounded observation, not an atomic transaction with GitHub.

The initial supported protection profile requires:

- Legacy branch protection containing the exact App-bound context.
- Admin enforcement, force pushes and deletion disabled, and no legacy review
  bypass allowances.
- An effective merge_queue rule from the branch-rules endpoint.

Missing, ambiguous or unsupported protection fails closed. Ruleset-only protection
is not supported by this initial proof. This proves the selected check is required
under the observed profile; it does not verify every review/queue rule or final
merge authorization. Protection fingerprints include the complete bounded raw
response. Effective-rule fingerprints reuse the existing canonical rule contract.

The caller's `before_write(preflight)` callback must durably save that observation
and recheck current local authorization. A callback rejection causes no check
write. The library then sends one create request and rereads the resulting check
ID, requiring exact App/name/head/external ID/conclusion equality. It does not
update an existing check or automatically retry creation.

A `MutationFailure` distinguishes a failure before dispatch from an uncertain
write. Once the request might have left the process, the caller must persist
uncertainty and use recovery. Revocation after dispatch cannot atomically recall
a GitHub write; releasing a successful result is an irreversible external effect.
The caller must serialize active publishers for the same repository/epoch/head/
program and must not reuse a historical dispatch receipt as permission to send.

## Recovery and limits

`observe_published_check` uses a new read-only token. It scans all checks for the
exact SHA/context twice with `filter=all`, at most 1,000 records and 20 per page.
Totals, IDs and complete results must remain consistent. It adopts only one exact
App/external-ID match with the expected head, context and conclusion. Duplicate
matches, drift, truncation and contradictory receipts fail closed. No match stays
**unknown**; absence does not prove that the original request was never sent.

Recovery may retain a historical preflight, including one that no longer permits
new dispatch. A receipt records a provider fact; it does not refresh approval or
certify current host policy. The caller's durable ledger remains authoritative
for local authorization and one-shot dispatch.

The transport has a fixed GitHub origin, bounded responses, no redirects or
implicit retries, four concurrent slots and 45-second phase timeouts. It attempts
token revocation after success or failure. A failed revocation is recorded on a
successful observation; it does not undo the check. Tokens expire independently.
Reports omit credentials and source-file contents.

Contract tests and actual loopback HTTP fixtures exercise PR/group drift, weak
protection, lost responses, wrong issuers/SHAs, callback rejection, pagination,
duplicate recovery, negative outcomes and failed revocation. These fixtures are
not evidence of a live installed-App write or the full Connected release gate.
