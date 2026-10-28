# Local file-preparation node

The development node now connects native workspace reservations to real files and
database acknowledgment. It runs on Linux/macOS using the same Rust CLI, local
PostgreSQL and local object backend as the server. The commands in this guide only
prepare files. Linux [managed runs](managed-runs.md) add bounded runtime execution
through a separate operator-approved profile and registered ext4 volume. There is
no persistent scheduler or remote node transport yet.

## Prepare an existing native workspace

First follow the [local service](local-service.md) and [native import](native-import.md)
guides: register a native base and profile, then create a workspace through the
authenticated client. The server configuration must include the same `storage`
root/backend used by source import. Run node commands as a trusted service operator;
the database configuration and node credential must never be given to agents.

The examples reuse `TH_STATE` and `TH_REPOSITORY` from the local guide. Keep the
state directory private and persistent. Initialize a **new** node root:

```sh
./target/debug/treeharbor node initialize --root "$TH_STATE/node" \
  --tenant example-team --principal node-local
```

Save the returned `storage` UUID in your operator configuration. Use that value
as `TH_STORAGE` below; it is a persistent identity, not a secret. Initialization
never replaces an existing directory. Opening an existing node requires its exact
identity, owner and physical root; missing or changed records fail closed.

```sh
TH_STORAGE=THE_RETURNED_STORAGE_UUID
./target/debug/treeharbor admin --config "$TH_STATE/config.json" principal-create \
  --tenant example-team --principal node-local --kind node
./target/debug/treeharbor admin --config "$TH_STATE/config.json" node-register \
  --tenant example-team --principal node-local --storage "$TH_STORAGE"
./target/debug/treeharbor admin --config "$TH_STATE/config.json" access-set \
  --tenant example-team --repository "$TH_REPOSITORY" --principal node-local \
  --permission runtime.control
./target/debug/treeharbor admin --config "$TH_STATE/config.json" token-issue \
  --tenant example-team --repository "$TH_REPOSITORY" --principal node-local \
  --permission runtime.control --output "$TH_STATE/node-credential.json"

./target/debug/treeharbor node prepare-once --config "$TH_STATE/config.json" \
  --credential-file "$TH_STATE/node-credential.json" --root "$TH_STATE/node" \
  --storage "$TH_STORAGE"
```

`prepare-once` claims one eligible native workspace in the credential's repository.
It returns `files_ready`, workspace ID, the private file path, baseline manifest,
snapshot, and `runtime: null`. No eligible job returns `idle`. After process loss,
a running job becomes reclaimable when its 60-second lease expires; a live worker
renews every 15 seconds. `idle` does not mean every queued workspace is complete.

To prepare one exact workspace, add `--workspace WORKSPACE_UUID` to
`node prepare-once`. It can be combined with `--slot SLOT_UUID`; the workspace
selects the queued job, while the slot selects its bounded placement. A slot by
itself does not select a workspace. Omitting `--workspace` retains normal
oldest-eligible scheduling.

The selection remains scoped to the authenticated repository, node and storage.
Unknown, foreign-scope, assigned-to-another-node, actively leased or already
prepared work returns `idle` without preparing another workspace. Nil UUIDs are
rejected. An expired selected lease can be reclaimed on its assigned node with
the original attempt and slot; an old worker remains fenced. Selection does not
override authorization, authority epochs, drain or lease checks. Use `prepared`
below to reopen acknowledged files; selecting a completed preparation does not
rebuild it or restart a task.

Reopen an already acknowledged workspace in a new process:

```sh
./target/debug/treeharbor node prepared --config "$TH_STATE/config.json" \
  --credential-file "$TH_STATE/node-credential.json" --root "$TH_STATE/node" \
  --storage "$TH_STORAGE" --workspace WORKSPACE_UUID
```

This command preserves edits, deletions and untracked files. It requires a current
file-ready placement with no runtime binding. Cached credentials, another node,
draining state or an old repository epoch cannot authorize reopening. A changed
baseline, intent or publication receipt is retained and reported as an error.

The returned directory is **files, not a sandbox**. These commands do not provide
process/network isolation or CPU/RAM/disk limits on later writers. `--max-workspace-bytes`
limits source materialization (4 GiB by default); it is not a filesystem quota or
a licensing limit. The external runtime integration remains a separate release gate.

## Ownership and recovery

One OS lock owns a storage root. A second process cannot open that root concurrently.
Clones of one in-process `Node` share bounded capacity and per-slot exclusion; the
library supports concurrent preparations of distinct workspaces. `prepare-once` is
a single-job CLI, not a daemon. Cancellation retains both storage and slot guards
in outstanding filesystem tasks until their actual writes/cleanup stop.

The node persists the exact reservation intent before source IO. A staging tree is
private and unexposed. After file/manifest sync, a publication record binds its exact
manifest bytes to that intent. An atomic no-replace rename publishes `tree/`; both
rename parents are synced before database acknowledgment. An IO error after rename
has an uncertain outcome and never permits replacing or deleting the destination.

| Failure boundary | Retry behavior |
|---|---|
| Before a durable intent | Only private unpublished record temporaries can be removed; unknown data is retained |
| During staging, before publication | Reclaim the same attempt/slot, exclude all old writers, remove only that attempt's unpublished staging and rebuild |
| After publication, before database acknowledgment | Verify the intent, publication record and exact baseline; reuse the same directory and acknowledge with a current lease |
| After database commit, before a response | Inspect the current ready placement and reopen its files; do not execute the job again |
| Drain, revocation or epoch change | Refuse late acknowledgment/reopening and retain data/pins for later safe cleanup |

Renewal and primary work are polled concurrently, including while filesystem work
is blocked. A renewal waiting for a database lock cannot suspend the transaction
which must release that lock. Ready-state replay is an authorized inspection of
the identical placement, not permission for a stale worker to complete a job.

The current root identity also records its physical device/inode. Copying or
recreating a root does not silently create another active node. Relocating/restoring
it onto a different physical identity requires a future fenced migration procedure.
Keep the root's ancestor directories under operator control. Same-OS-user hostile
processes, copied credentials, network filesystems with unverified locking semantics,
and hostile host administrators are outside this local ownership boundary.

Explicit [workspace cleanup](workspace-cleanup.md) uses observed stopped runtime
writers and a durable deletion journal before releasing slots/pins. Automatic
scheduling and cleanup of retained journals/artifacts are not implemented.

## Local records v1

```text
node/
  identity.json
  node.lock
  slots/<slot-uuid>/
    intent.json
    staging/<tenant>/prepare-*/
    tree/
      publication.json
      manifest.json
      files/
```

All metadata stays outside `files/`. Identity fields are `version`, `tenant`, `node`,
`storage`, `device`, `inode`. Private records reject unknown/duplicate fields and
are bounded on read. The lock is not deleted/recreated during opening.

`intent.json` has version 1 and binds tenant, repository, node, storage, slot,
attempt, workspace, generation, authority, epoch, revision, snapshot and profile.
Its exact encoding is the compact UTF-8 JSON in
[`intent-v1.json`](../crates/node/tests/fixtures/intent-v1.json), in that field order,
without a trailing newline. Integers are decimal; UUIDs are canonical lowercase;
IDs use their explicit type prefixes. The binding is SHA-256 of those exact bytes.
The independent fixture digest is
`bf3f38a95aac00da06f5185ae8a100b49fb9c0b313700b36d94547b90f02e6bd`.

`publication.json` is compact JSON with `version: 1`, `binding: "sha256:…"` and
`manifest: "sha256:…"`, followed by a newline. It records the exact manifest hash,
not current mutable file contents. Both IDs must match the trusted context.
[Manifest v1](workspace-manifest-v1.md) itself remains unchanged; identical source
baselines in two workspaces have distinct placement bindings.

Tests use actual files/PostgreSQL, process kills at three boundaries, inode-preserving
recovery, concurrent workspaces, real lease renewal, overlapping database locks,
cancelled queued IO, private-root substitution and an actual CLI/HTTP import-to-files
flow with Git absent from PATH. These are development correctness tests. They do
not prove power-loss behavior on every filesystem, runtime isolation of these
workspaces, a complete authority mode or a performance advantage.
