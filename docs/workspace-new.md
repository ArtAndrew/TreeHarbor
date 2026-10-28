# Create a workspace without managing its UUID

Available in the signed [0.1.2-alpha.1 preview](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.2-alpha.1).
`client workspace-new` saves a workspace UUID and operation key before sending the
existing creation request. Rerun the same command with the same state file after
a connection failure. It requests file preparation; it does not run an agent,
create a sandbox, publish a PR or indicate that files are ready.

## One state file per task

First configure the [local service](local-service.md), import an exact source
revision through [GitHub](github-import.md) or the applicable [native import](native-import.md)
path and register a full/sparse [source profile](source-profile-v1.md). Use the
imported `native:sha256:...` revision and the returned profile digest. The scoped
credential needs `workspace.write`; no publisher or database credential is needed.

Keep the request file in a persistent directory owned by you with mode `0700`.
The command writes a private `0600` file and refuses overwrite, aliases, changed
inputs and corrupt state. Set `TH_BIN` to your installed executable and use the paths
and current authority epoch from your own configuration:

```sh
"$TH_BIN" client --socket "$TH_SOCKET" --credential-file "$TH_AGENT_CREDENTIAL" \
  workspace-new --state "$TH_STATE/task-A.json" \
  --authority connected --epoch 1 --base "$TH_BASE" \
  --profile-digest "$TH_PROFILE"
```

The JSON response contains `result.workspace`, `result.generation`, the exact base
and `state: requested`. Use the workspace ID with [selected node preparation](local-node.md)
and `workspace-status`. The `requested` response is a historical creation receipt:
replaying it after preparation or draining does not assert the workspace's current
status. Profile selection controls delivery, not repository access permissions.

For another task, choose a new state file. Preserve the first task's file:

```sh
"$TH_BIN" client --socket "$TH_SOCKET" --credential-file "$TH_AGENT_CREDENTIAL" \
  workspace-new --state "$TH_STATE/task-B.json" \
  --authority connected --epoch 1 --base "$TH_BASE" \
  --profile-digest "$TH_PROFILE"
```

Use separate scoped credentials when A and B represent different agents. A
Standalone repository uses `--authority standalone`; this does not switch the
repository's mode. The server checks current authorization and authority/epoch.

## Retry and recovery

- After a timeout or process crash, retain the file and repeat the original command.
  The server's existing idempotency contract handles the same request. The client
  does not automatically retry or generate a replacement identity on error.
- Concurrent calls using the same file share the first durably published intent.
  Different files represent different tasks, even if their source is identical.
- Changing the repository scope, authority, epoch, base or profile requires an
  explicit new task. A mismatch leaves the existing intent intact and sends no
  creation request. Do not edit the saved UUID/key to work around an error.
- If the state file is lost, recover the original workspace and operation key from
  your retained records. Creating a new file cannot establish that the earlier
  request failed. Existing workspaces are not automatically drained or deleted.
- The state contains scope and request identities, not credentials. Keep it outside
  agent workspace files and include it in the operator's independent backups.

Existing `workspace-create --workspace ... --key ...` remains available for callers
that already manage their own request identities. This command does not automate
node/volume registration, runtime provisioning, capture or the two-agent helper.
