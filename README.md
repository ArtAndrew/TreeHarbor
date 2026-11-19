# TreeHarbor

**Self-hosted, Git-compatible workspaces for parallel coding agents.** Run several tasks from the same commit, keep each submitted change after its workspace is gone, and open a GitHub draft pull request for each one. TreeHarbor records the base revision and agent run behind every change.

Built for platform teams running code migrations and maintenance jobs with their own agents. Signed Linux alpha for test repositories.

[Try the two-agent workflow on Linux](docs/try-treeharbor.md) · [Watch the 36-second demo](docs/demo.md) · [Download the signed Linux alpha](https://github.com/ArtAndrew/TreeHarbor/releases)

[![One pinned commit, two agent workspaces, two saved changes and two GitHub draft pull requests.](docs/assets/connected-workflow.svg)](docs/demo.md)

## Two agents edit the same file

In a recorded trial, one task changed memory per worker and the other changed the worker count. Both edited `src/lib.rs` from the same commit. Each change produced a draft PR and could still be exported to Git after cleanup. [Memory change, PR #4](https://github.com/TreeHarbor/TreeHarbor-acceptance/pull/4) · [Worker-count change, PR #5](https://github.com/TreeHarbor/TreeHarbor-acceptance/pull/5) · [Resuming after GitHub accepted the first PR in 0.1.34](docs/demo.md#retry-after-github-accepts-a-pr)

## What works today

- **Connected.** Open a draft PR for each change and use your existing CI and review process. With a GitHub merge queue, an operator can also verify which checked commit landed after restarting TreeHarbor. [Set up two agents](examples/connected-pr/README.md#configure-one-immutable-task-pair) · [Recover a workspace](docs/workspace-new.md#retry-and-recovery) · [Single-PR queue trial](docs/demo.md#verify-a-merge-after-restarting-treeharbor)
- **Standalone, experimental.** TreeHarbor owns the branch history and merge queue. Run two agents without a GitHub account, resolve their conflict and inspect the result in ordinary Git. A cold backup also preserves the native changes and checks. [Standalone example](examples/standalone/README.md) · [Recover on another host](docs/standalone-host-recovery.md)

In the reference deployment, a node prepares the files and Docker with gVisor runs each task. The Rust service keeps submitted source in the object store and tracks runs in PostgreSQL. A separate publisher holds the GitHub App credentials. [Deployment diagram](docs/connected-pr-preview.md#system-layout) · [Agent controller API](docs/remote-tasks-v1.md) · [Experimental MCP adapter](docs/mcp.md)

## First run and repeat runs

Four tasks at a time on an 8,195-file repository, using a two-CPU Linux ARM64 VM. Two runs used a development build with packed Git import and file-only copy-on-write workspaces. Git worktrees included Git metadata.

| Time until all four tasks read a file | TreeHarbor | Git mirror + worktree |
| --- | ---: | ---: |
| First batch, including local source setup | 9.94–10.12 s | 0.69–0.76 s |
| Repeat batch, median in each run | 0.16–0.20 s | 0.45–0.55 s |

Git was faster overall on this workload. Across 13 batches and two revisions, setup and first reads added up to 13.57–14.47 s for TreeHarbor and 6.46–7.50 s for Git. Cleanup was measured separately. [Method and raw samples](docs/benchmark-preparation.md#packed-git-import-with-four-parallel-workspaces)

## What inspired it

Anthropic’s Scion inspired the separation of workspaces from source history. OpenAI’s Pando showed how Git can stay part of the daily workflow. TreeHarbor brings parallel agent runs back to a team's ordinary pull requests. [Scion](https://www.anthropic.com/research/redesigning-source-control) · [Pando](https://openai.com/index/scaling-git-at-openai/)

Trying it with your team? A report with your repository size, parallel task count and first setup hurdle helps us choose the next improvement. [Pilot discussion](https://github.com/ArtAndrew/TreeHarbor/discussions/144)

[Implementation status](docs/implementation-status.md) · [Roadmap](ROADMAP.md) · [Contributing](CONTRIBUTING.md)

## License

Source-available under the [Elastic License 2.0](LICENSE).
