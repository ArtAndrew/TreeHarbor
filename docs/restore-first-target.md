# Recover when the backup has no GitHub binding

Development functionality after 0.1.11; not included in the published package.
A backup taken before the first GitHub connection may contain no binding even
though agents later published branches and PRs. **No local records does not mean
no remote history.** Ordinary binding creation stays blocked after quarantine.

Use this procedure only when there are zero retained bindings in the restored
repository. If any binding survived, use its existing identity in the
[reconnect workflow](restore-reconnect.md). Do not delete it to use this path.

## Select a discovery target

Keep the original backup and old deployment fenced. Complete
[staging and local configuration](restore-configure.md). Identify the actual
GitHub App, installation and repository from your operational records. Provision
an enabled recovery bridge with `host.read` scoped to this TreeHarbor repository;
this command does not issue credentials or modify GitHub App permissions.

Registration is an immutable selection. **Check the identifiers before submitting.**
If you select the wrong repository or bridge, start a fresh staged recovery from
the preserved backup. This command cannot replace/delete the target or append an
alternative after registration. Existing published source and the backup are not
overwritten by this procedure.

Prepare a mode-0600 JSON file in a mode-0700 directory:

```json
{
  "version": 1,
  "manifest_sha256": "<backup SHA-256, 64 lowercase hex digits>",
  "target": "<fresh discovery-target UUID>",
  "binding": {
    "version": 1,
    "app_id": 7,
    "client_id": "<actual App client ID>",
    "bridge": "<recovery bridge principal>",
    "upstream": {
      "installation_id": 9,
      "repository_id": 11,
      "owner_id": 13,
      "owner": "<repository owner>",
      "name": "<repository name>"
    }
  },
  "evidence_sha256": "<SHA-256 of your private target-selection record>",
  "prospective_target_confirmed": true
}
```

Numeric IDs are examples; supply your actual installation. The evidence hash
records your choice; it is neither uploaded evidence nor proof of old host state.
Use an owner credential with `repository.write`:

```sh
treeharbor github restore-register-target \
  --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" --epoch 1 \
  --key restore-target-001 --specification /absolute/private/restore-target.json
```

The server validates the selected quarantine receipt against the current restored
database, current authority/epoch, absence of all bindings and the assigned bridge.
It atomically records the target in permanently disabled (`revoked`) state with
`outcome:prospective_recovery_target_registered`. This creates no active interval,
configured event, probe, publication or check. It makes no GitHub request.

The receipt explicitly retains `historical_connection_asserted:false`,
`remote_history_empty_asserted:false`, `publication_authorized:false` and
`reconciliation_complete:false`. This is a configuration created **during recovery**,
not a claim that a connection existed in the backup. The target ID can never become
active. Retain the receipt as the provenance of this binding record.

## Inspect, preserve and reconnect

Use the target UUID as `--binding` in
[inventory capture](restore-inventory-coverage.md) and as `source_binding` in the
[reconnect plan](restore-reconnect.md). The assigned recovery bridge can read the
current target; its disabled state prevents ordinary publishing.

1. Capture every observed branch and PR state using the existing paged inventory.
   An empty list of local retained effects does not satisfy inventory completeness.
2. Acquire discovered source and record actual server-verified Git mappings with
   independent recovery pins. A sealed inventory with missing source still fails
   the repository review.
3. Investigate post-backup/unknown host operations and fence old writers, runtimes,
   keys and issued tokens. These remain external operator responsibilities.
4. Follow the normal plan → fresh probe and inventory → repository review → atomic
   activation sequence. Activation requires a **different fresh UUID** and all
   existing freshness, source and authorization checks. No old CI success transfers.

The recovery target is retained alongside the newly active binding. Its v2 restore
context describes the current retained database identities, including the target
just registered; it is not historical evidence of an earlier active connection.
If a later backup contains this target, ordinary all-binding review includes it.

## Retry and inspect

Retry an uncertain registration with the same key and unchanged JSON file. The
same receipt is returned after restart; it neither adds another target nor changes
the target after reconnection. Concurrent registrations have one winner. A
second key cannot silently replace the selected target.

To retrieve the historical registration through the authenticated JSON API:

```json
{"action":"restore_target_status","key":"restore-target-001"}
```

This action also requires `repository.write`. Use reconnect `status` to inspect
current activation state; the registration receipt itself is historical.
Wrong manifest, missing quarantine, wrong authority/epoch, an absent/disabled bridge
or existing bindings reject registration without partial rows. Never clear journals
or edit database records to force admission.

## Evidence and limits

Tests cover competing registrations, wrong manifest/role/bridge, restart/replay,
permanent target disablement, missing remote inventory, missing source mappings,
actual Rust projection/preservation and subsequent ordinary reconnect admission.
The real CLI/service recovery trial runs both with a retained old binding and with
this new target, including a lost activation response and worker/server restart.
Provider and quarantine inputs in those trials are explicit fixtures. Live installed
full-disaster acceptance, lost post-backup metadata and external fencing remain
separate requirements. Registration does not certify any of them.
