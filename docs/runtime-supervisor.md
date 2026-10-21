# Experimental runtime task supervisor

`treeharbor-runtime-init` supervises one command **inside an existing Linux
sandbox**. It does not create a sandbox, authorize a workspace, allocate a volume,
or report trusted validation results. The service does not yet launch agents.

The supervisor is built as a static `x86_64-unknown-linux-musl` executable for the
runtime tests. The trusted operator must keep this binary and its image immutable;
do not load it dynamically from an untrusted image or make it writable by a task.
Only Linux x86-64 with the pinned gVisor profile below is currently exercised.
macOS builds reject execution; they are not an alternative isolation backend.

## Invocation protocol 1

The argument order is fixed:

```text
/init --protocol 1 --timeout-ms 10000 --max-processes 64 --uid 1001 --gid 1001 -- /absolute/program arg...
```

The trusted runtime controller supplies these arguments. Timeout is 100 through
86,400,000 milliseconds, and the process limit is 4 through 4,096. The complete
argument list is bounded to 140 arguments and 65,536 native argument bytes. Paths
and command arguments are passed directly to the program, without a host shell.

The trusted bootstrap must be namespace PID 1, initially UID/GID 0 with matching
real/effective/saved/filesystem IDs, no extra groups, exactly SETUID and SETGID in
permitted/effective/bounding capabilities, empty inherited/ambient sets, default
securebits, one thread and `no-new-privileges` enabled. It rejects execution if
these observations fail. These
checks do **not** identify or attest gVisor: the external controller must configure
and verify the runtime. Do not use this executable as the host's init process.

Before executing the command, the supervisor disables its own dumpability, disables
core dumps and sets both the soft and hard `RLIMIT_NPROC`. It launches the command
under the supplied non-root UID/GID, clearing supplementary groups and dropping
capabilities before entering `/workspace` and executing any task code. Task IDs
must be 1 through 2,147,483,647, excluding the reserved supervisor ID 65534.
The single-threaded supervisor then irrevocably drops its own UID/GID to 65534,
clears groups, disables dumpability again and verifies empty permitted/effective/
ambient capabilities. Only the inert bounding SETUID/SETGID bits remain; empty
inheritable sets and no-new-privileges prevent regaining them via execution.

**Distinct UIDs are required:** the pinned gVisor test allowed a same-UID task to
kill its container PID 1. Do not rely on PID-1 signal rules alone to protect the
deadline. The signal test now requires EPERM for STOP, KILL and TERM from the task.

It starts the command in
`/workspace` with only `PATH=/usr/local/bin:/usr/bin:/bin`, `HOME=/workspace` and
`TMPDIR=/tmp`. It does not pass through image environment variables or service
credentials. A future explicit environment/secret profile is separate work.

The supervisor reaps adopted children. On primary-command exit, or when its
monotonic deadline expires, it exits; the runtime must then terminate the whole PID
namespace, including descendants in other process groups. There is no dependency
on a live client connection or a polling worker. This is a cancellation mechanism,
not a hard real-time guarantee during host starvation, suspension or stalled IO.
The controller must still reconcile the **actual stopped runtime** before sealing
or removing a workspace. Restarting a container restarts the deadline; the future
controller must fence restarts against the persisted attempt and workspace state.

Exit status is the primary command's status, or 128 plus its terminating signal.
The supervisor uses 124 for timeout and 125 for its own errors. A command can also
return these numbers; neither exit codes nor stdout/stderr are authenticated
evidence of the cause, and they must not directly become an accepted Validation.

## Real-runtime test profile

The `gVisor task supervision` CI job builds the init and an adversarial Rust test
fixture into a scratch image by immutable local image ID. There is no shell, Git,
remote base image or registry credential in that image. gVisor
`release-20261012.0` (published 2026-10-16) is installed from its complete archive,
with a pinned SHA-512 checksum. The test runner is disposable Ubuntu 24.04.

The harness configures and inspects these external controls:

| Control | Mechanism in this test profile |
|---|---|
| Kernel boundary | Docker explicitly selects `runsc`; no fallback to the default runtime |
| Files | Read-only root image, one writable workspace bind, no engine socket or source-store mount |
| Privileges | Restricted root bootstrap with only SETUID/SETGID; task and supervisor drop to distinct non-root IDs, no-new-privileges |
| Network | `none`, no published ports |
| Memory / CPU | 256 MiB, no additional swap, one CPU of quota |
| Processes | Host cgroup limit 256; separate guest RLIMIT_NPROC 64 |
| Temporary files | `/tmp` tmpfs 16 MiB, `/dev/shm` 8 MiB |
| Persistent workspace | Separate 128 MiB ext4 loop filesystem per test slot, nodev/nosuid |
| Output | Docker local log driver, one 1 MiB log file |
| Restart | Disabled |

The dedicated filesystem supplies the persistent capacity bound. Docker's RAM
limit and root-layer storage options do not bound an arbitrary writable host bind
directory. The test filesystem persists after a task exits; mounting/provisioning
production workspace slots and recovering their ownership are **not implemented**.
Image storage, metadata, supervisor overhead, logs and cleanup are additional host
costs; these settings are not a whole-node disk budget.

The script exercises concurrent same-path edits, filesystem/network/credential
denial, rejected invalid bootstrap configurations, primary exit propagation, autonomous timeout,
PID-1 signal/proc attacks, descendants, guest process exhaustion, memory exhaustion,
and a full persistent volume without corrupting its neighboring slot. Cleanup
removes only containers created by that invocation, before unmounting their slots.
It never calls a global prune. This is an adversarial test fixture, not a sandbox
security certification or a performance benchmark.

Reproduce **only on a disposable Linux host** with Docker, runsc, sudo, e2fsprogs,
musl tools and the pinned Rust toolchain:

```sh
cargo build -p treeharbor-runtime --locked --target x86_64-unknown-linux-musl --bins --examples
python3 scripts/test-runtime.py
```

The workflow contains the exact runtime installation and checksum. Static binaries
must be rebuilt for the target architecture. Regular `cargo test --workspace`
checks parsing and rejection outside a namespace; it does not run these stress
cases on the developer's host.

## External contracts and remaining work

Linux documents [PID-namespace teardown and init signal rules](https://man7.org/linux/man-pages/man7/pid_namespaces.7.html)
and [non-dumpable process protection](https://man7.org/linux/man-pages/man2/PR_SET_DUMPABLE.2const.html).
gVisor's [resource model](https://gvisor.dev/docs/architecture_guide/resources/)
explains why guest processes and host cgroup processes are different. Its
[installation guide](https://gvisor.dev/docs/user_guide/install/) describes the
complete multi-file distribution. References checked 2026-10-21; behavior still
requires the actual supported runtime tests.

Remaining release gates include a production controller/adapter, immutable image
policy, durable node/attempt identity, job/epoch fencing, writer-stop observations,
safe cancellation/drain, disk-volume ownership and cleanup, restart reconciliation,
source materializer binding, and trusted validation collection. The supervisor
alone satisfies none of these end-to-end workspace lifecycle gates.
