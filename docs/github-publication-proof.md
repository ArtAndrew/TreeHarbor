# Published candidate evidence (development)

TreeHarbor can check whether the **exact saved queue candidate** became the PR's
merge commit and belongs to the observed target branch history. This works when
later commits have already advanced the branch. It does not certify CI execution,
reviews, host policy, or the complete admission decision; `merge_verified` remains
false until those separate release gates are implemented and verified.

## Collect and inspect

Start with a stable version4 [membership capture](github-queue-groups.md). Its two
snapshots must agree and contain the selected PR, with complete two-parent links
from candidate to observed base and exact PR head. A new capture of an empty queue
cannot reconstruct a membership that was never retained.

The assigned bridge uses its existing private queue journal and local `host.merge`
credential. No new GitHub permission is required:

```sh
treeharbor github queue --config /private/treeharbor/github-app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/bridge.json \
  --queue "$QUEUE_ID" --epoch 1 --state /private/treeharbor/queue-state \
  --publication-group 4
```

`4` is the retained membership report sequence, not a PR number. This option
requires an existing queue attempt and conflicts with the other evidence-selection
options. It never enqueues, dequeues or starts a workflow. Pending reports replay
before App configuration is loaded. Preserve the journal and rerun after recovery.
Upgrade CLI and service together; no database migration is needed beyond schema25.

A repository reader sends this through `treeharbor client ... github --request`:

```json
{"action":"queue_publications","epoch":1,"queue":"66666666-6666-4666-8666-666666666666","after":0}
```

Pages contain one capture and an optional `next_after` cursor. Each includes the
immutable report, `exact_candidate_published`, separately matched signed events,
and `checks_verified: false`. The bridge's `queue_publication_context` command
selects the source using `membership_sequence`; an agent cannot submit a substitute
member as trusted evidence.

## What a positive match establishes

The collector reads the exact PR and actual `refs/heads/<base>` twice, bracketing a
comparison addressed by **two full immutable commit IDs**. GitHub's comparison
summary must report `identical`, or `ahead` with zero commits behind, and name the
candidate as merge-base. It is a provider ancestry claim over authenticated API
responses, not a local traversal and rehash of every ancestor. Pagination of the
comparison's commit list is not used as proof; no patch or list is retained.

Both PR/target snapshots must agree. The final commit ID, tree and ordered parents
must equal the retained candidate, and the PR head must still match the original
proposal. A matching tree with a different commit ID does not pass this path.
Concurrent target movement, an open PR, an unrelated final commit, divergent or
behind history produce no positive match. Malformed or contradictory identities,
missing objects, truncated parent lists and API failures reject collection.

The result describes the observed interval, not an atomic lock on GitHub. A later
force-push, policy change or failed rerun cannot be ruled out by an old capture.
This read does not update the latest queue snapshot or transfer validations to a
new SHA. Squash/rebase mappings and full multi-entry admission remain unfinished.

The temporary App token is narrowed to the bound repository with Contents,
Metadata, Pull requests and Checks read. Revocation is attempted on failures as
well as success. Limits are eight complete parents, 24KiB encoded publication
payload, 128KiB per provider response and the existing bounded capture deadline.
A large comparison response can exceed that cap and remain unverified; the
collector does not silently omit data or fetch provider-supplied URLs.

Version8 is an exclusive queue observation payload. Earlier report versions keep
their bytes and meaning. The server resolves the original membership under the
same tenant, repository, queue, binding and authority epoch, rechecks the assigned
bridge, and retains reports with idempotent transaction/outbox completion.
Retiring a binding preserves historical reporting by its assigned, still-authorized
bridge. Revoking that bridge's repository access stops subsequent collection/reporting;
retired bindings cannot grant new queue dispatch.

## Evidence

Pure contracts cover later descendants, changed head/tree/parents, equal-tree
substitution, drift, malformed comparisons and mixed versions. Provider tests use
labelled HTTP fixtures; PostgreSQL and killed CLI/service tests exercise actual
persistence and replay. Real installed-App diagnostics, when linked from the
implementation status, are separate from end-to-end populated membership proof.
