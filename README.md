# TreeHarbor

**Self-hosted workspaces for parallel coding agents.**

Run agents from the same commit, keep their changes after workspace cleanup,
and send each change to GitHub as a draft PR. Built for platform teams running
code migrations and maintenance jobs with their own agents.

[**Install v0.1.17-alpha.1**](docs/try-treeharbor.md) ·
[Watch the 36-second demo](docs/demo.md) ·
[Signed Linux packages](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.17-alpha.1)

**Alpha for test repositories.** Setup is manual on Linux with PostgreSQL.
The two-agent walkthrough uses gVisor for execution isolation.

[![Two agents start from one pinned commit. Each saved change becomes a separate GitHub draft PR.](docs/assets/connected-workflow.svg)](docs/demo.md)

## What works today

- **Connected.** Use your existing GitHub CI, review and merge process.
  Each saved change records its starting revision and agent run, and you can
  export it to Git after cleanup. Agents that need Git commands can use a
  [workspace with private Git metadata](docs/local-node.md#prepare-an-existing-native-workspace).
- **Standalone (experimental).** Start an [empty native repository](docs/native-integration.md#start-a-new-project-with-empty-history),
  publish changes under TreeHarbor review and check policies, and export the
  history to Git when needed.

If you already run agents on your own workers, your controller can read pinned
source and submit changes over the [HTTPS API](docs/agent-https.md). There is also
a [Rust CLI](docs/agent-commands.md) and an [experimental MCP adapter](docs/mcp.md).

## Reuse the source between tasks

[Copy-on-write workspaces](docs/experimental-cow.md) share one prepared source
image and give each agent a writable layer. In an 8,195-file prototype test,
the median time until all four tasks read a file was **161 ms vs. 335 ms**
with Git worktree on repeat batches. The first batch took **2.07 s vs. 0.57 s**.
[Method and raw data](docs/benchmark-preparation.md#shared-source-images--9-november-2026)

## What inspired it

Anthropic’s Scion inspired the separation of workspaces from source history.
OpenAI’s Pando reinforced the value of keeping existing Git workflows.
TreeHarbor starts with the handoff from agent tasks to your existing review process.
[Scion](https://www.anthropic.com/research/redesigning-source-control) ·
[Pando](https://openai.com/index/scaling-git-at-openai/)

[Deployment layout](docs/connected-pr-preview.md#system-layout) ·
[Status and release gates](docs/implementation-status.md) ·
[Roadmap](ROADMAP.md) ·
[Contributing](CONTRIBUTING.md)

## License

Source-available under [Elastic License 2.0](LICENSE).
