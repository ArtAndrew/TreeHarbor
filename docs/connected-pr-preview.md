# Start the Connected PR Preview

Use this path for [the Connected PR Preview](https://github.com/ArtAndrew/TreeHarbor/releases):
**exact GitHub commit → two isolated tasks → immutable changes → draft PRs**.
GitHub handles checks, review and merge. TreeHarbor queue setup is not part of this path.
This is an operator-assisted Linux evaluation with manual infrastructure setup,
not a five-minute deployment or a ready-made LLM connector.

## 1. Check the task and host before setting up

The reference deployment uses PostgreSQL 18 over a trusted local Unix socket,
local durable object storage, Docker/cgroup v2 and the supported external gVisor
runtime. The operator also needs Git and Python 3.11+. Workspaces contain native
files **without `.git`**. Check whether your intended agent/tools can work that way
before provisioning the host. The included scripted fixture works with this format;
it does not prove compatibility with every coding agent.

Start with a disposable repository and dedicated test infrastructure. Keep source
backups. The external runtime supplies process isolation and resource limits;
workspace directories alone do not. Current network/proxy and runtime constraints
are in [GitHub connection](github-connection.md) and [managed runs](managed-runs.md).

## 2. Install the existing package

Follow the release's download, checksum and signature commands, then install into
a fresh prefix. No local Rust build is needed for the product binaries. Resolve
the executable for the prepared-agent helper. Set `TH_INSTALL_PREFIX` to the
absolute prefix you chose in the release installation commands:

```sh
: "${TH_INSTALL_PREFIX:?Set TH_INSTALL_PREFIX to the absolute prefix used by install.sh}"
TH_BIN=$(realpath "$TH_INSTALL_PREFIX/bin/treeharbor")
"$TH_BIN" --version
```

Commands shown as
`./target/debug/treeharbor` in development guides can use `"$TH_BIN"` instead.
The optional reviewed fixture-image build needs Rust; your own approved image
has its own build requirements. The binary installer does not provision a database,
Docker, gVisor or a GitHub App. See [package verification](release-packaging.md).

## 3. Prepare the service, storage and authority

Use an operator-owned private directory (0700) and configuration (0600). The
[service guide](local-service.md) explains its connection and file requirements.
Include local object storage and choose `connected` when creating the repository.
Its metadata-only example uses `standalone`; do not copy that authority for this
GitHub PR workflow. Repository authority is not switched implicitly by the helper.

If choosing a custom schema, **create it before `admin migrate`** using the same
PostgreSQL role/database as the service. For example, in your already authenticated
`psql` session on the dedicated test database:

```sql
CREATE SCHEMA treeharbor_preview AUTHORIZATION CURRENT_USER;
```

Set `"schema": "treeharbor_preview"` in the private service config and then run
`"$TH_BIN" admin --config /your/private/service.json migrate`. The schema name must
be new, and the role needs database CREATE permission. An existing schema requires
operator inspection of ownership and contents; do not silently adopt it with
`IF NOT EXISTS`. The default `public` schema does not need creation if it already
exists and is appropriate for that dedicated database. Serving never migrates
implicitly. [PostgreSQL CREATE SCHEMA](https://www.postgresql.org/docs/18/sql-createschema.html).

After explicit database preparation, inspect local service prerequisites:

```sh
"$TH_BIN" doctor --config /your/private/service.json
```

A failed prerequisite returns a nonzero status and a redacted repair hint.
Doctor never applies migrations or changes permissions. Passing covers only
its listed configuration/directory and database checks; continue with the
runtime, credential and host setup below. See [diagnostic scope](doctor.md).

## 4. Use PR permissions, not queue permissions

Register an operator-owned private GitHub App, install it only on the chosen
repository, and keep its RSA key/config outside agent files. Use the normal manual
registration flow in the [App guide](github-connection.md). For the complete
import/branch/PR path, the repository permissions are:

| GitHub App repository permission | Level | Purpose |
| --- | --- | --- |
| Contents | Write | Import source and publish the proposed branch |
| Pull requests | Write | Publish the draft PR |
| Metadata | Read | Verify the selected repository identity |

This preview does not need queue/managed-check templates, a webhook receiver,
a tunnel, or Actions/Checks/Administration/Merge queues grants.
A read-only probe needs only Contents-read/Metadata-read; that reduced App setup
cannot publish. The Rust bridge narrows each installation token to the specific
operation and numeric repository. These are instructions for your own App, not a
request to change any existing installation's permissions automatically.

GitHub App permissions and local TreeHarbor permissions are separate:

| Local identity | Repository permissions for this complete workflow |
| --- | --- |
| Human owner | `read`, `repository.write` |
| Assigned Bridge | `host.read`, `host.publish` |
| Each Agent | `read`, `workspace.write`, `change.write` |
| Each Node | `runtime.control` |

The Bridge needs `host.read` for connection verification/import and `host.publish`
for branch/PR delivery. Provision separate short-lived credentials explicitly
through the trusted admin CLI. Agents must never get the App key, owner or bridge
credentials. No queue/merge/check authority is required for the PR Preview.

## 5. Prepare and run the two tasks

Follow these guides in order, using the same repository/binding/authority epoch:

1. [Bind and verify the App](github-connection.md#retain-a-connection-in-a-running-treeharbor-server).
2. [Import an exact full commit SHA](github-import.md). Keep the returned native
   revision and snapshot; a branch name is not a pinned source.
3. [Prepare the two agent workspaces and task configuration](../examples/connected-pr/README.md#prepare-the-operator-environment),
   including two independently registered bounded placements and an approved image.
4. Run the example's `initialize`, `request`, `execute`, `capture`, `publish`, `status`
   phases with the original private configuration/journals. Save exact PR identities.
5. [Export retained proposals](github-projection.md), then [drain/clean up](workspace-cleanup.md)
   through the owning Agent and assigned Node. Revoke temporary credentials afterward.
   The installed trial also verified export after cleanup; retain the source before
   deleting private journals or backups. Never unmount active tasks or delete metadata
   manually to force cleanup.

An unknown publication response requires preserving the original IDs and journals,
not creating another PR intent. The Rust publisher performs bounded read-only
reconciliation. Full Connected publication disaster recovery remains unsupported.
The [installed ARM64 trial](evidence/connected-pr-preview-20261028.json) records what
actually passed; both architectures also passed installed package/runtime CI.

Report [where you stopped](../.github/ISSUE_TEMPLATE/pilot-feedback.md), time to the
first PR, help needed, and whether you would run another real task. Include the
release and host/task details, not private source or credentials. Track time spent
provisioning the environment separately from TreeHarbor setup and agent execution.
