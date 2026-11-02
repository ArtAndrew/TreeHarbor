# Discover source missing after a restore

Development functionality after 0.1.11; not in the published 0.1.11 package.
Use this after [quarantine and local configuration](restore-configure.md), with
the old deployment fenced and every recovered GitHub binding still revoked.
It records a completed traversal of GitHub heads and PRs, then identifies observed
revisions without verified, retained source in TreeHarbor.

**This does not reactivate publishing.** An inventory spans a time interval, not
an atomic GitHub snapshot. It cannot discover deleted history that GitHub no
longer exposes, prove external writers stopped, settle old dispatches, or establish
that the entire recovery is complete. Retained operations also require the
[separate assessment](restore-github-assessment.md).

## Capture once, resume the same capture

Use the assigned bridge's fresh `host.read` credential and the original App's
read access. An agent or ordinary repository owner cannot submit trusted host
observations. This command adds no App permissions or service credentials.
Keep App keys and private capture files outside agent workspaces.

```sh
umask 077
mkdir /absolute/private/treeharbor/inventory-001

cargo run -p treeharbor-service --locked -- github restore-capture-inventory \
  --config /absolute/private/treeharbor/github-app.json \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-bridge.json \
  --binding "$TH_OLD_BINDING" --epoch 1 \
  --manifest-sha256 "$TH_BACKUP_MANIFEST_SHA256" \
  --state /absolute/private/treeharbor/inventory-001
```

The command obtains the restore context before and after the existing read-only
provider traversal. It includes **all observed branches** and open, closed and
merged PRs, including non-TreeHarbor names and deleted branch/fork identities.
Foreign and unknown PR head repositories remain explicit.

Before the first server write, `inventory.json` retains the complete normalized
capture, its stable key and ordered page hashes in the private state directory.
The server accepts a manifest, individually hashed pages, then a seal. A seal
requires every page, exact counts and unique branch/PR identities. Partial uploads
cannot become complete inventories. Source capture neither creates old publication
attempts nor changes their state.

If interrupted, rerun **the identical command and state directory**. The CLI sends
the same manifest and pages before loading App configuration or accessing GitHub.
Committed writes replay their original receipts. `sealed.json` contains the final
receipt and `inventory_key`. A successful seal means the named traversal was
fully retained; `reconciliation_complete` and `publication_authorized` remain false.

Fresh manifest acceptance requires a capture completed within the last 120 seconds
(up to five seconds future clock tolerance). An already accepted manifest can
finish uploading later while its restore context remains current. If the manifest
never committed and expired, or retained context changed, investigate the response
and start a new capture in a new directory. Preserve the old directory for audit;
never edit its hashes to force acceptance. A historical seal replay is not current
host evidence. No page retransmission extends the original capture's timestamp.

## Inspect source coverage

Use a recovery-owner credential with `repository.write`. Save the following
request with the `inventory_key` from `sealed.json`, starting at page zero:

```json
{
  "action": "restore_inventory_coverage",
  "epoch": 1,
  "inventory_key": "restore-inventory:<UUID from the seal>",
  "page": 0
}
```

```sh
cargo run -p treeharbor-service --locked -- client \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-owner.json \
  github --request /absolute/private/treeharbor/coverage-page-0.json
```

Follow `next_page` until it is `null`. Each response groups the page's branch tips
and PR head/base/merge revisions and includes their origins. A revision may occur
on several pages: deduplicate by `git_revision` before counting missing commits.
`missing_on_page` counts unique revisions on that page only.

`preserved` requires an actual server-generated Rust Git-projection receipt for
that exact Git commit, in this repository/epoch and backup/binding scope, with its
independent recovery pin still pointing at a live native commit generation. An
ordinary base pin, imported origin claim, passing check or operator assertion is
insufficient. Missing or removed pins produce `missing_verified_source`.
Coverage does not reread every source byte or attest current disk health.

Use [exact source acquisition](restore-acquire-source.md) or
[source preservation and verification](restore-source-verification.md) for missing
revisions, then query coverage again. Acquisition uses the configured repository;
it cannot assume a foreign or deleted fork's head is fetchable there. If exact
source is unavailable, retain the missing status and recover from an independently
retained complete pack/backup with appropriate access. Do not substitute a current
branch tip or ignore a missing PR head.

Coverage is a current local read under the repository lock. It refuses changed
restore context; different pages can observe different moments of local source
preservation. It is **not an admission certificate**, even if all pages show zero
missing revisions. Old external processes, host writes after capture, unavailable
host history and missing post-backup operation metadata remain separate concerns.

## Bounds and evidence

The existing provider traversal is bounded to 5,000 heads and 5,000 PRs within
120 seconds. Up to 625 pages hold 16 items each; a page is limited to 48 KiB and
requests retain the existing 64 KiB limit. The private complete-capture file is
bounded to 32 MiB. Limit violations fail explicitly; no subset is sealed as the
full capture. An empty traversal has zero pages and coverage page zero is empty.
Inventory observations and recovery pins are durable and are not automatically
pruned; include their disk/metadata growth in retention planning.

The wire vector is [restore-inventory-v1.json](../crates/metadata/tests/fixtures/restore-inventory-v1.json).
Real PostgreSQL tests cover incomplete and duplicate pages, role/context fences,
restart, actual projection proof and loss of its pin. An actual CLI/server test
loses a committed page response, kills both processes and resumes without App
files. Its provider capture and quarantine are explicit fixtures, not a live
GitHub disaster-recovery acceptance test.
