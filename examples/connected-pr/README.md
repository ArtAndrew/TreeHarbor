# Two prepared agents to GitHub PRs

Experimental operator helper for the Connected PR Preview. It runs two approved
commands concurrently in separate prepared workspaces, captures their stopped
files through the Rust node, accepts immutable changes, and optionally publishes
two proposed branches and PRs. GitHub handles CI, review, conflicts and merging.

The installed helper passed the operator-assisted Linux ARM64 trial in
[alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.0-alpha.1): two concurrent scripted agents, two real draft PRs,
safe cleanup and verified Git export after cleanup. See [the scoped report](../../docs/evidence/connected-pr-preview-20261028.json).
Both architectures passed package/runtime CI; the live GitHub trial ran on ARM64.
This helper requires prepared infrastructure and credentials; it is not an automatic
host installer or evidence of an LLM integration. Use disposable pilot repositories
and retain independent backups.

Start with the [PR Preview setup path](../../docs/connected-pr-preview.md) for the
minimal App permissions, separate local roles and database preparation.

## Prepare the operator environment

Use one trusted Linux operator/bridge host with Python 3.11+, PostgreSQL, Git,
Docker/cgroup v2 and the supported external gVisor runtime. The installed Rust CLI
owns repository operations, permissions, capture, branch transport and PR recovery;
this helper does not implement another source engine.

1. [Install and verify the package](../../docs/release-packaging.md), then configure
   the [local service](../../docs/local-service.md) and its private durable storage.
2. Create a Connected repository, configure its selected
   [GitHub App binding](../../docs/github-connection.md), and
   [import an exact commit](../../docs/github-import.md). Keep the final JSON
   response as `import.json` in the private receipts directory described below.
3. Register the approved [runtime profile](../../docs/managed-runs.md) and two
   independent [nodes/bounded placements](../../docs/local-node.md). Use matching
   installed runtime init/image versions. Prepare two workspaces at the imported
   baseline with `prepare-once --workspace EXACT_UUID` and save each successful
   reply. You can also read it again with `node prepared`. Workspaces provide files; gVisor provides execution
   isolation. The helper cannot provision or certify those boundaries.
4. Supply separate current service credentials: each Agent needs `read`,
   `workspace.write`, `change.write`; each Node needs `runtime.control`; the Human
   owner needs `read`, `repository.write`; the binding's assigned Bridge needs
   `host.read` for verification/import and `host.publish` for PR delivery. The App needs Contents-write, Pull requests-write and implicit
   Metadata-read on the selected repository for branch/PR delivery. Queue, Checks,
   Actions and Administration grants are not required for this helper.

For a task that needs Git, request a full source profile and private Git
metadata. With CLI 0.1.34 or later, save a separate
[`workspace-new --git-compatible` request](../../docs/workspace-new.md#keep-private-git-history-for-a-task)
for each task. Clients from 0.1.13 through 0.1.33 use
`workspace-create --git-compatible` with a saved UUID and key.
Pass the returned workspace ID, its registered volume slot and
`--git /path/to/git` to `node prepare-once`. Install Git in the approved
agent image if the task needs it. Capture still selects working files against
the imported base; a local Git commit does not publish a GitHub branch.

Keep the App key/configuration, publisher credentials, database configuration,
binary and helper journal outside agent files. No credentials are created or
granted automatically. Never place a publisher or App key in an agent's argv,
environment or image. Follow the [security boundaries](../../SECURITY.md).

## Configure one immutable task pair

From 0.1.33, `configure` fills in the source revision, snapshot, prepared paths
and generation from three saved product replies. It generates and saves the six
run/seal/ChangeSet IDs and their retry keys in a new private config.

Before the import, choose a persistent operator directory outside the agent
workspaces. Set `TH_OPERATOR_DIR` to its absolute path and `TH_EXAMPLE` to this
verified example directory. Run this once for a new pair:

```sh
umask 077
TH_PAIR_PRIVATE=$(mktemp -d "$TH_OPERATOR_DIR/pair.XXXXXXXX")
mkdir -m 700 "$TH_PAIR_PRIVATE/receipts"
install -m 600 "$TH_EXAMPLE/settings.example.json" "$TH_PAIR_PRIVATE/settings.json"
```

Save stdout from the successful [`github import` command](../../docs/github-import.md)
to `$TH_PAIR_PRIVATE/receipts/import.json`. For a completed import, running it
again with the original import ID, epoch and state directory returns the same
source identity. Save the node replies as `A-prepared.json` and `B-prepared.json`
in that directory. If the workspaces are already prepared, use the registered
node and workspace values from setup. For example, for agent A:

```sh
"$TH_BIN" node prepared --config "$TH_SERVICE_CONFIG" \
  --credential-file "$TH_NODE_CREDENTIAL_A" --root "$TH_NODE_ROOT_A" \
  --storage "$TH_STORAGE_A" --workspace "$TH_WORKSPACE_A" \
  > "$TH_PAIR_PRIVATE/receipts/A-prepared.json"
```

Repeat for B with its own credential, root, storage ID and workspace ID.
In [settings.json](settings.example.json), enter the approved Git commit, node
and workspace IDs, exact executable hash, credential paths, task commands,
output paths and draft PR text. Choose a new journal path under
`TH_PAIR_PRIVATE`. Choose `git-compatible` for workspaces with private Git
metadata or `native` for file-only workspaces. Then run:

```sh
python3 "$TH_EXAMPLE/agent-pair.py" --config "$TH_PAIR_PRIVATE/pair.json" configure \
  --settings "$TH_PAIR_PRIVATE/settings.json" --receipts "$TH_PAIR_PRIVATE/receipts"
```

The command reads local files and creates the config with mode 0600. It reports
`created` on success and refuses to replace an existing config. Keep this file
and the journal together for retries. For packages through 0.1.32 or an existing
explicit setup, the [full config format](config.example.json) is still supported.

`binary` must name the resolved **regular executable**, not the installer's
`bin/treeharbor` symlink. Verify its hash against the independently verified
archive. For example, inspect `realpath /your/prefix/bin/treeharbor` and
`sha256sum` on that resolved path. A configured hash pins bytes; it is not proof
that those bytes passed runtime tests or signature verification.

In settings, `selected_capture: true` creates the `capture_worker` block in the
generated config, pinned to the same verified binary. This opts into the exact `seal-once --seal` worker. Enable it
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
chmod 0555 "$TH_IMAGE_DIR/init" "$TH_IMAGE_DIR/agent"
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

From this directory, check both prepared workspaces before issuing work. Then
start both tasks and publish their results as two GitHub draft PRs.

```sh
python3 agent-pair.py --config "$TH_PAIR_PRIVATE/pair.json" check
python3 agent-pair.py --config "$TH_PAIR_PRIVATE/pair.json" run-to-pr
```

`check` reads the two workspace and node receipts and leaves the journal untouched.
`run-to-pr` requires `draft: true` for both PRs. A retry with the same config
and journal resumes from the recorded operations. Once both PRs have been
observed, a retry reads their saved identities locally. If the journal is
damaged, restore it from backup before continuing.

For individual steps use `initialize`, `request`, `execute`, `capture`, then
`publish`. Use `status` to query the service while the runs are accessible.
For capture only, stop after `capture`.

The two execution workers run concurrently. Capture requires succeeded runs,
zero exit codes, no OOM and confirmed runtime removal. Rust computes the captured
objects and ChangeSets; the helper does not read mutable task files to invent a
proposal. Output retains run/ChangeSet identities and observed PR numbers/node
IDs. Those are historical delivery observations, not a fresh claim that a PR is
still open, current, checked or merged. No queue or merge command is invoked.

## Run another pair after the base branch advances

Keep the first pair's config and journal for retries and export. Drain both
old workspaces and wait for node cleanup to report `deleted` before reusing
their registered slots. The service, nodes, runtime profile and GitHub App
binding can serve the next pair.

Create a fresh private pair directory as above. Import the new Git commit and
save its completion as `receipts/import.json`. Set `TH_EPOCH` to the
repository's current authority epoch and `TH_FULL_PROFILE` to its registered
full source profile digest. With CLI 0.1.34 or later, use a new private
state file to request a Git-compatible workspace for agent A.

```sh
TH_BASE=$(jq -er '.revision' "$TH_PAIR_PRIVATE/receipts/import.json")
"$TH_BIN" client --socket "$TH_SOCKET" --credential-file "$TH_AGENT_CREDENTIAL_A" \
  workspace-new --state "$TH_PAIR_PRIVATE/A.workspace.json" --git-compatible \
  --authority connected --epoch "$TH_EPOCH" --base "$TH_BASE" \
  --profile-digest "$TH_FULL_PROFILE" > "$TH_PAIR_PRIVATE/A.requested.json"
TH_WORKSPACE_A=$(jq -er '.result.workspace' "$TH_PAIR_PRIVATE/A.requested.json")
"$TH_BIN" node prepare-once --config "$TH_SERVICE_CONFIG" \
  --credential-file "$TH_NODE_CREDENTIAL_A" --root "$TH_NODE_ROOT_A" \
  --storage "$TH_STORAGE_A" --slot "$TH_SLOT_A" --workspace "$TH_WORKSPACE_A" \
  --git /usr/bin/git > "$TH_PAIR_PRIVATE/receipts/A-prepared.json"
```

Run the same steps for B with its own state file, credentials and slot. In the new
settings, enter the new Git commit, both returned workspace IDs, task commands,
output paths, PR text and a fresh journal path. Then use `configure`, `check`
and `run-to-pr` as above. If a creation reply is lost, repeat
`workspace-new` with its original state file. For a workspace already
prepared, read the reply again with `node prepared`. On clients through
0.1.33 use `workspace-create --git-compatible` with a saved UUID and key.

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
[pilot feedback](https://github.com/ArtAndrew/TreeHarbor/issues/new?template=pilot-feedback.md).
