# Compare a retained webhook delivery during recovery

Introduced in 0.1.12-alpha.1; not included in 0.1.11 or earlier packages.
After [restore assessment](restore-github-assessment.md), inspect one retained
webhook recovery attempt without redelivering it or reactivating a channel.
The command reads the exact provider delivery ID twice and retains bounded
metadata and a normalized payload hash. It does not store provider headers,
secrets or the provider's request body in the receipt.

Keep old publishers and receivers fenced. Both the selected binding and event
channel must be revoked in the quarantined database. Use the original private
App configuration and a fresh `host.read` credential for its assigned bridge.
The GitHub delivery API uses an **App JWT**, with App-wide access, not a narrowed
installation token. TreeHarbor checks App, installation, owner, repository,
delivery GUID, action and callback identity locally. Do not give the App key to
agents. No installation token, new permission or delivery request is created.

```sh
umask 077
mkdir -m 700 /absolute/private/treeharbor/delivery-capture-001
treeharbor github restore-inspect-delivery \
  --config /absolute/private/treeharbor/github-app.json \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-bridge.json \
  --binding "$TH_BINDING" --epoch "$TH_EPOCH" \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  --recovery "$TH_RECOVERY" --provider-id "$TH_DELIVERY_ID" \
  --state /absolute/private/treeharbor/delivery-capture-001
```

Use reviewed identities from restore context. `recovery` is the retained
TreeHarbor recovery UUID; `provider-id` identifies a GitHub delivery attempt.
Several attempts can share a GUID. The GUID alone cannot select an attempt.
Unknown operations created after the backup are not supported by this command.

## Read the receipt

`classification` is `changed_during_capture`, `matches_retained_payload`,
`differs_from_retained_payload` or `no_retained_payload`. Consistency compares
attempt metadata and payload hashes, ignoring differences in observation time.
Matching the payload does not imply identical historical HTTP status.

The server independently compares the observed hash with its retained,
authenticated inbox for that exact channel and GUID:

- `received`: matching content is already present, with retained event ID,
  sequence and raw-body hash. This does not establish event processing.
- `missing`: no retained matching delivery identity exists in this database.
  It may have been received after the backup; this is not proof of global absence.
- `payload_conflict` or `comparison_unavailable`: content does not match or
  cannot be compared. Do not treat it as confirmed receipt.
- `capture_inconsistent`: the two provider reads changed; no receipt comparison
  is accepted from this capture.

A provider HTTP 2xx status never substitutes for the inbox comparison.
`prior_dispatch_settled`, `event_processing_verified`, `reconciliation_complete`
and `publication_authorized` remain false. Old attempts, reports and channels
are unchanged, and no outbox job is created.

## Retry and limits

Reuse the same private directory and arguments after interruption. A committed
report replays with current authorization before loading App configuration or
contacting GitHub. It remains a historical observation. A new capture needs a
new directory; do not edit old timestamps to make a stale report acceptable.

The collector bounds setup and both reads to 45 seconds. The server requires
ordered observations inside a capture of at most 120 seconds, ending within the
previous 120 seconds, allowing 5 seconds of forward clock skew. A changed local
restore context requires a fresh capture. Historical committed retries retain
their original receipt even if that context later changes.

Missing, forbidden, expired or mismatched provider deliveries fail the capture;
they are not inferred to have never existed. A changed callback configuration
also fails. Two equal reads are not atomic and do not prove discovery coverage.
The service trusts its assigned bridge's provider observations; it does not verify
a GitHub signature over the provider API response. Inbox authenticity comes from
the separate signed receiver path.

`restore_delivery_context` takes `epoch`, `binding`, `manifest_sha256`,
`recovery` and `provider_id`. `restore_delivery_record` takes `key`, `epoch` and
`specification`, shown in the [v1 wire fixture](../crates/metadata/tests/fixtures/restore-delivery-v1.json).
The context wrapper is v1 over the unchanged v2 restore context. A repository
`read` credential can retrieve an already committed receipt:

```json
{"action":"restore_delivery_status","key":"<saved request key>"}
```

Full recovery still needs post-backup discovery and source preservation, external
fencing and verified reconnection. This command does not authorize replay of an
uncertain dispatch or activation of a replacement publisher.
