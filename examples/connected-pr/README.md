# Two prepared agents to GitHub PRs

Experimental operator helper for the Connected PR Preview. It runs two approved
commands concurrently in separate prepared workspaces, captures their stopped
files through the Rust node, accepts immutable changes, and optionally publishes
two proposed branches and PRs. GitHub handles CI, review, conflicts and merging.

**The complete installed live path of this helper is not yet verified.** Controlled
reply tests cover its orchestration and failure boundaries; they do not establish
GitHub or runtime acceptance. This is not the installable alpha or an automatic
host installer. Use disposable pilot repositories and retain independent backups.
The example uses scripted agents; it is not evidence of an LLM integration.

## Prepare the operator environment

Use one trusted Linux operator/bridge host with Python 3.11+, PostgreSQL, Git,
Docker/cgroup v2 and the supported external gVisor runtime. The installed Rust CLI
owns repository operations, permissions, capture, branch transport and PR recovery;
this helper does not implement another source engine.

1. [Install and verify the package](../../docs/release-packaging.md), then configure
   the [local service](../../docs/local-service.md) and its private durable storage.
2. Create a Connected repository, configure its selected
   [GitHub App binding](../../docs/github-connection.md), and
   [import an exact commit](../../docs/github-import.md). Save the returned native
   revision and snapshot, not an approximate branch name.
3. Register the approved [runtime profile](../../docs/managed-runs.md) and two
   independent [nodes/bounded placements](../../docs/local-node.md). Use matching
   installed runtime init/image versions. Prepare two native workspaces at the
   imported baseline with `prepare-once --workspace EXACT_UUID`; save each actual
   `node prepared` receipt. Workspaces provide files; gVisor provides execution
   isolation. The helper cannot provision or certify those boundaries.
4. Supply separate current service credentials: each Agent needs `read`,
   `workspace.write`, `change.write`; each Node needs `runtime.control`; the Human
   owner needs `read`, `repository.write`; the binding's assigned Bridge needs
   `host.publish`. The App needs Contents-write, Pull requests-write and implicit
   Metadata-read on the selected repository for branch/PR delivery. Queue, Checks,
   Actions and Administration grants are not required for this helper.

Keep the App key/configuration, publisher credentials, database configuration,
binary and helper journal outside agent files. No credentials are created or
granted automatically. Never place a publisher or App key in an agent's argv,
environment or image. Follow the [security boundaries](../../SECURITY.md).

## Configure one immutable task pair

Copy [config.example.json](config.example.json) into an operator-owned directory
with mode 0700; make the configuration mode 0600. Its placeholders intentionally
fail validation. Fill the actual scope, baseline, prepared paths, node identities,
runtime profile, credential paths and approved argv. Use new nonnil UUIDs for
each run/seal/ChangeSet and distinct operation keys; preserve them on retry.

`binary` must name the resolved **regular executable**, not the installer's
`bin/treeharbor` symlink. Verify its hash against the independently verified
archive. For example, inspect `realpath /your/prefix/bin/treeharbor` and
`sha256sum` on that resolved path. A configured hash pins bytes; it is not proof
that those bytes passed runtime tests or signature verification.

The `capture_worker` block opts into the exact `seal-once --seal` worker. Enable it
only for an installed binary with verified selector acceptance. Omitting it
allows capture acceptance only for an already-ready seal; it never falls back
to an unqualified worker. Prepared/run/capture selection always uses exact IDs.

Select explicit changed paths under `selection`. Sparse absence does not mean
delete: deletions must be listed explicitly. For an overlap demo, both agents can
edit `src/lib.rs` from the same baseline. This preserves independent proposals;
it does not promise a conflict-free or semantically valid combined result.

The optional `github` block specifies the owner/bridge paths, existing binding,
base and explicit title/body/draft status of each PR. `publish` is an explicit
operator action. Initializing the journal saves distinct projection/publication/
pull identities before any external write. It creates no remote resources.
Omit this block to evaluate run/capture only.

## Build the reviewed scripted fixture image

This optional example needs the pinned Rust compiler with the native Linux musl
static target/linker. The product CLI is installed from its package; compiling a
fixture is not a requirement to use your own approved agent image.

For a disposable repository, use the existing
[small Cargo fixture](../github-queue/seed/) with `src/lib.rs` replaced by
[fixture/baseline.rs](fixture/baseline.rs), then import its exact commit. The
scripted program refuses any other file bytes before writing. The project trial
uses that same source at acceptance-repository commit
`a91d290d47abeaec32867dd21bb7107bec87ef05`.

On the trusted Linux host, set `TH_BIN` to the verified resolved package executable
and `TH_EXAMPLE` to this example directory. Build only this reviewed fixture:

```sh
TH_TARGET="$(uname -m)-unknown-linux-musl"
TH_IMAGE_DIR=$(mktemp -d)
rustc --edition 2024 --target "$TH_TARGET" -O \
  "$TH_EXAMPLE/fixture/agent.rs" -o "$TH_IMAGE_DIR/agent"
cp "$(dirname "$TH_BIN")/treeharbor-runtime-init" "$TH_IMAGE_DIR/init"
docker build --network none --pull=false \
  --iidfile "$TH_IMAGE_DIR/image-id" \
  -f "$TH_EXAMPLE/fixture/Dockerfile" "$TH_IMAGE_DIR"
```

Pin the resulting `sha256:...` image ID in the registered runtime profile; do not
use a mutable tag. Register the supported resource/user limits from the runtime
guide. Each run's argv is `/agent A` or `/agent B`. No network or credentials are
needed inside these tasks. Keep untrusted repository build scripts inside their
external runtime; this host compilation example is for the reviewed fixture only.

Both agents edit `src/lib.rs`. A doubles per-worker memory and the budget; B
doubles workers and the budget. Each proposal fits the budget by itself, while
the combination exceeds it. This is an intended scenario, not a claim of an
executed combined-candidate check. The helper publishes proposals and leaves
validation/review/merge to the ordinary GitHub process.

## Execute and publish

Run from this directory, replacing the config path with your private file:

```sh
python3 agent-pair.py --config /private/operator/pair.json initialize
python3 agent-pair.py --config /private/operator/pair.json request
python3 agent-pair.py --config /private/operator/pair.json execute
python3 agent-pair.py --config /private/operator/pair.json capture
python3 agent-pair.py --config /private/operator/pair.json publish
python3 agent-pair.py --config /private/operator/pair.json status
```

The two execution workers run concurrently. Capture requires succeeded runs,
zero exit codes, no OOM and confirmed runtime removal. Rust computes the captured
objects and ChangeSets; the helper does not read mutable task files to invent a
proposal. Output retains run/ChangeSet identities and observed PR numbers/node
IDs. Those are historical delivery observations, not a fresh claim that a PR is
still open, current, checked or merged. No queue or merge command is invoked.

For another task, create a new config and journal with new task identities.
Do not change an existing journal's configuration to reuse it for different work.

## Failure, cleanup and leaving TreeHarbor

Preserve the helper journal, request files and Rust node/publisher journals after
an interruption. Lost run/seal/accept replies use the exact operation key; issued
run/capture workers resume by status only. Unknown outcomes stop rather than
starting another worker. A timeout does not prove the runtime stopped.

Re-entering `publish` retains the same IDs, payloads and Rust state directories.
The Rust publisher owns the sole dispatch grant and read-only recovery of an
existing attempt. Missing/ambiguous remote effects do not permit another POST,
push or replacement ID. A changed identity or snapshot stops the workflow.
This does not promise exactly-once remote creation or full disaster recovery.

After retaining/exporting accepted source, explicitly drain each workspace and
use the [node cleanup procedure](../../docs/workspace-cleanup.md). The helper
does not delete workspaces or journals automatically. Do not unmount/delete a
placement while a task, capture or cleanup is active. Retain independent backups;
full Connected publication disaster recovery remains outside the first preview.

Published branches/PRs remain ordinary GitHub resources. Use normal GitHub tools
for CI/review/merge and Git import/export to retain source when disconnecting.
See [interop](../../docs/git-export.md) and
[pilot feedback](../../.github/ISSUE_TEMPLATE/pilot-feedback.md).
