# Experimental work selection on one node

`node work-selected` is an operator command in the upcoming Copy-on-Write alpha for a bounded
group of already authorized operations. It shares one node process across
managed runs, stopped-file captures and workspace cleanup. It supports ordinary
placements and the [Copy-on-Write preview](experimental-cow.md).

The owner/controller still creates runs, requests captures, accepts changes and
requests workspace deletion through the existing authenticated API. The node
does none of those actions on the owner's behalf. In particular, an entry in
this file is not permission to delete a workspace or accept its changes.

## Select exact operations

Save a private JSON file owned by the node operator, with mode `0600`:

```json
{
  "version": 1,
  "runs": ["d9480199-f366-4514-ac33-682da953b6bb"],
  "seals": ["a142f86e-fda1-485b-89d2-a982ccad5f1e"],
  "cleanups": ["ddf6fe0d-2d39-4591-a1ed-35c9c8dfe3cf"]
}
```

Replace these illustrative UUIDs with the persisted run ID, planned capture ID
and workspace ID. A capture ID may be selected before its owner requests that
capture, but the owner must wait for runtime removal before making the request.
All three arrays are required; unused arrays may be empty. There must be 1–16
operations in total, no nil IDs and no duplicate IDs within an array. The input
is limited to 4 KiB. Unknown fields and versions are rejected.

Using the existing node configuration, storage identity and scoped credential:

```sh
treeharbor node work-selected \
  --config /private/treeharbor/config.json \
  --credential-file /private/treeharbor/node-credential.json \
  --root /private/treeharbor/node \
  --storage "$STORAGE_ID" \
  --selection /private/treeharbor/selected-work.json \
  --engine-socket /run/docker.sock \
  --parallelism 4 \
  --wait-seconds 120
```

The optional `--cow-driver` requires the same protected, operator-provisioned
helper as the preview preparation and cleanup commands. No mount paths or
privileges are supplied by an agent. The runtime socket and credentials remain
outside exposed workspace files.

## Execution and results

Each selection targets only its exact ID on this node and storage instance.
Unavailable work never falls back to another pending task. Per-slot guards
serialize operations on the same physical placement; the existing parallelism
limit bounds active operations across different placements. Waiting for a
future owner request does not reserve a slot.

The command can capture and clean up a stopped task while another task on a
different slot continues. Cleanup still requires an explicit owner drain,
finished managed execution and actual runtime removal. Ready capture results
still require separate owner acceptance as immutable ChangeSets.

`--wait-seconds` bounds waiting for unclaimed operations, from command startup
(default 30, maximum 3,600 seconds). Choose enough time for the owner's planned
requests. This is not a deadline for cancelling an active run, capture or
cleanup: once claimed, those operations finish through their ordinary
supervised paths. Runtime profiles continue to bound execution time.

The final JSON array identifies each operation by `kind` and `id` and includes
its original result under `result`. Ordering is not significant. An unclaimed
selection reports `{"state":"idle"}`; this is **not proof of completion**.
Read the original run/capture/workspace status with the owner's credential.

An operation error does not cancel other selections. The command waits for them,
prints their results and exits unsuccessfully if any selected operation failed.
Do not interpret the command's exit code as the state of every operation.

Retry the same IDs after an uncertain outcome. Completed runs and captures may
report idle; completed cleanup returns its original receipt without reopening
old files. The selection cannot launch a new execution identity or publish a
second capture simply because it is delivered again.

This command is not a resident scheduler. Killing its process is not proof that
managed runtimes stopped. Existing run recovery, node identity, runtime journal
and lease rules still apply. After proving the old worker has exited and allowing
its database leases to expire, retry the same selection against the same intact
node and volumes. The runtime journal reconciles the original execution attempt;
do not create new run IDs to recover an uncertain attempt. The local Linux drill
verifies that both original containers are reclaimed after worker termination.
Automatic source-image rotation, remount after
reboot and replacement of a missing node or volume remain unfinished.
