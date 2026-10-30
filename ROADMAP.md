# Roadmap

The Connected PR Preview is available. The next milestones improve first use and
repeat tasks while the full source-control platform remains unfinished. Dates are
not release guarantees. Existing components and synthetic acceptance tests are not
evidence of adoption or production readiness.

| Release | User outcome | Release boundary |
| --- | --- | --- |
| **v0.1.0-alpha.1 — Connected PR Preview** | Install one documented deployment, run two isolated agents from an exact revision, retain immutable changes and publish real GitHub PRs | One supported Linux/runtime/deployment profile; exact PR identities, safe cleanup/export, tested signed packages and explicit operational limits. GitHub handles checks, review and merge. |
| **alpha.1.x — pilot fixes** | Complete the same workflow with fewer setup failures and manual interventions | Small patches for observed installation, documentation and workflow defects; do not wait for a new feature milestone to ship a fix. |
| **alpha.2 — repeatable use** | Run another real task without repeating assisted setup | Address pilot-reported friction in configuration, task status and diagnostics. Verify one agent integration if pilots need it. Choose the exact scope from feedback. |
| **Later Connected releases — integration and recovery** | Automate a demonstrated integration bottleneck, then recover the advertised deployment | First certify one TreeHarbor-managed queue/check profile if users need it; ship full Connected publication/queue disaster recovery as a separately verified increment. Neither blocks the PR Preview. |
| **Standalone S2** | Own native history, review, checks and integration without Git/GitHub on the native path | Complete the native lifecycle and fenced migration on the shared Rust core, under the same ELv2. Prioritize after the first Connected usage evidence. |

Current preview: [0.1.6-alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.6-alpha.1),
with [external task creation/status/close through MCP](docs/mcp.md#start-a-task-using-your-own-execution-environment).
Controllers supply their own execution and isolation; TreeHarbor retains exact
source and immutable proposals. The earlier [external API task contexts](docs/external-api-runs.md),
[local diagnostics](docs/doctor.md), [saved workspace requests](docs/workspace-new.md)
and experimental [native preparation retries](docs/native-advance.md) remain available.
Infrastructure setup and the complete two-agent helper are still operator-assisted.
Start with the [operator setup path](docs/connected-pr-preview.md).

## First preview acceptance

Verify a clean installation and the complete packaged two-agent-to-PR demo,
including overlapping edits. Retain durable proposal receipts, bounded retries
without duplicate publication, credential/runtime separation, safe stopped-file
capture and cleanup, source export, and exact-tested-artifact signed provenance.
Document backup instructions and the unsupported disaster-recovery cases.
This preview is for controlled pilot repositories, not unattended production.

## Let pilot use determine expansion

First record installation-to-first-PR time, operator interventions, successful
tasks and whether teams return for another task. Assisted trials are useful, but
must be distinguished from unassisted onboarding. Issue counts, stars and our
scripted demos do not establish product demand.

Share an [onboarding report](.github/ISSUE_TEMPLATE/pilot-feedback.md) or a concrete
[workload proposal](.github/ISSUE_TEMPLATE/workload.md). Keep private source,
credentials and security reports out of public issues; use [SECURITY](SECURITY.md)
for vulnerability reporting.

## Deferred expansion and production readiness

Standalone remains part of the full product goal. S3, remote worker fleets, broader
host policies, HA, VFS, structural merge and distributed SQL do not block the first
pilot. The experimental MCP adapter exists; actual host/onboarding acceptance is
still open. Avoid expanding these components before a concrete workload needs them.

Production readiness requires operational fault campaigns, backup/restore,
upgrades, capacity evidence and pilot acceptance for each supported deployment.
See the [full release gates](docs/implementation-status.md#release-gate-ledger).

Performance targets remain hypotheses: at least 2x better p95 preparation **or**
30% lower full infrastructure cost than the best applicable tuned-Git baseline,
with time to useful work no worse. Start with a pilot's real workload; publish raw
cold/warm data and full costs before claiming an advantage. The broader benchmark
matrix follows evidence of a useful workflow. Scion's internal deployment is not
available as a controlled comparison.
