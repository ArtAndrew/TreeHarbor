# Experimental local runtime controller

The `treeharbor-runtime` Rust library creates, inspects, starts and removes one
external sandbox using Docker Engine API 1.47 over a local Unix socket. It selects
`runsc` explicitly and uses the [trusted task supervisor](runtime-supervisor.md).
This is an internal operator API. It is **not yet connected to service-managed
AgentRuns**, authorization, node leases or workspace drain. No agent-facing launch
endpoint or production node daemon is provided by this increment.

## Supported boundary

The controller requires Linux x86-64, a rootful Docker Engine with cgroup v2 and
runsc, and an immutable local Linux/amd64 image ID containing trusted `/init`.
It never pulls an image or accepts arbitrary Docker options. Image registration
and provenance remain the operator's responsibility. The root-owned engine socket
and its peer are checked; socket replacement is rejected. Engine and privileged
host administrators are trusted. An agent must never receive this socket, node
credentials, journal directory, or source-store mount.

Each task uses a private directory on a separate operator-provisioned bounded
ext4 filesystem. The controller verifies device, inode, mount identity, capacity,
nodev/nosuid and private propagation, rejects nested mounts, and retains directory
handles and an exclusive mount lock. A normal directory and a capacity setting
alone do not satisfy this contract. The journal must be on a different filesystem,
outside the writable volume. Privileged provisioning and recovery after host
remount/replacement are not automated. Hostile privileged mount changes are outside
this trust boundary.

The controller verifies image, deterministic name, attempt/binding/profile labels,
command, capabilities, network, root filesystem, resource and log limits, restart
policy, and the exact workspace mount before adoption or execution. It then acts
by full container ID. A mismatch retains the resources for operator investigation.
The image starts as restricted root only to drop task/supervisor identities; task
code runs as the node's non-root numeric UID/GID inside the external sandbox.

The profile enforces read-only root, no network, one writable workspace, no added
swap, bounded CPU/RAM/PIDs, bounded temporary files and logs, and no automatic
restart. The supervisor owns the task deadline and descendant teardown. These
controls depend on Docker, gVisor, the host kernel and the mounted filesystem;
TreeHarbor does not implement a replacement isolation runtime.

## Recovery semantics

`Controller::open` requires an exact durable intent and exclusive journal/volume
ownership. Its caller must obtain current repository/workspace/run authorization
before invoking the library. The opaque 32-byte binding is supplied by that trusted
caller; it is not an authorization token or a Validation.

| Operation | Durable effect and recovery |
|---|---|
| `create` | Record create-issued before the RPC. Recover a lost response by deterministic name, verify the entire specification, and save the full ID. Never create a different attempt to bypass an unknown outcome. |
| `start` | Record start-issued before the RPC. A repeated request observes the same running/exited container; it never issues another start. A still-created container after an uncertain start remains unknown. |
| `status` | Inspect the owned container and retain uncertainty on disappearance or configuration drift. Local intent alone never reports running. |
| `remove` | Record remove-issued, remove only the verified full ID, observe absence, then record removed and release the volume assignment. A lost removal response can be reconciled. |

Explicit Docker start can reexecute an exited container even with restart policy
`no`. Therefore an uncertain start is not automatically retried. This favors
non-duplication over availability. A request delayed in transport may still execute
after its client dies; removal by the old full ID fences that late start before a
volume can be reused. An unknown create without an observed ID retains ownership.
A terminal cleanup replay never removes a successor's volume assignment.

Journal/volume locks serialize local owners. Immutable records use synced files,
no-clobber publication and directory sync. Incomplete journal initialization,
corrupt markers and inconsistent marker ordering fail closed. Operator-assisted
repair and recovery across engine/host replacement remain future work. A stopped
container, its exit code and its output are **not trusted source validation**.

## Profile and journal formats, version 1

The profile is compact UTF-8 JSON with this fixed field order, no whitespace or
trailing newline: `version`, `image`, `uid`, `gid`, `memory_bytes`, `nano_cpus`,
`max_processes`, `timeout_ms`, `workspace_bytes`. Unknown fields and noncanonical
bytes are rejected. `image` is `sha256:` followed by 64 lowercase hexadecimal
characters. UID/GID are 1..2147483647 except 65534. Bounds are 128 MiB..16 GiB RAM,
0.1..16 CPU, 4..4096 task processes, 100 ms..24 hours, and 64 MiB..64 GiB workspace.
The profile digest is SHA-256 of `treeharbor-runtime-profile\0v1\0` followed by
canonical bytes; `\0` denotes a zero byte.

The task is 1..128 UTF-8 arguments, at most 60 KiB total argument bytes, no NUL,
and an absolute first executable path. Arguments are never interpreted by a host
shell. The immutable journal `intent.json` is compact JSON with recursively sorted
object keys: version, engine ID, attempt UUID, lowercase binding digest, profile
object, argv and physical volume identity. The volume includes mount/files paths,
device, both inodes, mount ID, measured capacity and allowed capacity. Changing
these bytes refuses to reopen the old attempt.

`treeharbor-execution.json` on the volume root contains version 1, attempt UUID
and SHA-256 of exact intent bytes, with sorted keys. The journal contains
`task.lock`, `container.json` (`id` and `version`, sorted keys), and immutable
`create-issued.json`, `start-issued.json`, `remove-issued.json`, `removed.json`
markers. Each marker is exactly `{"version":1}` followed by one newline.
A container ID requires create-issued; start/remove-issued require a saved ID;
a created attempt cannot be removed without remove-issued. Removal of an attempt
that never issued create is allowed. Temporary unpublished writes confer no state.
Records are private regular files with one link. These formats are experimental
and versioned; do not edit journals or copy them between nodes.

Independent fixtures are in `crates/runtime/tests/fixtures/`: profile and intent
bytes plus SHA-256 values, derived using Python's standard JSON/hashlib rather than
the Rust serializer. The profile object inside the sorted intent has sorted keys,
which intentionally differs from standalone profile field order.

## Verification

Ordinary Rust tests cover canonical vectors, invalid inputs, mount-record
rejection, journal exclusion, reopen, conflicting intent and unsafe/corrupt files.
The separate real-runtime CI harness runs only on a disposable Linux runner:

```sh
cargo build -p treeharbor-runtime --locked --target x86_64-unknown-linux-musl --bins --examples
python3 scripts/test-runtime-controller.py
```

It uses a real root-owned Unix proxy to delay/drop actual Docker replies and
requests. It fabricates no successful runtime response. It kills the controller
at create/start/remove boundaries, checks that the task executes only once,
exercises two independent same-path writers, retained volume assignments, stale
cleanup and configuration drift. The existing supervisor stress suite remains
required. This is correctness testing, not a performance benchmark or security
certification. The example driver is a test tool, not the product CLI.
