# Review the whole recovered GitHub repository

Introduced in 0.1.12-alpha.1; not included in 0.1.11 or earlier packages.
This owner-only inspection combines retained operations and source coverage across
**every retained GitHub binding** in the recovered TreeHarbor repository. It requires
quarantine and revoked bindings. It does not reconnect the service or send host
requests, and its successful exit code means the inspection ran, not that recovery
is complete.

Start after [local restore configuration](restore-configure.md). Use
[durable inventory capture](restore-inventory-coverage.md) to retain GitHub heads
and PRs. Keep old infrastructure fenced while investigating recovery.

If the backup predates the first binding, [register a discovery target](restore-first-target.md)
first. That permanently disabled record is new recovery configuration, not proof
of a pre-backup connection. This review still requires complete host inventory and
source preservation for it; zero local effects never bypass those obligations.

## Inspect

The recovery owner's fresh credential needs `repository.write`. This command
does not provision credentials, load an App key, change host permissions or contact
GitHub. Run without `--inventory` first to discover the retained binding IDs and
local blockers. Then select one capture per binding using repeated options:

```sh
treeharbor github restore-review \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-owner.json \
  --epoch 1 --manifest-sha256 "$TH_BACKUP_MANIFEST_SHA256" \
  --inventory "$TH_FIRST_INVENTORY_KEY" \
  --inventory "$TH_SECOND_INVENTORY_KEY"
```

Use only as many `--inventory` options as there are retained bindings. The keys
come from capture seals. Selection order does not affect the assessment hash.
Omitting a binding's inventory reports that binding as incomplete; it cannot
silently remove the binding from the review. Duplicate selections, two captures
for one binding, wrong backup/scope/epoch or unknown inventory keys are rejected.

The equivalent authenticated `client ... github --request review.json` request is:

```json
{
  "action": "restore_review",
  "epoch": 1,
  "specification": {
    "version": 1,
    "manifest_sha256": "<retained backup manifest SHA-256>",
    "inventory_keys": ["<selected inventory key>"]
  }
}
```

The API computes the assessment under one repository lock. It returns
`assessment`, its canonical `assessment_sha256` and the local observation time.
It writes no operation record and creates no publication authorization. Keep a
private copy for operator review; a later invocation recomputes the current state.

## Read the result

Each entry in `assessment.bindings` contains:

- **Inventory:** `missing`, `incomplete`, `stale_context` or `sealed`. A selected
  inventory only counts as fresh when its recorded capture ended within 120 seconds,
  with at most five seconds future clock tolerance. Upload/replay does not refresh
  the capture. Complete source preservation first, then recapture host state if needed.
- **Decisions:** every retained branch, PR, queue, check, dequeue, recovery and
  redelivery subject needs a latest, current-context `do_not_replay` decision to
  count as reviewed. Pending, superseded or stale decisions cannot inherit success.
  Recovery-page identities are included in their parent recovery context.
- **Source:** all selected inventory branch/PR head/base/merge Git revisions plus
  known retained operation targets, including queue bases and combined check
  candidates. Native commits retained by publication intents are checked separately.
  The disappearance of a host branch does not remove these retained obligations.

`sources.missing_git` requires [server-verified mappings](restore-source-verification.md)
with live independent recovery pins in the same binding/backup scope.
`sources.missing_native` requires independent live recovery pins for the old native
commits, even when their Git mapping was never prepared. Ordinary base/publication
pins alone do not satisfy these recovery predicates.

For a retained branch, first record a `preserve_source` decision using
`source_holder:"github-publication:<operation UUID>"` and its exact native revision
(`native:sha256:` plus the context's `native_object_hex`). Retained PR and queue
holders use `github-pull:<operation UUID>` and `github-queue:<operation UUID>`.
The server verifies that the holder actually exists in this repository and names
the supplied live commit; these names do not bypass authorization or create source.
Then record `do_not_replay` with the preservation decision as `previous_key`. The independent pin remains. See
[decision chains](restore-decisions.md). Do not abandon uncertain host outcomes
merely to make a local checklist green: `do_not_replay` records intent, not proof
that a prior request did or did not run.

Counts and `all_sha256` cover each complete dependency/blocker list. Samples show
at most eight entries; `sample_truncated` explicitly identifies longer lists.
Use the detailed context and [per-page source inspection](restore-inventory-coverage.md#inspect-source-coverage)
to investigate the rest. Samples do not limit what the server evaluates.

`local_review_complete:true` means only these local conditions passed for all
retained bindings. **It does not permit binding creation or publication.** The
result always retains `external_fencing_verified:false`,
`prior_host_mutations_settled:false`, `reconciliation_complete:false` and
`publication_authorized:false`. An assessment hash is neither a bearer token nor
a durable admission certificate. Later decisions, removed pins or changed retained
state invalidate earlier conclusions; the next review exposes the change.

## Remaining recovery work and bounds

External runtime/publisher/token fencing, settlement of unknown host writes,
final provider reobservation and policy review remain external responsibilities.
The separate [operator-assisted reconnect protocol](restore-reconnect.md) now
re-evaluates this review atomically when creating a fresh binding. Live full-disaster
acceptance remains unfinished. The review does not attest disk health,
restore missing post-backup operation metadata, recover unavailable deleted host
history or certify all native-mode recovery. Existing bindings stay revoked.

The review supports 1–256 retained bindings, the existing per-binding context and
inventory limits, and a total budget of 50,000 examined subjects, retained revision
identities and inventory items. Its assessment is limited to 512 KiB. Exceeding a
bound fails explicitly without marking a partial result complete. Git mapping
lookup is batched per binding; no performance advantage is claimed. Captures across
different bindings remain different provider observation intervals, even though
the local review is one transaction.

Verification includes real PostgreSQL tests with two bindings, actual Rust
projection proofs, independent native retention, missing selection, changed decisions,
pin removal and service reconnect. The CLI is exercised against a real local
service after interrupted inventory upload and restart. Host capture, quarantine
and execution in these fixtures are not a live full-disaster acceptance result.
