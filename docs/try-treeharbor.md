# Try TreeHarbor

Start here if you build agent infrastructure or run repeated automated changes.
Choose a GitHub pull request workflow or a Standalone repository. The current
release is an **operator-assisted alpha** for disposable repositories on test
infrastructure.

[Watch the 36-second walkthrough](demo.md) · [Ask a setup question](https://github.com/ArtAndrew/TreeHarbor/discussions/categories/q-a)

## Choose your first experiment

| You want to… | Start with | What you need |
| --- | --- | --- |
| Understand the result before installing | [Recorded two-agent trial](demo.md) | A browser; no credentials or installation |
| Run two tasks and get real draft PRs | Steps below | Linux x86-64/ARM64, GitHub CLI, Git, Python 3.11+, PostgreSQL, GitHub App, Docker/cgroup v2 and gVisor |
| Keep your existing agent runtime | [Remote task assignment](remote-tasks-v1.md) | Prepared TreeHarbor service and pinned source; your controller supplies execution/isolation |
| Run two agents in Standalone | [Packaged overlap example](../examples/standalone/README.md) | Signed Linux package, Docker/gVisor, static BusyBox and a local PostgreSQL image |
| Connect an MCP host | [Experimental MCP stdio adapter](mcp.md) | Prepared local service, scoped agent credential and stdio-capable host; no managed launch or GitHub publishing tool |

This walk-through uses Git-compatible workspaces with private `.git` directories
and two separately registered volumes. Provision PostgreSQL, Docker, gVisor and
the GitHub App before running the tasks. The package installs the TreeHarbor
binaries and ships the example in the verified archive.

## 1. Install a signed Linux package

Run these commands in a fresh Linux shell with an authenticated `gh`. They
select the most recently published alpha, record its tag and commit, and verify
the archive against that exact source and the release workflow. To use a specific
published alpha, set `TH_TAG` first. Keep the generated directory and use the
same shell for the following steps. [Published releases](https://github.com/ArtAndrew/TreeHarbor/releases)

```sh
set -eu
TH_REPO=ArtAndrew/TreeHarbor
test "$(uname -s)" = Linux
TH_ARCH=$(uname -m)
case "$TH_ARCH" in
  x86_64|aarch64) ;;
  *) printf '%s\n' "Unsupported Linux architecture: $TH_ARCH" >&2; exit 1 ;;
esac
TH_RELEASES=$(gh api --paginate "repos/$TH_REPO/releases?per_page=100" --jq '
  .[] | select(.draft == false and .prerelease and .published_at != null)
  | select(.tag_name | test("^v[0-9]+\\.[0-9]+\\.[0-9]+-alpha\\.[0-9]+$"))
  | [.published_at, .tag_name] | @tsv')
if [ -z "${TH_TAG:-}" ]; then
  TH_TAG=$(printf '%s\n' "$TH_RELEASES" | LC_ALL=C sort -r | awk 'NR == 1 {print $2}')
fi
if ! printf '%s\n' "$TH_RELEASES" | awk -v tag="$TH_TAG" '$2 == tag && NF == 2 {found=1} END {exit !found}'; then
  printf '%s\n' 'Choose a published TreeHarbor alpha tag from the Releases page.' >&2
  exit 1
fi
TH_VERSION=${TH_TAG#v}
TH_SOURCE=$(gh api "repos/$TH_REPO/git/ref/tags/$TH_TAG" --jq '.object | select(.type == "commit") | .sha')
printf '%s\n' "$TH_SOURCE" | grep -Eq '^[a-f0-9]{40}$'
printf 'Selected %s at %s\n' "$TH_TAG" "$TH_SOURCE"
TH_ASSET="treeharbor-$TH_VERSION-$TH_ARCH-unknown-linux-musl"
TH_TRIAL=$(mktemp -d "${TMPDIR:-/tmp}/treeharbor.XXXXXXXX")
cd "$TH_TRIAL"

gh release download "$TH_TAG" --repo "$TH_REPO" \
  --pattern "$TH_ASSET.tar.gz" \
  --pattern "$TH_ASSET.tar.gz.sha256" \
  --pattern "$TH_ASSET.provenance.sigstore.json"
gh attestation verify "$TH_ASSET.tar.gz" \
  --bundle "$TH_ASSET.provenance.sigstore.json" --repo "$TH_REPO" \
  --signer-workflow "$TH_REPO/.github/workflows/release-provenance.yml" \
  --signer-digest "$TH_SOURCE" --source-digest "$TH_SOURCE" \
  --source-ref "refs/tags/$TH_TAG" --deny-self-hosted-runners
sha256sum --check --strict "$TH_ASSET.tar.gz.sha256"
tar -xzf "$TH_ASSET.tar.gz"

TH_INSTALL_PREFIX="$TH_TRIAL/install"
"$TH_TRIAL/$TH_ASSET/install.sh" --prefix "$TH_INSTALL_PREFIX"
TH_BIN=$(realpath "$TH_INSTALL_PREFIX/bin/treeharbor")
TH_EXAMPLE="$TH_TRIAL/$TH_ASSET/examples/connected-pr"
test -f "$TH_EXAMPLE/agent-pair.py"
test "$("$TH_BIN" --version)" = "treeharbor $TH_VERSION"
"$TH_BIN" --version
printf 'Trial directory: %s\n' "$TH_TRIAL"
```

For a Standalone trial, use the [two-agent example](../examples/standalone/README.md)
when the archive includes it, or [create a native repository](native-integration.md#start-a-new-project-with-empty-history).
For a Connected trial, continue below. Use the `gh` login on the operator host
for artifact verification. Set up the GitHub App and agent credentials separately.

For an existing deployment, follow the [upgrade steps for your installed version](release-packaging.md)
before restarting the service.

## 2. Prepare one disposable Connected repository

Follow the [service setup](connected-pr-preview.md#3-prepare-the-service-storage-and-authority)
and [App setup](connected-pr-preview.md#4-use-pr-permissions-not-queue-permissions)
with `TH_BIN` from step 1. The guides cover database migration, diagnostics, the
Connected repository and separate owner, agent, node and bridge credentials.
Scope the App to the chosen test repository and use Contents-write, Pull
requests-write and Metadata-read. Then [bind the App](github-connection.md#retain-a-connection-in-a-running-treeharbor-server)
and [import the exact commit](github-import.md).

**Checkpoint:** `doctor` passes its documented local checks; the App is bound to
the chosen repository and one exact full Git commit has been imported. Save the
returned native revision and snapshot. A successful doctor does not certify the
runtime or GitHub connection.

## 3. Run the supplied two-task example

Follow [the prepared-agent example](../examples/connected-pr/README.md). Register
the two bounded placements and the approved runtime image. For each workspace,
choose a full source profile, request `workspace-create --git-compatible`, and
use `prepare-once --workspace WORKSPACE_UUID --slot SLOT_UUID --git /path/to/git`
with its registered node. Set `"workspace_format": "git-compatible"` in the
private pair configuration. Fill `capture_worker.verified_binary_sha256` with
the same verified installed CLI digest used for `binary_sha256`. Keep the original
run, workspace and operation IDs. If the bridge and node share the same
private import directory, you can add
[`--git-import-pack`](local-node.md#reuse-an-imported-git-pack) to preparation.

The supplied fixture uses two scripted edits to the same file. Once the nodes,
runtime and private configuration are ready, run the helper from the verified
archive:

```sh
cd "$TH_EXAMPLE"
python3 agent-pair.py --config /your/private/pair.json initialize
python3 agent-pair.py --config /your/private/pair.json request
python3 agent-pair.py --config /your/private/pair.json execute
python3 agent-pair.py --config /your/private/pair.json capture
python3 agent-pair.py --config /your/private/pair.json publish
python3 agent-pair.py --config /your/private/pair.json status
```

Replace `/your/private/pair.json` with the configuration you prepared. `publish`
writes branches and draft PRs through the trusted bridge. Keep its access on the
operator host.

**Checkpoint:** two immutable accepted ChangeSets and two distinct GitHub draft
PR identities. Inspect the diffs and let GitHub handle CI, conflicts, review and
merge. If a response is uncertain, retain the original config/IDs/journals and
follow the retry guidance; do not create a second publication intent.

## 4. Verify the exit path and clean up

[Export both retained proposals](github-projection.md#request-prepare-and-export)
before following [workspace drain and cleanup](workspace-cleanup.md). The helper
has already prepared their projections. Use `agents.A.github.projection` and
`agents.B.github.projection` from `pair.json` inside the private journal directory
for `github export`, then run `git-export` to obtain each `history.bundle`. For each
workspace, request drain as its owning agent with `--authority connected`, then
use its assigned node credential, original node root, registered storage UUID,
and Engine socket for cleanup. Keep the registered volumes mounted until both
workspace statuses show `deleted`.

Export both retained proposals again into new destinations after deletion.
Clone each bundle with Git and run `git fsck --strict` on each clone. Compare
the exported files with the matching PR diff. Retain the exports and private
journals until you have checked both results, then revoke trial credentials.

**Checkpoint:** workspaces are deleted, exported source is intact, temporary
access is revoked. Full publication/queue disaster recovery remains unsupported.

## Tell us where you stopped

A failed attempt is useful feedback. Use the
[pilot report](https://github.com/ArtAndrew/TreeHarbor/issues/new?template=pilot-feedback.md)
with the version, host, intended task, last completed checkpoint and what blocked
you. Separate infrastructure provisioning time from TreeHarbor setup and task time.
Would you run a second task? What do you use instead today?

Never include credentials, private source or private repository URLs. For security
reports, use [SECURITY](../SECURITY.md). No external code contribution is required
to participate. TreeHarbor is source-available under [ELv2](../LICENSE), with no
workspace-count license limit. [Preparation measurements](benchmark-preparation.md)
describe where reuse saves time and where Git worktree is faster.
