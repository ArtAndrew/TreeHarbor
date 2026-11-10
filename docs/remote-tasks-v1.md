# Remote task assignment

Development builds with schema 033 can dispatch a pinned task to external
controllers. Use a separate agent principal for each worker and keep its
credential outside the execution environment. The controllers supply runtime
isolation, process stop and local file cleanup. TreeHarbor records source,
assignment and accepted changes.

## Queue and claim a task

Provision each worker with `read`, `workspace.write` and `change.write` in the
repository. The operator uses `repository.write` on the private Unix socket to
enqueue or cancel. The task names eligible workers, an exact registered native
base and a registered delivery profile. Use distinct task UUIDs for independent
tasks, even when they share the same base.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$OPERATOR_TOKEN_FILE" \
  remote-task-enqueue --key "$ENQUEUE_KEY" --authority connected --epoch 1 \
  --task "$TASK" --base "$BASE" --profile-digest "$PROFILE" \
  --agent migration-controller/1 --label update-imports \
  --worker worker-a --worker worker-b
```

On a worker, define `th` as the [HTTPS client](agent-https.md#connect-an-agent-controller)
with that worker's credential and trusted CA. Save the claim key before sending.

```sh
th remote-task-claim --key "$CLAIM_KEY" --authority connected --epoch 1 \
  --task "$TASK" --ttl-seconds 120
th remote-task-status --task "$TASK"
```

Omit `--task` to claim the oldest eligible task in this repository and epoch.
An `empty` result allocates nothing. A successful claim returns the worker,
attempt, lease deadline, run, workspace, generation, base and profile. Check
that status still shows the same live attempt before starting execution. On
an uncertain response, repeat the same key and arguments. A replay returns
the original receipt, even after the attempt expires or the task completes.
Keep a durable local execution record per attempt to resume after disconnects.

```sh
th source-checkout --workspace "$WORKSPACE" --generation "$GENERATION" \
  --authority connected --epoch 1 --output "$PRIVATE_ROOT/task-files"
th remote-task-renew --key "$RENEW_KEY" --authority connected --epoch 1 \
  --task "$TASK" --attempt "$ATTEMPT" --ttl-seconds 120
```

Use a fresh key for each new renewal. The database clock determines expiry.
Each renewal can extend the deadline of the current live attempt. Stop execution
on lease loss and preserve any files that need operator inspection. An expired
attempt can still have a process running on its controller. The next worker
gets its own run and workspace, and the previous worker's source reads and new
proposal writes are fenced. A worker principal gets one attempt per task.
If all listed workers have already tried, the operator can cancel and create a
new task after reviewing the previous attempts.

Submit a new ChangeSet with the returned run and workspace generation through
the [proposal API](change-proposals.md). The first accepted version completes
the task and closes its logical workspace in the same transaction as the change
and receipt. Retained ChangeSet pins are independent of that workspace. Query
`remote-task-status` for its exact ChangeSet and version. Review and integration
follow the repository's usual GitHub or native path. Use `standalone` instead
of `connected` when TreeHarbor is the repository authority.

An operator can cancel a queued or leased task with `remote-task-cancel`
using the socket, original task UUID and current authority and epoch. This
closes any logical attempt. Accepted tasks are terminal. Direct API close
and workspace deletion of an assigned attempt are refused so the task and
workspace cannot diverge. Controllers remove their own stopped files.

## Version 1 commands

Use the existing `/v1/commands` envelope with repository-scoped authentication.
Only claim, renew and status are exposed on the restricted HTTPS listener.

| Method | Fields | Permission |
| --- | --- | --- |
| `remote_task_enqueue` | `key`, `authority`, `epoch`, `task`, `base`, `profile_digest`, `agent`, `label`, `workers` | `repository.write` |
| `remote_task_claim` | `key`, `authority`, `epoch`, optional `task`, `ttl_seconds` | `workspace.write` |
| `remote_task_renew` | `key`, `authority`, `epoch`, `task`, `attempt`, `ttl_seconds` | `workspace.write` |
| `remote_task_status` | `task` | `read` |
| `remote_task_cancel` | `key`, `authority`, `epoch`, `task` | `repository.write` |

Task IDs are non-nil UUIDs. A task can name up to 16 distinct, repository-scoped
agent principals. Lease durations range from 1 to 3,600 seconds. Task and agent
labels are data and never provide a command line. Status is scoped to the task's
operator and eligible workers. Only the operator and current worker receive
that attempt's workspace and run IDs. For an expired lease, `state` remains
`leased` and `lease_active` is false. Source and proposal operations check the
lease again under repository authorization. Revocation and expiry cannot retract
bytes the controller already received.

The [golden vectors](../crates/metadata/tests/fixtures/remote-task-v1.json) bind
the enqueue and claim requests to their canonical operation digests. An `empty`
claim creates no receipt and does not consume the key. Successful mutating
requests preserve their original results. Treat old receipts as history and
query status before using an assignment. The attempt counter is independent of
workspace generation. A ChangeSet receipt includes `remote_task.task` and
`remote_task.attempt`. This records provenance without claiming runtime or
validation evidence.

Schema 033 adds the task and immutable attempt tables. Stop writers and take a
consistent backup before migration. Upgrade the server and clients together
when using these commands. Existing external API contexts keep their current
behavior. During recovery, fence the old deployment and revoke restored
credentials before reconnecting workers. Task leases alone cannot stop an old
deployment from writing to a separate database.
