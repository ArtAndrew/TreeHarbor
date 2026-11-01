# Quarantine restored work

Unreleased development command. This follows [restore staging](restore-stage.md);
released 0.1.10 packages do not contain it. It does not authorize activation.

Revoking old tokens alone is insufficient: a newly registered worker could claim
an old workspace that was not yet assigned to a node. Quarantine retains that
work and its source while withdrawing its permission to continue automatically.

Keep the original deployment fenced and the staged database isolated. Stop all
services, workers, publishers, cleanup/GC and administration writers. These are
operator responsibilities; the acknowledgment below does not perform or verify
external fencing. Use a private staged directory from your own trusted backup and
the manifest digest retained independently when that backup was created.

```sh
treeharbor backup restore-quarantine \
  --stage /private/treeharbor/recovered \
  --manifest-sha256 "$BACKUP_MANIFEST_SHA256" \
  --writers-stopped
```

The command verifies the copied inventory, target identity, schema and object
catalog. It requires every restored access token to remain revoked, and refuses
conflicting database writers. All database changes commit in one transaction:

- File workspaces still requested, ready or in use move to `draining`, with a new
  generation. Their preparations also drain. Files and source pins are retained.
- Unremoved managed runs receive a stop request. No exit status, runtime removal,
  successful check or cleanup receipt is fabricated.
- Pending native queue entries and candidates are cancelled with the reason
  `restore_quarantine`. Accepted proposals, published refs, complete reachable
  history and existing check/review evidence remain intact. Requeue a proposal
  explicitly and check its new candidate when normal operation is authorized.
- Previous node principals are disabled. Replacement nodes require new identities
  and storage registration; copied node journals grant no execution permission.
- Active GitHub bindings and event channels are revoked. Publication attempts,
  uncertain outcomes and signed events remain available as records. No request is
  sent to GitHub. Reconciliation and controlled reconnection remain separate work.

API-only task contexts are retained; TreeHarbor does not execute them. Uploads
are not marked completed and background events are not marked done. Repository
state, authority and epoch do not change. Frozen/retired repositories stay so.
A database without any repositories is refused; initialize a fresh deployment
instead of using this operation for an empty installation.

Success prints `status: quarantined` and creates private `quarantined.json`.
The result includes counts and reports `activation_authorized: false`. No service
configuration or credentials are generated, and no process is started.

If the response or local receipt is lost after commit, repeat the identical
command while the target remains stopped and no new tokens have been issued.
The database journal returns the original result without draining twice. A changed
repository set, conflicting journal, live token or damaged backup is refused.
Keep partial output for inspection; never infer activation from a directory name
or from the presence of an older staging receipt.

This command is one recovery step. Safe service activation, recovery cleanup of
old node storage and full Connected external-state reconciliation are unfinished.
