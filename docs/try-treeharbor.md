# Try TreeHarbor

Start here if you build agent infrastructure or run repeated automated changes
and want to keep GitHub as your review and merge workflow. The current release is
an **operator-assisted alpha**, for disposable repositories on dedicated test
infrastructure. A local Git worktree may be sufficient for a few personal tasks.

[Watch the 36-second walkthrough](demo.md) · [Ask a setup question](https://github.com/ArtAndrew/TreeHarbor/discussions/categories/q-a)

## Choose your first experiment

| You want to… | Start with | What you need |
| --- | --- | --- |
| Understand the result before installing | [Recorded two-agent trial](demo.md) | A browser; no credentials or installation |
| Run two tasks and get real draft PRs | Steps below | Linux x86-64/ARM64, PostgreSQL, GitHub App, Docker/cgroup v2 and external gVisor |
| Keep your existing agent runtime | [External API tasks](external-api-runs.md) | Prepared TreeHarbor service and pinned source; your controller supplies execution/isolation |
| Connect an MCP host | [Experimental MCP stdio adapter](mcp.md) | Prepared local service, scoped agent credential and stdio-capable host; no managed launch or GitHub publishing tool |

Native workspaces have **no `.git` directory**. Check your agent's requirements
before provisioning a host. There is no automatic installer for PostgreSQL,
gVisor or the GitHub App, and no verified five-minute end-to-end setup yet.

## 1. Install a signed Linux package

Use a new directory and the version-pinned instructions in
[0.1.12-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.12-alpha.1).
They verify the archive against the signed workflow/source, check the checksum
and install into a new prefix. Pick `aarch64-unknown-linux-musl` on ARM64 or
`x86_64-unknown-linux-musl` on x86-64. No local Rust build is required for the
product binaries. Follow all verification steps before extraction.

**Checkpoint:** the installed `treeharbor --version` prints `0.1.12-alpha.1`.
This confirms the CLI installation, not a running service.

## 2. Prepare one disposable Connected repository

Follow [Connected setup, steps 1–4](connected-pr-preview.md). That guide covers
service/storage initialization, database migration, diagnostics, the Connected
authority and separate owner/agent/node/bridge credentials. Use its minimal App
permissions: Contents-write, Pull requests-write and Metadata-read, scoped to the
chosen test repository. The PR Preview needs no merge queue or webhook tunnel.

**Checkpoint:** `doctor` passes its documented local checks; the App is bound to
the chosen repository and one exact full Git commit has been imported. Save the
returned native revision and snapshot. A successful doctor does not certify the
runtime or GitHub connection.

## 3. Run the supplied two-task example

Follow [the prepared-agent example](../examples/connected-pr/README.md), including
its two separate bounded placements, reviewed fixture image and private task
configuration. Its Rust scripts edit the same file from one base; they are not LLMs.
Once that setup is complete, run the documented phases with your own config:

```sh
python3 agent-pair.py --config /your/private/pair.json initialize
python3 agent-pair.py --config /your/private/pair.json request
python3 agent-pair.py --config /your/private/pair.json execute
python3 agent-pair.py --config /your/private/pair.json capture
python3 agent-pair.py --config /your/private/pair.json publish
python3 agent-pair.py --config /your/private/pair.json status
```

Run these from `examples/connected-pr/` at the matching release tag. The path is
a placeholder, not a supplied credential. `publish` writes branches and draft
PRs through the configured trusted bridge; the agents must not hold its access.

**Checkpoint:** two immutable accepted ChangeSets and two distinct GitHub draft
PR identities. Inspect the diffs and let GitHub handle CI, conflicts, review and
merge. If a response is uncertain, retain the original config/IDs/journals and
follow the retry guidance; do not create a second publication intent.

## 4. Verify the exit path and clean up

[Export retained proposals](github-projection.md), then follow
[workspace drain/cleanup](workspace-cleanup.md). Confirm tasks have stopped and
the runtime has been removed before cleanup. Verify export again after cleanup;
the recorded trial did this for both proposals. Retain the export and private
journals until you have checked the result. Revoke trial credentials explicitly.
Deleting a workspace does not delete its accepted proposals or GitHub PRs.

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
workspace-count license limit. No production readiness or measured speed advantage
is claimed.
