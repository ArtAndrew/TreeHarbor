# Configure a quarantined local restore

Available in [0.1.11-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.11-alpha.1).
Earlier packages do not contain `backup restore-configure`.
This is an operator-controlled local recovery path;
full Connected disaster recovery and unattended production operation remain open.

Start with [restore staging](restore-stage.md) and [quarantine](restore-quarantine.md).
Keep the original deployment fenced: its database writers, nodes, runtimes and
publishers must not restart. Keep the target stopped too. The command cannot prove
these external actions; `--old-infrastructure-fenced` acknowledges your responsibility
for them. Do not run this while old and recovered deployments can both accept work.

```sh
: "${TH_BACKUP_SHA256:?Set the independently retained manifest digest}"
treeharbor backup restore-configure \
  --stage /srv/treeharbor-recovery/stage-001 \
  --manifest-sha256 "$TH_BACKUP_SHA256" \
  --writers-stopped \
  --old-infrastructure-fenced
```

The command rechecks the backup, exact staged database, schema/storage identity,
live objects and quarantine journal. It then checks current state: all restored
tokens are revoked, old nodes disabled, file workspaces draining or deleted, pending
native candidates cancelled, stop requests retained, and old GitHub bindings and
event channels revoked. A historical quarantine receipt cannot bypass these checks.
No repository authority, epoch, branch, source history or validation is rewritten.

The only output location is `STAGE/active`, which must not exist. It contains:

- `objects/`: a separate, fully copied and reverified local store. Future writes
  use this directory, preserving `STAGE/backup` as an unchanged backup.
- `private/`: an empty owner-private directory for new operational state.
- `service.json`: private configuration pointing at the staged database and new
  object root/socket. It retains service limits from the backup and uses the
  PostgreSQL role/connection used for staging, with an explicit target database.
- `configured.json`: the final completion receipt. No database password or
  TreeHarbor bearer secret is printed to stdout.

Reserve disk for an additional complete object copy. Metadata writers are excluded
while this offline copy is made. Verification/copy passes use the existing byte,
entry and shared deadline budgets; filesystem IO may block. The generated Unix
socket path must fit within 100 bytes. Use a short private staging path.

## Start with a new reader

Only after the command succeeds and `configured.json` is present, inspect the
configuration and provision explicitly selected credentials. The command does not
issue credentials, grant API permissions, or launch a process. For example, with
an existing tenant/repository from the recovered deployment:

```sh
TH_CONFIG=/srv/treeharbor-recovery/stage-001/active/service.json
TH_SOCKET=/srv/treeharbor-recovery/stage-001/active/service.sock
TH_PRIVATE=/srv/treeharbor-recovery/stage-001/active/private
: "${TH_TENANT:?Set the recovered tenant}"
: "${TH_REPOSITORY:?Set the recovered repository UUID}"

treeharbor admin --config "$TH_CONFIG" principal-create \
  --tenant "$TH_TENANT" --principal recovery-reader --kind human
treeharbor admin --config "$TH_CONFIG" access-set \
  --tenant "$TH_TENANT" --repository "$TH_REPOSITORY" \
  --principal recovery-reader --permission read
treeharbor admin --config "$TH_CONFIG" token-issue \
  --tenant "$TH_TENANT" --repository "$TH_REPOSITORY" \
  --principal recovery-reader --permission read --ttl-seconds 3600 \
  --output "$TH_PRIVATE/reader.json"

treeharbor serve --config "$TH_CONFIG"
```

Keep `serve` in the foreground or supervise it with your existing service manager.
From a second terminal, inspect the recovered repository:

```sh
TH_SOCKET=/srv/treeharbor-recovery/stage-001/active/service.sock
TH_PRIVATE=/srv/treeharbor-recovery/stage-001/active/private
treeharbor client --socket "$TH_SOCKET" \
  --credential-file "$TH_PRIVATE/reader.json" repository-info
```

Check accepted changes and export the required history before enabling new write
access. Create fresh node identities and storage; do not copy or resume old worker
journals. Fresh agent/validator/integrator permissions and approved runtime programs
remain explicit setup steps. Frozen/retired repositories are not automatically
reactivated. Old workspace cleanup is retained for a later recovery procedure;
configuration does not fabricate runtime removal or delete old volumes.

Connected repositories retain their proposed changes but their previous bindings
remain revoked. This command does not reconcile GitHub refs, PRs, queue/check
attempts or effects occurring after the backup. Keep publishers off until a
separately controlled reconciliation/reconnection procedure is completed.

### Development builds after 0.1.11

The server now enforces the reconnection restriction for repositories recorded by
`restore-quarantine`, including journals created by 0.1.11. `repository-info`
reports `github_reconciliation_required: true` for the restored Connected
repository. A new GitHub binding receives HTTP 409 with `host_recovery_required`.
New credentials, a different binding ID, service restart or deleting the local
quarantine receipt do not remove the durable restriction. Other repositories in
the same installation can still connect normally.

This guard is not included in the published 0.1.11 package, whose operator must
keep publishers disconnected as described above. The development guard preserves
source reads/export and historical inspection; it does not perform reconciliation
or provide an override. Do not delete recovery journals to bypass it. Use the
separate [operator-assisted reconnect protocol](restore-reconnect.md) in development
builds; its local checks do not independently verify external fencing.

The development [GitHub inventory command](restore-github-inventory.md) can collect
current heads and all PR states for that investigation. It does not clear this
restriction or prove that external effects have been reconciled.

## Failures and retries

Existing complete or partial `active/` output is never overwritten or silently
resumed. Failure leaves output for inspection. Do not start a service merely
because `service.json` exists: completion can fail before the final receipt is
published. Diagnose the failure before deciding whether to remove only this new
partial output; never remove the retained backup or target database automatically.

The completion receipt records a point-in-time check and the operator's fencing
acknowledgment, not proof of external fencing or permission for future arbitrary
administrative changes. It reports `service_started: false`,
`credentials_issued: false`, `external_fencing_verified: false` and
`github_reconciled: false`. Backup integrity and source retention are separate from
runtime isolation and external publication authority.

Development builds after 0.1.11 include a separate
[operator-assisted reconnect protocol](restore-reconnect.md). It requires explicit
external assertions, fresh probe/inventory and atomic local re-review. Configuration
alone still cannot clear quarantine; the published 0.1.11 behavior is unchanged.
