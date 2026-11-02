# Capture an exact check during recovery

Introduced in 0.1.12-alpha.1; not included in 0.1.11 or earlier packages.
After [restore assessment](restore-github-assessment.md), compare one retained
TreeHarbor check-publication intent with an explicitly selected GitHub check run.
The command reads its numeric ID twice and verifies its opaque node ID. It never
creates, updates or reruns a check, and never resumes a publisher.

Keep old publishers fenced. Use the original private App configuration and a
fresh `host.read` credential for the binding's assigned bridge. The App token is
narrowed to one repository with contents, metadata, pull-request and checks read
permissions. No write or administration permission is requested by this capture.

```sh
umask 077
mkdir -m 700 /absolute/private/treeharbor/check-capture-001
treeharbor github restore-inspect-check \
  --config /absolute/private/treeharbor/github-app.json \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-bridge.json \
  --binding "$TH_BINDING" --epoch "$TH_EPOCH" \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  --operation "$TH_CHECK_PUBLICATION" \
  --check-id "$TH_GITHUB_CHECK_ID" --node-id "$TH_GITHUB_CHECK_NODE" \
  --state /absolute/private/treeharbor/check-capture-001
```

Set variables to reviewed identities. `operation` is the retained TreeHarbor
publication UUID, not a native validation-run UUID. GitHub's numeric and opaque
identifiers are distinct; do not derive one from the other. A previously retained
check identity must match the selection. Legacy receipts lacking a node ID still
constrain the numeric ID. When no identity was retained, selection is explicit
and is not automatic adoption. Missing publication intents are not supported.

## Interpret the result

The receipt retains both complete snapshots: ID/node, App, name, head revision,
external operation marker, status and conclusion. The server computes:

- `matches_retained_target`: snapshots agree and match the original App, name,
  exact commit, external ID and completed conclusion. This can match a retained
  **failure**, so matching metadata does not itself mean passing tests.
- `differs_from_retained_target`: snapshots agree but differ from that intent,
  including a pending run, changed code, missing marker or different conclusion.
- `changed_during_capture`: snapshots differ; capture again before treating them
  as current observations.

`metadata_matches` reflects only that comparison. `validation_authorized`,
`source_mapping_verified`, `reconciliation_complete` and
`publication_authorized` remain false. An old success is never transferred to
new code. This command does not re-establish current execution trust, policy or
permission to publish. It leaves old reports, attempts and preflights untouched.

Equal reads are not atomic and do not prove there are no duplicate checks or
post-backup operations. A missing/forbidden check or wrong numeric/node identity
fails the capture; it is not interpreted as a check that was never published.
The service trusts the assigned bridge's observations, validates scope and times,
and does not independently verify a GitHub signature or TLS transcript.

## Retry and API

Use the same private directory and arguments after interruption. The exact report
and request key are persisted before submission. A committed report replays after
restart without an App key or another GitHub read, subject to current TreeHarbor
authorization. It remains historical evidence. Use a new directory for a fresh
capture; do not alter old timestamps or identifiers.

Fresh capture duration is at most 120 seconds, and its end must be within the
preceding 120 seconds with 5 seconds of forward clock skew allowed. Missing
requested nullable `external_id`/`conclusion` fields fail parsing; explicit nulls
are retained as mismatches where applicable. Responses are bounded at 128 KiB;
external IDs at 1,024 bytes, status/conclusion at 32 bytes and names at 256 bytes.
The collector uses bounded setup/read phases and attempts token revocation on
errors as well as success. Failed revocation yields an unsuccessful CLI exit even
when the report was stored. Killing the process does not revoke the provider token.

`restore_check_context` takes `epoch`, `binding`, `manifest_sha256`, `operation`
and `identity: {"id": 73, "node_id": "<opaque ID>"}`. Its version-1 wrapper
returns the unchanged version-2 restore context and exact retained target.
`restore_check_record` takes `key`, `epoch` and `specification`; the
[version-1 wire fixture](../crates/metadata/tests/fixtures/restore-check-v1.json)
documents the shape with test-only static identities/timestamps.
Ordinary repository `read` permission can retrieve a committed receipt:

```json
{"action":"restore_check_status","key":"<key returned by the capture>"}
```

Full recovery still requires discovery/preservation of post-backup work, delivery
facts, external fencing and verified reconnection. This command covers selected
retained managed-check publications, not every GitHub Actions workflow attempt.
