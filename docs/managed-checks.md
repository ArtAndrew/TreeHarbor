# Managed native checks (experimental)

An assigned validator can now request execution of an approved offline check on
an exact native integration candidate. The API does not accept a replacement
command, environment, exit code or evidence digest on this path. A trusted node
uses the existing [bounded gVisor runtime](managed-runs.md); a protected collector
records its terminal observation after confirmed runtime removal.

This is a development interface, not automatic CI scheduling or a complete
Standalone release. Native Linux x86-64 or ARM64, PostgreSQL schema 25, an operator-approved
immutable runtime image and a separately provisioned bounded volume are required.
There is no network installation of dependencies, host environment inheritance,
CI secrets injection, shared build cache, artifact/log service or packaged install.

## Roles and workflow

- An operator registers a runtime profile containing the immutable image ID,
  uid/gid and resource limits, as in the managed-run guide.
- A human with `policy.write` registers a `CheckProgram` and includes its returned
  four identities in a native policy check. In v1, the immutable image is both the
  toolchain and external dependency bundle. Repository dependencies and manifests
  are part of the exact candidate snapshot. The policy names trusted validators.
- An integrator assigns the current check run using `check_assign`.
- That assigned validator (`validation.report`) prepares and starts a dedicated
  full native workspace. It receives no generic `workspace.write` permission.
- The trusted node prepares files and executes the stored command using the
  existing `prepare-once` and `run-once` commands.
- The validator calls `check_collect` after runtime removal. The service computes
  evidence from the durable runtime observation. `check_status` exposes the receipt,
  outcome and evidence digest to the assigned validator with `read` permission.
- Publication separately requires current checks, independent human review and
  unchanged target/epoch/policy. A check passing does not itself publish code.

Use private JSON files with `treeharbor client --socket SERVICE --credential-file
CREDENTIAL native --request REQUEST`. Example requests below use placeholders;
replace every digest and UUID with a returned value. Mutations include a unique
idempotency key and the repository's current epoch.

```json
{"action":"program_put","key":"program-1","epoch":1,"program":{"version":1,"runtime_profile":"sha256:<runtime-profile-digest>","argv":["/tools/check","test"]}}
```

The response supplies `definition`, `environment`, `toolchain` and `dependencies`.
Add `name` and `validators` to these fields in the policy's `checks` array. See
[native integration](native-integration.md) for policy/ref/queue setup and review.
Once the integrator has assigned `CHECK_RUN_UUID`:

```json
{"action":"check_prepare","key":"check-files-1","epoch":1,"run":"CHECK_RUN_UUID"}
```

This returns a server-generated workspace and execution-run UUID. Run node
`prepare-once` on an unused registered volume; then:

```json
{"action":"check_start","key":"check-start-1","epoch":1,"run":"CHECK_RUN_UUID"}
```

After node `run-once` confirms removal:

```json
{"action":"check_collect","key":"check-collect-1","epoch":1,"run":"CHECK_RUN_UUID"}
{"action":"check_status","run":"CHECK_RUN_UUID"}
```

`preparing → requested → collected` tracks orchestration. The separate managed
run exposes requested/launching/running/stopping and terminal state. A collected
receipt can be `passed`, `failed`, `error` or `cancelled`; collection is not success.
A missing exit observation produces `error`, never an inferred zero exit.

## Correctness and recovery

A check workspace starts from the candidate's native commit, with a full profile,
initial generation and no prior execution. Before first start, the trusted node
verifies the exact entry set, file bytes, executable flags and symlink payloads
against its protected baseline. It uses bounded descriptor-relative reads without
following symlink ancestors. Extra/deleted/modified files and hard-linked regular
files are rejected. This adds a full input read; no performance advantage is claimed.
Recovery of an already running or completed runtime does not repeat this check or
reject legitimate build outputs. It reconciles the existing execution instead.

The original validator credential, current check assignment, candidate, target,
epoch, policy and runtime-profile approval fence future starts and renewals. A new
assignment, cancellation or policy change requests removal of an active check.
There can be an already admitted external start request in flight; exact runtime
cleanup handles it. No instantaneous revocation of physical execution is claimed.

The collector accepts no user-supplied outcome. Evidence binds candidate/context,
program, validator credential, workspace, execution, preparation manifest, node,
volume, runtime binding, container identity, exit/OOM/timestamps and completion
ownership. The node is trusted infrastructure; this is not hardware attestation.
Node principal/access and runtime-profile approval are checked again at publication.
A disabled evidence principal is locked through publication so a completed disable
cannot race a later admission based on an earlier MVCC read. Expiry limits future
credential use; explicit validator-token revocation also fences pending evidence.
Revoking an old node token alone is not retroactive revocation of every observation
by that node; revoke its repository grant/principal or the runtime profile instead.

Registered programs cannot use the external `validation_report` route, including
before execution has been prepared. Legacy external collectors remain supported
for policy definitions without a registered program; their asserted evidence is a
separate trust model. Merely knowing an ordinary AgentRun exited zero is insufficient.

Retries reuse one execution binding, one runtime attempt and one immutable receipt.
A new request key for an already requested/collected check does not rerun it. Use a
new integrator-assigned check run to request another execution. Old receipts cannot
satisfy that new assignment. Unknown or incomplete cleanup retains the placement.
After collection the assigned validator can request `check_release` through the
native API. The [workspace cleanup protocol](workspace-cleanup.md) then allows the
assigned node to remove files and release the slot/pin while retaining check evidence.
Automatic CI retention and cleanup scheduling remain unfinished.

## Version 1 encoding

`CheckProgram` is canonical UTF-8 JSON in the field order `version`,
`runtime_profile`, `argv`, at most 16 KiB. The profile is a canonical SHA-256 ID;
argv follows runtime Task validation. Unknown/duplicate fields are rejected.
Its ID is SHA-256 of `treeharbor-check-program\0v1\0` followed by canonical bytes.
Arguments are literal argv, not an implicit shell command.

Execution evidence is canonical compact JSON of this ordered array:

```text
[1, tenant, repository, check_run, candidate, context_digest, program_digest,
 validator, original_validator_token, workspace, execution_run, revision,
 snapshot, runtime_profile, argv, runtime_attempt, preparation_attempt,
 runtime_binding, manifest_digest, node, storage_instance, slot, volume_digest,
 engine, container, run_state, exit_code, oom_killed, started_at, finished_at,
 stop_requested, finished_generation, finished_worker, outcome]
```

IDs are canonical strings; absent runtime observations are null. Its ID is SHA-256
of `treeharbor-check-execution-evidence\0v1\0` followed by those bytes, at most
64 KiB. Tenant/repository-scoped immutable rows retain the canonical bytes.
The zero bytes shown as `\0` above are actual NUL domain separators, not backslashes.
Golden vectors are in `crates/metadata/tests/fixtures/check-program-v1.json`.
