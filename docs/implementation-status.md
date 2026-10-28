# Implementation status

Updated: 2026-10-26. This file describes implemented behavior and remaining release
gates. A foundation milestone does not complete either mode.

For a concise current overview, see [what works today](../README.md#what-works-today)
and [release readiness](../README.md#what-remains-before-a-full-release). The entries
below retain evidence from individual increments. Their earlier statements about
unfinished work describe the scope at that time; they are not a replacement for
the current overview or the full [release gate ledger](#release-gate-ledger).

Last full CI baseline before this increment: [PR79](https://github.com/ArtAndrew/TreeHarbor/pull/79),
published on 2026-10-26 after all six required checks and both package jobs passed.
The [CI run](https://github.com/ArtAndrew/TreeHarbor/actions/runs/40357750037) passed
402 ordinary tests on Linux and 400 on macOS, 328 real PostgreSQL contracts,
14 static-runtime unit tests and 48 installed-runtime/controller/managed/native
scenario groups on each of Linux x86-64/ARM64, and 11 webhook TLS/failure groups.
These correctness checks do not establish complete mode readiness or performance
superiority. The additional restored-node scenario below needs its own CI evidence.

## Binary-derived Rust dependency reports

Pinned cargo-auditable/Syft tooling now builds and inspects the actual static
binaries on both Linux architectures. [Package CI](https://github.com/ArtAndrew/TreeHarbor/actions/runs/40357750059)
retains checksummed reports beside archives. The audited archives each contained
589 manifest files and 279 Rust library components; binary, observation and report
hashes matched, and both reports passed the official CycloneDX 1.7 JSON schema.
Normalization boundaries also refuse ordinary binaries without an embedded graph,
even when source metadata is available. Declared licenses and source archive
checksums are joined without claiming they identify compiled native inputs.
This is Cargo coverage, not a complete native SBOM or signed provenance.
See [report scope](binary-dependency-reports.md).

## Current GitHub configuration observations

The analyzer recognizes the optional extra-approval rule and caller-bypass
indicator conservatively. Zero-approval interpretation requires the whole
effective/source policy to agree; missing legacy-review settings need explicit
supplementary absence. Positive, malformed, incomplete or contradictory evidence
remains unproven, and conversation requirements remain visible. Raw identities and
existing encoding vectors are preserved.

The configuration collector also reconciles check timeouts with one explicit REST
minute-valued rule, accepting direct equality or an exact sixtyfold observed
GraphQL value. Other/ambiguous values refuse collection. This does not change
ordinary queue decoding or establish merge authority. See
[host-policy scope](github-host-policy.md).

Local combined contract/transport tests passed 207 cases, including actual
loopback provider fixtures, and strict Clippy/formatting passed. Local analysis
of saved provider observations found no remaining unknown configuration fields;
this is not a fresh installed-App report. Current-head CI and full Connected
acceptance remain required.

## Retaining runtime-tested development archives

Runtime CI now preserves the exact archive installed for its managed/native
lifecycle tests, after comparing the installed binary hashes with the archive and
dependency sidecars. The receipt distinguishes the actual checkout, PR head and
workflow run/attempt. It is unsigned and covers those runtime tests only; it does
not establish complete Connected acceptance, native dependency coverage or build
provenance. A separate managed-check App template now documents the permissions
needed for that development workflow without changing the original template.
The [PR82 checks](https://github.com/ArtAndrew/TreeHarbor/pull/82) passed on both
architectures. The retained runtime bundles were independently checked against
their receipts, all 590 manifest files, binary hashes and dependency-report links.
The unsigned receipts record the tested merge revision separately from the PR head.

## Selecting one workspace preparation

`node prepare-once --workspace WORKSPACE_UUID` selects an exact queued workspace
inside the existing repository/node/storage lease scope. An unknown, unavailable
or already prepared selection never falls back to another workspace. `--slot`
continues to choose placement independently; omitting the workspace flag keeps
normal scheduling. Reclaim preserves the attempt/slot and fences the older worker.
The new real PostgreSQL regression, 81 ordinary affected-package tests, strict
Clippy, formatting and CLI help passed locally. The installed Linux harness
prepares the later of two pending workspaces first and checks unknown/completed
selection. That scenario passed on x86-64 and ARM64 in
[PR83](https://github.com/ArtAndrew/TreeHarbor/pull/83).

## Selecting one node execution

`node run-once --run RUN_UUID` restricts the existing node/storage-scoped lease
claim to that execution. Unknown, foreign-node, actively leased and completed
runs return idle without consuming another task. Omitting the flag retains normal
scheduling. Authorization, stop handling and lease-generation fences are unchanged;
no database or object format changes. The real PostgreSQL regression passed locally
for out-of-order selection, no fallback, lease reclaim and revoked access. The
Linux harness exercises two queued tasks on one node through the actual CLI.
That scenario passed on x86-64 and ARM64 in
[PR81](https://github.com/ArtAndrew/TreeHarbor/pull/81).

## Replacement-node recovery drill

The cold restore workload now also retains a requested but unexecuted check.
After restoration, it explicitly revokes the old node credentials and restarts the
service; the old worker credentials must produce an explicit authentication error (rather
than an unrelated missing permission or transport failure), the old worker must
fail with Engine transport available, and no execution evidence may appear for
its check. That candidate cannot land. A newly registered node with fresh local
state, a bounded volume and a new approved program executes a fresh exact check
through gVisor, publishes the native candidate, exports it to ordinary Git and
cleans up. This tests deliberate replacement after stopping the original writers;
it does not resume an old task or reconcile a Connected GitHub publisher.
The scenario passed locally on Linux ARM64 using the actual installed PR79
package. Both architecture CI legs for this increment remain required; this is
not a release approval.

## Static toolchain notice inventory

The package builder now requires seven unchanged upstream notice files for the
pinned Rust standard library, compiler-builtins, libm and musl. A tracked manifest
binds their hashes and compiler version; omission, replacement, an unexpected path
or an unreviewed compiler pin refuses packaging. These files are third-party legal
materials, not additional TreeHarbor implementation. This closes a notice-inclusion
gap; a complete binary SBOM, build-input linkage and signed provenance remain open.
See [the inventory](../third-party/toolchain/README.md).

## Development installation and restore acceptance

[Package CI](https://github.com/ArtAndrew/TreeHarbor/actions/runs/40327207139)
built static Linux x86-64 and ARM64 archives, verified hashes, installed into fresh
prefixes and executed the CLI. The [packaging guide](release-packaging.md) describes
scope and temporary development artifacts. No approved release exists yet.

Runtime CI now uses installed service/runtime-init binaries for the managed agent
and native check workloads. The subsequent [cold restore drill](backup-restore.md)
uses real PostgreSQL dump/restore and copied objects, then verifies published history,
accepted changes and failure on missing/corrupt objects with original storage absent.
Both installed-runtime and cold-restore legs passed in the final successful
[PR76 CI](https://github.com/ArtAndrew/TreeHarbor/actions/runs/40318740436).
These legs do not establish Connected host reconciliation or safe resumption of
restored workers.

The installed static workload exposed a real mode-000 teardown failure: passing
raw filesystem flags through the standard open wrapper lost `O_PATH`. The runtime
now uses a typed `rustix` open, retains ownership/type checks, and has Linux/musl
regressions for unreadable directories, links and foreign ownership. Safe diagnostics
expose only the error kind, with a test excluding private error text.

## Connected Preview priority and candidate assessment

The next release targets one installable Connected workflow for controlled pilots.
Standalone remains a later milestone. Neither an alpha tag nor the current tests
establish production readiness or complete the full two-mode roadmap.

Candidate policy capture now retains complete supplementary visibility in each
actual provider observation round (Report v2, QueueObservation v10), with the old
v1 encoding preserved. Explicit `--host-policy-settings` requests the existing
Contents-write grant for settings reads; the default does not escalate privileges.

The optional `host_policy_sequence` on `queue_admission_evidence` joins a fresh
capture to the owner-reviewed configuration, exact checks/candidate, latest
membership and current trust. Supported complete results can report a bounded
current admission assessment; `merge_verified` remains false and no write grant
is issued. Unknown review/workflow requirements and multi-member policy coverage
remain unproven. See [the contract](github-admission-evidence.md).

Schema 31 adds an immutable [check-dispatch policy record](github-check-publications.md#retained-dispatch-policy-record)
for new successful candidate publications under complete owner policies. It is
transactional with the one-shot dispatch grant and has a historical read action.
It is not complete candidate admission or merge authorization; older dispatches
are not backfilled. Installed-App acceptance of this increment remains open.

Schema 30 retains authenticated report sources. Legacy rows retain their payloads
with unknown attribution, so a new assessment requires fresh attributable evidence;
a replay cannot silently upgrade an old report's trust.

Local verification includes independent v1/v2 encoding vectors, actual HTTP
transport fixtures, PostgreSQL current-policy/source-revocation and populated
schema-upgrade tests, plus real CLI/service recovery for both report versions
after a committed response is lost. Provider payloads in these tests are fixtures;
this increment still needs full CI and live installed-App acceptance before its
behavior can count toward the release gate.

## Owner-reviewed configuration pins

AdmissionPolicy version2 binds a complete Configuration v1 to the owner-approved
policy. Adoption checks the exact upstream repository/base, effective rules and
entire required App/name set. The existing generation CAS, immutable records,
queue/execution fences and replay receipts apply without a database migration.
V1 bytes/digests stay unchanged. Explicit restoration creates a new generation
and does not revive an old queue, even when the required checks are identical.

`github policy-draft` constructs private reviewable input from a stable inspection
and managed/workflow requirements without hand-written rule hashes. It works
offline and refuses output overwrite. `policy-plan` previews the next context names
without approval, including a false `required_matches` during host setup.
`policy-adopt` explicitly stores the reviewed policy with a generation and replay
key; mismatching configured checks cannot be adopted.

Independent v2/v1 encoding tests, real PostgreSQL queue/CAS/ABA boundaries, actual
CLI/service crash recovery and private-file CLI tests exercise this increment.
Local verification on 2026-10-26 passed 388 ordinary workspace tests, 25
PostgreSQL admission regression cases, two CLI/service recovery scenarios,
formatting and strict workspace Clippy. Provider observations in those tests are fixtures. A pin records owner intent,
not a claim that GitHub currently enforces it. Current candidate-bound supplemental
visibility, complete review/workflow proof and final admission remain unfinished.
See [reviewed configuration](github-admission-policy.md#review-and-pin-complete-configuration-version2).

## Configuration inspection before the first PR

`github policy-inspect --config <private-app-config> --base <branch>` reads two
bounded configuration rounds independently of PRs, queue entries and local service
state. The default token is read-only; `--include-queue-settings` explicitly
requests Contents-write solely for GitHub's queue configuration read. No implicit
scope expansion, host mutation, policy approval or admission occurs.

A versioned configuration wrapper binds Profile v1 to explicit ref/repository
visibility, protection flags and complete bypass lists, without changing earlier
profile bytes. The report retains drift and revocation outcome. Supplementary
absence can resolve only specific absent REST fields when the sources agree;
contradictory, malformed, unknown or redacted data remain unproven.

Seven new contract tests, five network scenario groups and the CLI private-input
case cover independent encoding vectors, target/identity substitution, incomplete
or duplicate lists, drift, exact permission requests, revocation and errors.
These are local synthetic-provider tests, not an installed-App acceptance claim.
See [configuration inspection](github-host-policy.md#inspect-configuration-before-the-first-pr).
Current candidate policy/review/workflow proof and full admission remain open, along with both modes' remaining release gates.

## Effective host-policy analysis

The shared Rust contract now analyzes retained configuration separately from the
candidate. Profile v1 is independent of PR identity, preserves raw policy fields,
and has an independent Python/hashlib encoding vector. The historical API page
includes before/after configuration identities, complete required App/name sets,
distinct review requirements and named unproven conditions.

The interpreter compares all queue settings, rejects ambiguous issuers and unknown
clauses, distinguishes hidden bypass from an empty list, and does not infer review
counts/code-owner/team approval from `APPROVED`. Signature, linear-history,
HEADGREEN, non-MERGE and multi-member policy proof remain explicit missing evidence.
All complete verification flags remain false. Existing report bytes, receipts,
queue state and authorization are preserved; no database migration is required.

The focused contract/GitHub suite passed 172 tests, followed by 13 final interpreter
cases including two additional boundaries. The real PostgreSQL historical-read/
replay test and CLI/server process-loss recovery test passed, as did final strict
workspace Clippy and formatting. Exact-head CI remains required before merge. See [policy analysis](github-host-policy.md#policy-analysis)
for the encoding, supported interpretation and limits.

Complete current host-policy/review/workflow proof and final admission remain open. A historical diagnostic is not merge authorization. Both
mode lifecycles, operations, packaging and comparative benchmarks remain required.

## Host policy and review observations

The experimental [host-policy collector](github-host-policy.md) reads complete
bounded policy and review snapshots twice for an exact retained queue membership.
It keeps raw protection/ruleset documents, source identities and review threads;
redacted bypass actors and unavailable documents cannot become empty policy.
Version9 observations are exclusive, preserve old report formats and do not replace
the latest queue state. The API provides scoped immutable pages and an independently
specified evidence digest. The CLI uses the existing durable report journal.

Local verification passed 357 ordinary workspace tests and 309 real PostgreSQL,
materializer/node and service contracts on macOS, plus strict workspace Clippy,
formatting and the four queue-workload cases. The focused contract/transport suite
passed 161 tests. The actual CLI/service test kills both processes after the policy
report commits but before its reply arrives, then recovers the same receipt without
duplicate effects. It also caught a server stack overflow caused by embedding the
large payload in all queue reports; boxing that payload preserved the wire format
and the reproduced recovery scenario now passes. Exact-head CI remains required
before merging this increment.

Full supported-policy semantics,
owner-reviewed host-policy pins, workflow execution-context proof, final admission
and live host-policy acceptance remain open. Empty capture reasons never mean
permission to merge; all complete verification flags remain false. No App rights
were changed and no live acceptance credentials were issued for this increment.

## Candidate check-set evidence

The experimental [evidence reader](github-admission-evidence.md) evaluates every
pinned admission clause for one exact retained queue candidate. Managed requirements
join original collected runtime facts, immutable publication receipts and the exact
provider check. Report version 2 preserves the authenticated numeric/node ID pair;
legacy v1 bytes and replay remain unchanged and do not gain inferred linkage.

Missing, failed, ambiguous or inconsistent evidence prevents a complete set. PR
checks cannot cover the combined candidate. Historical evidence is distinct from
current source/policy/runtime/owner/reporter trust; revocation or cancellation
invalidates current use without erasing completed facts. Later check captures or
unknown same-context publications also fence current use. The read creates no
approvals, receipts or outbox events. Optional final-publication linkage requires
the same full candidate and membership. Independent encoding vectors cover the
returned historical evidence digest.

Workflow attempt/root-file diagnostics remain separate from execution-context
proof: those clauses still report `workflow_context_unproven`. Complete effective
host/review policy, final admission, unattended operation and live adopted-policy
acceptance remain gates. The complete merge/admission/host-policy flags stay false.
Provider/runtime fixtures are labelled. Required exact-head CI checks must pass
before merge; this is not an installable release.

Local verification passed 345 ordinary workspace tests, 308 real database/service
contracts on macOS, formatting and strict workspace Clippy. The new focused suite
passed 10 PostgreSQL cases and one wire
case; publication contract and loopback transport suites passed 14 and 13 cases.
The real CLI/service recovery case exercises both report versions, including a
committed reply lost to the client, restart and exact replay without duplicate
receipts or outbox effects. Published in PR69 after all six exact-head checks passed.

## Branch admission policy

Experimental [Connected admission policy](github-admission-policy.md) adds
schema29: owner-controlled branch policy references, immutable queue/check pins,
explicit adoption and compare-and-swap generation changes. New actions on adopted
branches require the current pin; revocation leaves a tombstone and restoring
older policy bytes creates a different generation. Legacy requests keep their
original receipts and do not inherit newly adopted approval.

The policy combines offline CheckPrograms/allowed validators and workflow
requirements. The owner-only read-only preview returns the next generation's
required App/name pairs without approving or configuring GitHub. Queue admission
checks the exact required set and reviewed rules fingerprint. Managed approval,
prepare/start/first collection and required-check dispatch check the current policy;
execution transitions also recheck dependencies after receipt/outbox writes.
Historical collected results and late publication reports remain recoverable.
Stale requested executions use normal drain/stop/removal fencing for cleanup.

Check target version2 and execution evidence version3 retain admission identity.
Version2 context names bind epoch/binding/base/generation/program, so older
same-SHA success cannot satisfy a rotated policy. Same-context unknown effects
remain fenced; a distinct new context can progress without retrying the old POST.
Legacy target/evidence encodings and permission grants remain unchanged.

Independent canonical/context/evidence vectors, actual loopback transport tests,
PostgreSQL revocation/ABA/CAS/scope/cleanup/migration tests cover this increment.
The focused suite passed 14 PostgreSQL cases plus one wire case. A real CLI/service
test also passed: process death before commit leaves no policy/receipt/outbox;
after a committed response is held from the client, killing both processes and
retrying returns the original receipt. It also covers owner-only preview, context
rotation, revocation/restoration and preservation of the legacy workflow policy.
Local verification passed 336 ordinary workspace tests and 298 real database/service
contracts, plus formatting and strict workspace Clippy. Provider/runtime fixtures
are labelled; live adopted-policy acceptance remains a gate. Full host/review policy,
workflow execution-context and final-admission certification remain incomplete;
verified merge/admission flags remain false. Published in PR68 after all six
required checks passed.

## Live managed required checks and repair

The [2026-10-25 acceptance record](evidence/connected-required-check-live-20261025.json)
now links actual managed gVisor execution to required GitHub App checks, retained
queue membership and the final published candidate. The original PR passed its
own check; its combined candidate failed. A fresh scripted repair passed separate
PR and combined-candidate executions and merged through the protected GitHub queue.
The Rust bridge then confirmed the exact candidate commit, tree and ordered parents
in the target history. Four distinct managed checks were published; old success
was not reused for a changed candidate. The later same-SHA `push` workflow is kept
separate from the selected `merge_group` workflow.

A real service kill plus deliberately hidden local acknowledgement recovered the
same report and check ID without an App configuration or another publication.
All four check workspaces and both scripted-repair workspaces were deleted through
product cleanup. All five temporary credentials were revoked within the authorized
hour, original grants restored, and the VM copy of the App key removed. The runtime
proxy and temporary volume were also removed. The trial used private operator drivers, an external
sandbox and a pinned offline check program. It is not packaged onboarding, an LLM
benchmark, a full server restore drill or evidence of performance superiority.

The trial also exposed a repeated-removal defect: after a settled removal and a
new enqueue for the same PR, the predecessor still blocked removal of the successor.
The conflict check now recognizes the same settled-removal evidence used by
re-enqueue. Unknown operations, absence without a receipt/positive no-send, and
attempts to remove a predecessor while its successor is live remain fenced.
The new PostgreSQL regression failed before the fix and covers three successive
cycles for both acknowledged-removal and positive-no-send paths. All ten dequeue
PostgreSQL tests passed on macOS and Linux, strict metadata Clippy passed locally,
and the previously refused live cleanup request succeeded after the update. The normal exact-head
CI checks remain required before merge.

Full policy/admission certification and unattended reconciliation remain open.
The product's `exact_candidate_published` content fact is true for the repair,
while `checks_verified`, `host_policy_verified` and `merge_verified` remain false
in the final publication view. No operator override changes those flags.

Experimental [required-check transport](github-required-check-transport.md) now
creates an exact App check after stable PR/group and required-context reads, then
rereads its immutable identity. Unknown writes are recovered by read-only complete
scans without retrying creation. Program/epoch-specific names prevent silently
reusing an older program's context. The initial policy proof requires the documented
legacy-protection profile; it is not a complete host-policy or final-merge proof.
Contract vectors and loopback HTTP tests cover drift, failure boundaries, exact
identities and bounded recovery. The later bounded live trial used explicitly
authorized Checks-write and Administration-read permissions on the test App only.
Experimental [managed check publication](github-check-publications.md) now adds
schema28, explicit Bridge `host.check`, execution-derived owner requests, one-shot
attempt/dispatch records, immutable reports and a private-journal CLI. Source,
owner/validator/runtime trust, binding, epoch and deadline are rechecked at the
dispatch boundary. Unknown writes remain fenced after expiry/cancellation and
cannot be reclassified as not-sent. Historical recovery never renews a grant.
Eight focused PostgreSQL tests passed, including populated migration, preserved
credential scope, negative execution, exact group head, revocation, replay and
expiry during receipt/outbox writes, and concurrent no-send/dispatch fencing.
The actual CLI/server kill regression also
passed: a lost reservation reply is safely closed as not-sent, and a lost committed
report reply replays exactly once without an App key. Git import is real; provider
and terminal-runtime observations in this regression are explicitly fixtures.
The bounded live execution/publication trial above now passed;
these components alone do not complete the product admission gate.

Experimental [managed Connected checks](github-managed-checks.md) now reuse the
trusted offline runtime for an owner-approved exact PR head or queue candidate and
completed Git import. PR checks need no existing inspection or queue entry.
Schema27 preserves collected group evidence and adds the separate PR source;
schema26 introduced immutable approval, source and execution receipts;
current status checks trust again. Unknown exits never become success, and source,
authority or permission changes fence new starts. Publication uses the separate managed-check publisher; execution alone does not
complete host admission. Collector-success fixtures in the original regression
are explicitly synthetic; the separate live record above uses actual execution.

Focused validation: `cargo test -p treeharbor-metadata --locked managed_checks --
--ignored --test-threads=1` passed 7 PostgreSQL contracts. The separate metadata
library suite passed 20 tests, including independent evidence encoding vectors.

Experimental [published candidate evidence](github-publication-proof.md) now compares
an exact retained queue candidate with the PR's final commit and observed target
history, including later descendants. Version8 records stable PR/ref snapshots,
ordered parents/tree and immutable-ID provider ancestry comparisons. It reuses the
assigned bridge, private journal and retained membership. This content fact does
not certify CI context, host policy or complete merge admission; those flags remain
false. The [actual installed-App diagnostic](evidence/publication-read-20261025.json)
matched acceptance PR2's candidate/final commit/tree/parents and current target;
its narrowed read token was revoked. Its historical membership input was an
operator diagnostic, not a product-retained chain. Later-descendant behavior is
tested with labelled fixtures. Squash/rebase mappings and full live source-chain
acceptance remain gates.

Experimental [owner workflow policies](github-workflow-policy.md) now pin exact
App/name, workflow identity, path, root-file bytes and expected context identities.
Additive schema25 stores immutable policies, generation-based active references
and explicit queue pins. Replacement/revocation fences fresh dispatch, including
changing away and back to the same policy. Legacy queues gain no approval.
Version7 file observations revalidate the complete retained attempt/check/membership
chain. A root file match is separate from current policy, CI success and runtime
context. The actual killed CLI/server replay and five PostgreSQL policy/upgrade/
source-linkage cases passed locally. The [installed-App read](evidence/workflow-file-read-20261025.json)
retrieved the actual executed workflow and verified its Git blob and SHA-256 hashes,
then revoked its read-only token. Its source remained a provider diagnostic;
product linkage, owner approval, runtime context, host policy and final publication
were not certified. No native validation or merge authorization is synthesized.

Experimental [exact-attempt evidence](github-group-attempts.md) links a selected
retained merge-group check to its workflow attempt and job through independently
validated provider IDs. Version6 records distinguish historical linkage from a
later current attempt, preserve original queue snapshots and recover after a lost
commit reply. Contract/HTTP fixtures, a real PostgreSQL linkage/replay case and the
actual killed CLI/server regression passed locally. The optional collector needs
Actions-read in addition to the existing read token. An explicitly authorized
permission update kept the test App installed only on the acceptance repository.
The [actual Rust diagnostic](evidence/group-attempt-read-20261024.json) linked its
original merge-group check to attempt 1 and revoked the temporary token. Its source
was a prior provider diagnostic, not a persisted product membership report.
Live product linkage, private entitlement, trusted definitions/policy and final
publication remain unverified.
No native validation or merge authorization is inferred from this evidence.

Experimental [candidate-check captures](github-group-checks.md) now bind two exact
commit/tree check reads to an earlier retained membership report. Version5 records
preserve all bounded suite/run/workflow identities and distinguish `merge_group`
from later same-SHA/name/App `push` checks. The read-only App collector, scoped
`queue_checks` API and real CLI/server killed-process replay passed targeted tests.
The [actual Rust public-repository diagnostic](evidence/group-checks-read-20261024.json)
also passed and revoked its token; product membership linkage and private-repository
entitlement were not part of that diagnostic. Current workflow attempt metadata
does not certify historical check attempts. Trusted definitions, exact attempts,
host policy and final published-history proof remain unfinished, with verification
flags false. No permission expansion, database migration or native-validation
promotion is included.

Experimental [retained group observations](github-queue-groups.md) capture actual
main, bounded complete queue membership and immutable candidate parents twice
using a narrowed read-only App token. Version4 reports survive lost responses,
client/service restart and later missing membership. The scoped `queue_groups`
read API pages original captures and correlates late/reordered signed facts by
binding/epoch/provider identity and exact base/head/tree. Reconnecting an upstream
does not mix new events into old evidence. Two agreeing reads and two-parent
relationships remain separate from CI success and final-merge certification.
The [actual Rust installed-App read](evidence/queue-group-read-20261024.json)
passed for both acceptance PRs after the queue emptied, observing actual main
separately from the older PR base and revoking both narrowed tokens. Populated
membership and crash tests use labelled provider fixtures; live populated-group
capture with this collector and full group/check/policy/final-tree proof remain
unfinished. Upgrade CLI and service together; no database migration is required.

A real installed-App queue test passed on the synthetic repository: both exact
PR heads were enqueued through the Rust CLI. Agent B's group passed three tests
and became main; the combined A+B candidate failed the budget test (`2048 > 1024`),
leaving PR1 open outside the queue. Four signed group events were durably retained.
After service SIGKILL, the actual last local acknowledgements replayed identically
without an App key or Git; attempt IDs, report counts and event digests were
unchanged. Temporary ingress was stopped, the webhook disabled and both one-hour
local credentials revoked and verified rejected. See
[queue evidence](evidence/connected-queue-live-20261024.json).
This is host-enforced validation, not general semantic conflict detection.
TreeHarbor still reports `merge_verified: false`: complete group membership,
required-check policy and final-tree certification remain release gates.

Ordinary PR queue dispatch now uses GraphQL `enqueuePullRequest` with the exact
head, `jump: false` and durable attempt correlation. An actual installed-App
REST async request had returned202 and then failed authorization; old requests
retain their original UUID recovery path and are never silently resent. Version3
queue reports retain separately typed enqueue receipts; contradictory transport
histories, attempts and positive no-send claims are rejected. Read-only observation
omits write-protected queue configuration while retaining and checking queue
identity. Dequeue version2 supports that snapshot without widening token scopes.
Upgrade service and CLI together. See [queue operations](github-queues.md).
Local verification passed 83 transport/contract tests, 21 PostgreSQL queue-related
cases and the actual CLI/server restart regression. Formatting and strict
all-target Clippy also passed. These targeted checks are distinct from the live
installed-App evidence and the full required CI suite.

A live Connected path now passed from the real App-imported synthetic revision
through two simultaneous isolated gVisor tasks, separate immutable ChangeSets,
actual App branch/PR publication and exact selected-check capture. The tasks edit
the same `src/lib.rs` in independent workspaces. Each published head passed three
GitHub Actions tests. Publication, PR and inspection tokens were revoked. After
service SIGKILL/restart, all six completed results replayed identically with no
App configuration or Git executable. See the [reviewed evidence](evidence/connected-live-20261024.json),
[PR 1](https://github.com/TreeHarbor/TreeHarbor-acceptance/pull/1) and
[PR 2](https://github.com/TreeHarbor/TreeHarbor-acceptance/pull/2).
These are scripted acceptance workloads, not LLM or performance results. Neither
PR was enqueued during this earlier observation. The later queue acceptance
above established signed live delivery and combined-candidate rejection; repair
and complete final integration mapping remain unverified.

The experimental runtime adapter now selects native Linux x86-64 or ARM64 from
the compiled controller target and requires matching Engine/image architecture.
It retains the same immutable image, external gVisor isolation, bounded ext4
volume, durable intent and uncertain-outcome rules. It has no emulation fallback
or platform override. The CI runtime matrix exercises both architectures with
the same supervisor/controller/managed-run/native-check suites and a real
foreign-image rejection case. Local ARM64 supervisor and controller acceptance
passed in an isolated Linux VM, including simultaneous tasks, actual resource
exhaustion, autonomous teardown and lost-response recovery. This does not add a
macOS sandbox, automatic VM installer or production security certification.

A live [installed-App read connection](github-connection.md#verification-and-remaining-gates)
passed on the public synthetic acceptance repository on 2026-10-24. The actual
Rust client verified pinned identities and revoked its narrowed temporary token.
The service retained a separate live observation in PostgreSQL; SIGKILL/restart
and replay without an App configuration preserved the exact observation and IDs.
A subsequent [actual App source import](github-import.md#resource-and-verification-boundaries)
passed through a trusted Linux aarch64 bridge: the pinned synthetic root commit
was retained as 15 native objects and restored through server export to ordinary
Git with the original commit/tree, all seven paths and strict object verification.
The imported fixture contains one commit. SIGKILL/restart retained its identity, and
recovery required no App key, Git or original local spool. The later publication
and ARM64 runtime observations above are separate acceptance steps. Complete queue
acceptance and the planned remote command transport remain open.

Completed `github import` retries now print the same v1 result as the initial
completion, including exact native revision/snapshot. The assigned bridge's context
supplies that result from its registered source after current authorization checks;
pending contexts supply null. No schema migration or new permission is needed.
Upgrade CLI and service together; older servers do not supply the new context field.
Local verification passed all 12 metadata import cases, the actual CLI/server
restart and full-history transport case, strict all-target Clippy and formatting.
The upgraded Linux client also returned the exact original result for the live
accepted import without an App key, Git or local spool. Broader live histories,
PR/check/queue acceptance and release operations remain unfinished.

The experimental [MCP stdio adapter](mcp.md) exposes twenty named agent tools
through the existing authenticated service. It fixes repository scope from a
private credential and adds no validation, approval, runtime or publication
authority. Native queue request/cancellation is separate from admission; Connected
queue operations retain the owner/bridge workflow. Candidate identity and managed
check status are separate tools, and historical passing evidence never grants
current admission.

The independent wire client passed lifecycle, scope rejection, bounded-input
recovery and protocol-only output checks. Two actual SDK child clients negotiated
the two supported protocol revisions against PostgreSQL and the HTTP service.
They passed overlapping immutable proposals, exact replay, history/diff reads,
native queue request/replay/cancel without moving main, server/client restart,
stale generation/foreign workspace refusal, immediate revocation and drain.
Runtime facts in this test are explicitly fixtures. Local regression passed 263
ordinary workspace tests and, separately, 257 PostgreSQL/materializer/node/service
tests. The final extended MCP scenario and independent wire test also passed.
Strict all-target Clippy and formatting checks passed. Full MCP host/runtime
onboarding, complete two-mode queue-to-publication acceptance and packaged
installation remain release gates.
All five required PR51 checks passed before merge: 263 ordinary tests on each OS,
265 explicit PostgreSQL/Linux tests, real TLS/failure and gVisor lifecycle/native
publication scenarios, formatting, strict Clippy and contribution rights.

Experimental [event recovery](github-event-recovery.md) now connects owner-bounded
requests, explicit `host.recover` bridge permission, durable PostgreSQL scan/detail/
dispatch records and the resumable `github recover-events` command. Schema 24
preserves old events and permissions. One dispatch per GUID/recovery is authorized;
replay or lost responses cannot grant another send. Accepted/unknown/not-sent
reports remain separate from actual signed inbox receipt.

Because the delivery GUID header is not HMAC-signed, correlation compares a
versioned digest of App-authenticated parsed payload with independently verified
inbox content. Raw signature/body provenance remains separate. Changed payloads
fail closed. Coverage and merge verification stay false, including after a scan
reaches its observed pagination end.

Local targeted checks passed eight real PostgreSQL recovery tests, including
populated schema-23 upgrade, cancellation, expiry during commit, concurrent
same-GUID dispatch and spoofed GUID/content mismatch. The actual CLI/server kill
test passed all four lost-commit boundaries (page, detail, send grant and report),
then resumed without the App key and confirmed receipt through the real HMAC
receiver. Transport/contract regression passed, including read-only content
inspection and changed payload rejection. Provider observations are explicit
fixtures; none establishes a real App installation. All five required PR50 checks
passed before merge: 262 ordinary tests on each OS, 264 explicit PostgreSQL/Linux
contracts, strict Clippy/fmt, TLS failure tests, contribution rights and the actual
gVisor lifecycle/native-publication harness. Installed-App and full queue
membership/check/final-tree acceptance remain open.

The [webhook HTTPS recipe](webhook-https.md) now supplies a dedicated Caddy 2.11.7
configuration for the existing Rust receiver. It proxies only POST /github/events
to the private Unix socket, preserves signed bytes, bounds requests/deadlines and
filters request headers/URI from runtime error logs. Certificates are provided by
the operator; no public DNS, automatic issuer or App installation is provisioned.
It does not expose the general command API or act as a sandbox/rate limiter.

Local acceptance passed all eleven scenario groups using the actual config,
real Caddy, Rust receiver and temporary PostgreSQL schema. Coverage includes TLS
trust and replacement, HTTP/2, exact body/header limits, slow requests, duplicate
headers, retained raw group bytes, restart/replay, revocation, proxy SIGKILL before
and after commit, DB lock timeout and log filtering. An initial test incorrectly
expected HTTP 504 for a DB lock; the existing 5s database lock timeout returns 503
before the receiver deadline. The expectation was corrected without changing
production timeouts. Linux execution passed in the required PostgreSQL CI job;
all five required checks passed for [PR49](https://github.com/ArtAndrew/TreeHarbor/pull/49)
before merge. The App/probe data
in this test is an explicit fixture, not live GitHub evidence.

The [GitHub queue acceptance kit](../examples/github-queue/README.md) now provides a
synthetic Rust seed, a pinned `merge_group` workflow, exact overlapping edits and a
private-App permission template. Its local driver runs actual Git/Cargo: baseline
and three independent proposals pass, a textually clean combination fails its
budget invariant, an explicit new repair passes, and a separate overlap conflicts.
The workload is configured to run in both OS CI jobs. This prepares live-host acceptance;
it is not an installed App or a completed TreeHarbor Connected scenario.

A separate read-only operator diagnostic records bounded queue/commit/check
snapshots using GitHub CLI. Its query was accepted against real TreeHarbor PR47,
returning the merged commit/tree/parent and a null queue. This verifies query syntax
and that observation only; populated queue membership and final mapping still need
the eligible live fixture. Truncated pages are explicit errors. Diagnostic captures
always keep `merge_verified: false` and cannot register product validation.

Merged signed-event intake (PR47, schema23) has passed all five required checks.
Inspected CI logs include 244 ordinary tests on each OS, 255 explicit database/Linux
transport tests and 42 actual gVisor runtime PASS lines. The receiver validates
original bytes, commits scoped immutable events/dedup/outbox before acknowledgement,
and recovers after process loss. Actual installed-App callback acceptance, delivery
backfill, complete group membership/check/final proof and the full operational/
installation/benchmark gates remain open. Older entries below record prior stages;
their local counts and then-pending features are historical, not current totals.

Experimental [explicit queue removal](github-dequeue.md) adds an independent Human
PR-scoped authorization, one-shot Bridge dispatch, immutable PostgreSQL reports
and a recoverable `github dequeue` command. Original local queue cancellation
never gains remote-write authority on upgrade. Dequeue waits for terminal original
enqueue evidence; absence, elapsed time or an unavailable result cannot substitute.

Schema22 retains source/grant/history data and adds monotonic positive no-send
evidence. QueueObservation v2 distinguishes a completed preflight failure from a
lost host response; v1 reports remain readable and cannot assert no-send. A new
explicit intent is fenced while an earlier removal could still execute. Known
merge revisions/trees and late receipts survive newer missing observations.

Local verification: 239 ordinary workspace tests passed, as did nine final focused
PostgreSQL cancellation tests. The actual CLI/server kill scenario recovered lost
committed grants and reports without the App key or a Git executable. Strict
all-target Clippy passed. The full explicit database/materializer/node/service run
passed 239 tests. All required exact-head CI remain mandatory before merge. Provider tests are labelled fixtures: minimal
live App permissions, real queue behavior and complete group/final-tree proof
remain unverified. Both-mode lifecycle, operations, installation and benchmark
release gates remain open.

Experimental [GitHub queue delivery](github-queues.md) joins an explicit Human
`host.queue` request to the assigned Bridge's `host.merge` capability, durable
one-shot dispatch and private CLI recovery journal. Selected head checks, PR/base,
rules and queue configuration are recollected before an exact-head native queue
PUT. Replay or an unknown response cannot trigger another PUT. Async result GETs
use the retained UUID and a separately narrowed Contents-write token; that API
requires write permission even though recovery only reads it.

Schema21 preserves previous records/grants and retains append-only queue reports.
Late journal reports preserve the provider UUID without replacing a newer snapshot.
Terminal async results survive later missing responses. A confirmed failure permits
a new explicit owner request; a404, expiry or local post-dispatch cancellation does
not. Actual CLI/server process-loss tests and provider transport fixtures cover
recovery, scope, concurrency and stale targets. No fixture proves a live installed
App. Durable merge-group events, membership/check/final-tree proof
and real host acceptance remain unfinished; `merge_verified` stays false.

Local verification: 231 ordinary tests passed. The explicit database/materializer/
node/service run passed 228 tests and exposed a malformed key in the new CLI test
journal. After correcting that fixture, the remaining CLI scenario passed, including
late-UUID and terminal-result replay without App configuration or Git. Formatting
and strict all-target Clippy passed. Required Linux/macOS, PostgreSQL and actual
gVisor CI are still required before merge; these local results are not live-host
acceptance.

New [exact GitHub check observations](github-inspections.md) provide an owner
request, assigned read-only bridge, typed capture and recoverable `github inspect`
command. Check names and App IDs are pinned to the exact published head. PR head,
base and active ruleset snapshots are compared before/after capture. Missing,
ambiguous, rerunning or non-success results never become selected success. These
are historical observations, not complete host-policy verification or permission
to merge; legacy protection/statuses and merge-group validation remain distinct.

Schema20 preserves existing PR/publication/source records and grants. A shared pure
Rust contract evaluates reports without introducing HTTP/JWT into metadata. The
inspection deadline is independent of earlier branch delivery; current version,
projection cancellation, binding, credential and epoch still fence collection.
Capture/report replay, source staleness, populated upgrade and actual CLI/server
process-loss tests use explicit provider fixtures. Actual installed-App, queue,
full both-mode lifecycle and installable release gates remain open.

Local verification for this inspection increment: 220 ordinary tests and 221
explicit PostgreSQL/materializer/node/service tests passed. Six final focused
inspection tests and the actual CLI/server recovery test passed after historical
receipt handling was finalized. Formatting and strict all-target Clippy passed.
Required Linux/macOS, PostgreSQL and real-runtime CI remain mandatory before merge.

New [GitHub pull request delivery](github-pulls.md) connects an owner-selected
observed branch publication to PR creation or explicit title/body updates. Schema
19 retains immutable intent/lineage/full-graph pins, one-shot dispatch, scoped
provider identity and uncertain-outcome recovery. The Rust CLI journals before
claim, uses narrowed App tokens and checks current eligibility before token creation
and the single host write. Repeated attempts observe only; they cannot authorize
another POST/PATCH. Code updates remain separate branch publications.

Tests cover lost host responses, closed/ambiguous PR search, stale sources, scope/
revocation/epoch/deadline fences, concurrent claims, populated schema18 upgrade and
actual CLI/server restart after losing a committed grant. Host observations are
explicit fixtures, not real installed-App evidence. Large escaped PR text survives
journal recovery. Metadata PATCH preflight is not atomic with a simultaneous human
edit on GitHub. Full installed-App, exact checks/queue, automatic lifecycle,
operational recovery and installable two-mode release gates remain open.

Local verification: 209 ordinary tests and 214 explicit database/materializer/node/
service tests passed. A final focused run passed all eight PR transport tests and
the additional escaped-text journal regression. Formatting and strict workspace
Clippy passed. Linux-only host/runtime paths remain subject to required CI; none
of these results substitutes for installed-App acceptance.

New [accepted-version GitHub projection](github-projection.md): an owner selects
the exact current accepted ChangeSet version and binding. The service derives one
immutable commit from the retained base/operations/result and acceptance time,
reserves and syncs it, then commits a full-graph pin and replayable ready receipt.
Version, binding, authority, permission and deadline fences apply after IO and
after receipt/outbox writes. The assigned publisher exports through a separate
`host.publish` purpose; `host.read` tokens do not gain publication access. This
does not register a base, move a ref or approve any checks/merge.

Local verification: 187 ordinary tests and 200 explicit PostgreSQL/materializer/
node/service tests passed, plus the separate public HTTPS connection test.
Five focused projection tests passed again after final HTTP/error assertions.
Coverage includes concurrent completion, cancellation/version/access changes after
disk IO, expiry inside receipt/outbox writes, full-graph retention, populated
schema-16 upgrade and actual CLI/server kills before and after commit. Exported
Git history passes strict fsck with the exact original parent and accepted content.
These fixtures do not prove a real installed App or PR/queue workflow.

Schema 17 preserves existing imports/bindings/credentials without added grants.
Actual GitHub branch/PR creation, check/queue reconciliation and installed-App
acceptance remain release gates, as do the full two-mode operational requirements.

New [exact GitHub source import](github-import.md): an owner fixes the Git commit,
binding, epoch, deadline and budgets. Only the assigned Bridge's `host.read` may
upload that request; Human source routes remain separate. Schema 16 preserves
existing bindings/observations/credentials and partially uploaded Human sources.
Completion checks the native root's GitOrigin and full graph membership, pins the
source base and commits a replayable receipt/outbox without writing main or granting
validation. Current access, cancellation, binding and expiry fences apply after IO
and after transactional writes.

The Rust CLI uses fresh one-repository App access, private full-history fetch and
native conversion, a durable intent and resumable object upload. The Linux Git
download supervisor bounds time/per-process resources, detects parent loss and
refuses redirects; hard aggregate quotas and arbitrary process teardown still
require an external runtime. Failed partial stores cannot masquerade as completed
bundles. Network download currently supports GitHub.com/SHA-1/Linux only.

Local verification: 185 ordinary tests and 194 explicit PostgreSQL/materializer/
node/service tests passed. This includes expiry inside request/begin/finish writes,
populated schema-15 upgrade, wrong Git identity, revocation between disk write and
publication, concurrent completion and actual CLI/server restart followed by exact
Git export. Strict all-target Clippy and formatting passed. Linux smart-HTTP,
redirect/timeout/parent-loss fixtures run separately in required CI; local macOS
tests do not prove those Linux behaviors. A real installed-App import and the full
PR/check/merge-queue workflow remain release gates.

New [durable GitHub bindings](github-connection.md): Human repository owners
configure/revoke immutable Connected bindings; only the assigned Bridge with
`host.read` can register a bounded connection attempt and its immutable observation.
This permission is separate from publication and cannot be granted to agents.
Schema 15 preserves existing tokens/grants without expanding their permissions.
Current scope, assignment, authority epoch, revocation and post-write expiry are
checked. CLI retries retrieve completed observations without the App key or a new
network probe. Historical receipts do not reactivate revoked bindings or confer
publication readiness. PostgreSQL stores identifiers/observations, never App keys
or temporary GitHub tokens. The server trusts its assigned bridge reporter.

Local verification: 183 ordinary tests and 187 explicit PostgreSQL/materializer/
node/service tests passed. New cases cover concurrent binding/report requests,
post-write expiry, immutable receipts, cross-tenant/role/epoch/revocation fences,
unchanged existing credentials after upgrade, and actual CLI/server restart.
The CLI rejects substituted App pins before network access and reads completed
observations with the App configuration removed. Those observations are trusted
fixtures; they do not establish an installed-App or complete Connected pass.

The earlier [GitHub App connection probe](github-connection.md): an explicit trusted
operator command checks App/installation/repository/owner identity, requests and
verifies one-repository read access, independently enumerates token scope, reads
repository metadata and revokes the temporary token. HTTPS/API version, response
bounds, deadlines, redaction and no-redirect/no-retry behavior are explicit.
Queue ownership eligibility is reported separately from unverified configuration.
That probe alone does not bind a repository. Source import is a separate command;
PR publication and queue admission remain unimplemented.

Local verification: 182 ordinary workspace tests and 178 explicit PostgreSQL/
materializer/node/service tests passed; strict all-target Clippy and formatting
passed. New HTTP-client fixtures include identity substitution, scope escalation,
expiry/suspension, lost token response, bounded slow/large responses and revocation
failure. OpenSSL independently verifies a fixture JWT signature. A separate real
public GitHub HTTPS test passed with API version 2026-03-10 and no credentials.
These are not installed-App/real-host integration evidence: authenticated App
setup, authenticated real-host import, PR/check/queue reconciliation and confirmed merge
remain release gates. Existing native runtime checks remain required in CI.

Explicit [workspace cleanup](workspace-cleanup.md) now joins schema-14 release
metadata to a guarded physical node operation, owner status and `node cleanup-once`.
A durable intent precedes unlink; a separate completion record precedes atomic
source-pin/slot release. The node checks the existing runtime journal and current
Engine, retains the registered placement inode, and resumes after partial deletion.
Completed receipt reads avoid the node/slot locks, including during successor work.
The assigned validator can explicitly release a collected native check's workspace;
immutable evidence remains available and final publication still rechecks context.

Local tests cover real file deletion, permissions, symlink/hardlink containment,
worker SIGKILL during unlink, after local completion and after metadata commit,
slot reuse, CLI/HTTP ownership and exact replay. Database tests cover current
fences, concurrent claims, expiry during writes, populated schema-13 upgrade and
retention of accepted ChangeSet graphs. These are distinct from real Linux/runtime
evidence. The required gVisor harness also passed removed-runtime proof, worker-loss,
active successor replay, check-file release before publication, and source/Git
export after agent-file deletion in
[CI 39932578201](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39932578201).
A missing real runtime marker blocked deletion despite a database removal record.
These observations apply to the pinned Linux/runtime profile, not arbitrary
unmanaged code or host replacement.
Local verification: 169 ordinary tests and 178 explicit PostgreSQL/materializer/
node/service integration tests passed (four concurrent integration cases), including
actual CLI drain/delete/replay and collected-check release. Formatting and strict
all-target Clippy passed. CI independently passed 169 tests on each Linux/macOS
runner, 178 PostgreSQL integration tests and the actual runtime harness. The five
required checks remain mandatory for subsequent heads.
Automatic cleanup scheduling, abandoned-journal/artifact retention, node replacement
and full release gates remain open. This increment is not a product release.

Experimental [managed seals](managed-seals.md) now connect filesystem capture to
owner acceptance. A [selected-file capture library](capture-format-v1.md)
now creates an independently verifiable immutable artifact from an exact baseline,
with explicit replacements/deletions/exclusions and sparse coverage checks. This
library does not prove runtime quiescence. Schema 13 now adds durable owner requests,
stop/cancel, fixed deadlines and fenced node claims, with PostgreSQL tests. A node
adapter rechecks removed runtime identity and persists a separate completion record.
Scoped server publication binds an immutable declaration, proves baseline
membership from the exact original snapshot and commits ready pins/receipts
atomically. The node adapter connects capture to publication and supports exact
completed-response replay. [Owner HTTP/CLI commands](managed-seals.md) now request,
inspect, cancel and accept seals. Acceptance computes the exact sealed operations
through the shared native engine, preserves original run/base lineage, enforces
version CAS and atomically retains the result without carrying over checks.
Automatic workspace cleanup, the wider fault matrix and full release gates remain open.

Local capture/acceptance verification: 164 ordinary tests passed; 164 explicit
PostgreSQL/materializer/node/service tests passed with four parallel test cases.
Seven new publication tests cover scoped baseline proofs, child-first uploads,
restart/reclaim, exact replay, quotas, GC retention, current access fences and
expiry inside finalization. These database tests use trusted runtime fixtures.
Four additional acceptance tests cover version CAS, borrowed reservations, current
access/epoch/drain fences, GC retention and expiry inside final commit. An actual
CLI/server test kills the server before object publication and after commit,
recovers the exact accepted binary/directory result and rejects forged identities
or result bytes. Independent reservation vectors and strict wire tests are included.
These local tests do not prove the real-Linux capture adapter. The required
gVisor harness separately exercises two real writing agents, capture/acceptance, worker
loss after artifact durability, markerless recovery, active-writer stop, exact
checks/review/publication and restoration into ordinary Git. This path passed in
[CI 39912060970](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39912060970),
together with Linux/macOS Rust checks and PostgreSQL contracts. It remains required
on subsequent changes. This evidence covers that pinned Linux/runtime profile,
not arbitrary unmanaged writers, other runtimes or a complete mode release.

New review increment: [immutable review diffs](review-diff.md) show an exact
accepted ChangeSet version, prepared integration candidate or two registered
native revisions through read-only HTTP/CLI. Candidate diffs compare the captured
target head against the prepared merged result and expose immutable target,
policy, proposal and run context. Retained candidates remain inspectable after
supersession, cancellation or publication, subject to current repository/access
fences. Reading never approves a candidate or makes old checks current.
Byte paths, empty directories and bounded text/page behavior remain unchanged.

Ordinary [proposal version updates](change-proposals.md) now atomically make that
ChangeSet's older pending queue entries/candidates stale, matching rebase behavior.
Published history and old evidence remain retained. New candidates require fresh
checks/review; failed updates and completed-request replays cannot repeat mutations.

Local verification for this candidate-review increment: 148 ordinary workspace
tests and 144 explicit PostgreSQL/materializer/node/service tests passed, along
with formatting, strict Clippy and independent fixture reproduction. The required
Linux gVisor job also tests candidate inspection against checked files and ordinary
version replacement during a running check. Local macOS tests do not prove that
runtime behavior. The complete release gates below remain open.

New rebase increment: [immutable ChangeSet rebase](change-rebase.md) adds owner
start/status/cancel and durable server preparation of one accepted version on an
exact native base. Schema 12 retains original run/version lineage and pins, and
atomically makes pending older candidates stale. This creates a single-parent
revision without moving refs or copying validation. General history stack rewriting
remains open; this does not complete Standalone.

New history increment: [scoped native history](native-history.md) now exposes
commit display, deterministic all-parent or first-parent log pages, ancestry and
all best merge bases through actual HTTP/CLI. Queries read only verified commits
from registered repository roots, with current authority/permission fences and
GC holds. They neither move refs nor implement rebase.

New source-history increment: [live source export](live-source-export.md) downloads
a registered native base and its complete reachable graph through scoped reads.
The CLI verifies a bounded binary archive and atomically publishes a full native
pack v3, usable for reimport and optional Git export. Actual CLI tests restart the
server, remove original source packs and recover both imported Git origins and
newly published two-parent native commits. Full server backup remains unfinished.

New interop increment: [Git bundle import](git-import.md) converts an exact Git
commit's complete parent/tree graph into a durable full-source pack v2. Import
preserves original commit IDs/bytes, verifies first-parent deltas, and feeds the
existing scoped resumable source upload and native workspace preparation. No
network credential, main-ref update, authority cutover or CI approval is implied.
The local native path works with Git absent after optional import/export.

New increment: [managed native checks](managed-checks.md) bind approved offline
commands to exact candidate files and derive validation from removed, bounded
runtime observations. Schema 11 persists programs, executions and immutable
evidence. Generic agent exits and manual reports cannot satisfy registered programs.
The node verifies pristine inputs before first start; recovery uses the existing
runtime and preserves build outputs. A required Linux scenario now exercises this
path in `scripts/test-native-checks.py`; local database tests cover replay, current
assignment/credential fences, missing-exit errors and populated schema-10 upgrade.
Local verification: 104 ordinary workspace tests and 117 real PostgreSQL/transport
tests passed, with fmt and strict Clippy. The first real-runtime pass is
[CI 39728681125](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39728681125).
The required job retains the added server-death/collection replay and node-revocation
checks on every subsequent head. Automatic scheduling, logs/artifacts, retention
and release gates remain open.

New admission increment: [native integration](native-integration.md) adds durable
policies/refs, a serial queue, immutable proposal/integration commits, scoped check
runs and human reviews, exact final admission and atomic publication. Schema10
preserves accepted schema9 data. Actual CLI/server restart tests merge compatible
same-file changes and report a conflicting third proposal without Git on PATH.
Validation outcomes in those transport tests are trusted fixtures; the managed
check path above has a separate real-runtime harness. Automatic scheduling and a
complete Standalone release remain open.

The [native snapshot merge](native-merge.md) combines exact
base/current/proposed trees, preserves original inputs and reports structured
conflicts. Real-Git reference fixtures and a no-Git local-storage test pass locally.
The library has explicit conservative limits; its output now feeds native admission.

New increment: [immutable change proposals](change-proposals.md) expose owner-scoped
resumable blobs and server-computed result trees through HTTP/CLI. Schema9 adds
purpose-specific upload state; accepted proposals preserve original run/base lineage.
Local PostgreSQL/transport tests passed, including server process loss and exact
replay. The required Linux harness also submits overlapping real managed-agent output.
Managed filesystem sealing now has the separate acceptance path above. Managed
offline checks are described above.

Development increment: [managed AgentRun](managed-runs.md) requests, owner-bound
status/cancellation, immutable runtime policy/placement records and terminal receipt
replay now have real PostgreSQL tests. Schema 8 preserves existing runs and reserved
preparations. The actual service/CLI-to-gVisor path passed in
[CI 39641169748](https://github.com/ArtAndrew/TreeHarbor/actions/runs/39641169748):
native import, bounded preparation, simultaneous isolated agents, cancellation/drain,
resource outcomes and worker-loss recovery. No complete authority mode is implied.

| Component | Evidence | Remaining |
|---|---|---|
| GitHub App connection/import/projection | Live scoped import and two isolated agent proposals published as real App branches/PRs, exact head-check capture and restart recovery; actual enqueue, signed group callbacks, passing main and incompatible combined-candidate rejection | Complete group/policy/final-tree verification, repair workflow, incremental reuse and unattended operation |
| Canonical SHA-256 object format | Versioned framing, independent golden vectors, strict decoder | Compatibility policy for later formats |
| Immutable directory trees and operations | Exact expected-before checks, canonical subtree operations v2, bounded structural diff, structural sharing, explicit deletes; native merge and Git reference matrix | General history rebase, broader rename/base support, live-server Git interop |
| Local object store | Atomic no-clobber writes, digest verification, file/directory fsync, tenant directories, physical generations and upload receipts | S3, backup/restore, kill fault campaign |
| Native DAG and admission | Scoped history/log/ancestry/merge-base HTTP/CLI, persisted refs/policies/queue/checks/reviews, verified candidate commits, final transactional admission and no-Git CLI/process-loss tests | General history stack rebase, automatic scheduling, full lifecycle and recovery |
| Review diff | Read-only exact ChangeSet/candidate/revision HTTP/CLI, retained integration/policy/version/run identity, scoped GC holds, bounded byte paths and text, actual Git path oracle and restart/page/error tests | Review UI, persistent diff indexing and larger-repository profiling |
| PostgreSQL metadata and outbox | Real database transaction/idempotency/lease tests; repository authority/epoch checks; additive schema 28 with publication, queue, event recovery, owner policy, PR/group managed-check and required-check delivery ledgers; local service pool and crash/replay | Remote service integration and full server restore |
| Native ChangeSet intake | HTTP/CLI immutable proposals and separate ready-seal acceptance, scoped blobs, computed versions, original run lineage, atomic pins/outbox and process-loss replay | GitHub base synchronization, remote transport and retention |
| Protected incremental source reads | Pinned sessions, verified frontiers, bounded prefix batches and descriptors surviving local GC; authenticated workspace-root sessions | Per-tenant service quotas, bulk node delivery and S3 |
| Native file materialization | Private copies from exact snapshots, full/sparse selection, staged expansion preserving edits/deletions, bounded IO and filesystem checks; guarded node publication and selected capture after runtime removal | Cache/reflink, wider capture fault matrix and Git-compatible format |
| Object catalog and retention | Receipt-bound publication, typed edges, atomic persistent pins, read holds, generation-safe retirement/retry; independent reachability model | Automatic GC/reconciliation/dry-run, abandoned-upload cleanup, S3 and full restore |
| Exact validation and native publication | Context binding, immutable scoped receipts, current check runs, reviewer policy, final target/policy/revocation checks and atomic ref/audit/queue/pin commit | CI artifacts/logs, automatic check scheduling, evidence retention policy, review UI |
| Credentials and agent commands | Repository-scoped opaque tokens, transactional revocation, distinct actor classes, owner-bound reads and strict command dispatch shared by CLI/HTTP/MCP | Remote transport and production identity/admission integration |
| Local HTTP service and CLI | Actual Unix-socket server/client, trusted admin and file-preparation node CLI, private credentials, bounded HTTP/pool, strict startup schema, SIGKILL replay, cancellation/rollback, reconnect and graceful shutdown tests | TLS/remote access, packaged installation |
| Workspace baseline API | Owner/generation/epoch-bound directory pages and verified blob download, repeated authorization, final revocation/drain rejection, bounded response ownership, actual CLI/HTTP tests | Bulk delivery to nodes, remote streaming, persistent cache and runtime binding |
| Source graph uploads | Authenticated scoped sessions, child membership, fixed deadlines/budgets, backend identity, binary HTTP/CLI, concurrent retry, restart and schema-v5 upgrade tests | Incremental reuse, streaming/batching and automatic cleanup |
| Native directory import | Explicit selection, descriptor-relative capture, durable pack/journal v1, independent vector, actual CLI-to-server partial-upload recovery and completed replay with Git absent from PATH | Incremental reuse, automatic spool cleanup and runtime-proven snapshots |
| Source delivery profiles | Canonical full/sparse format with independent vectors, immutable scoped definitions, workspace foreign-key binding, v4 upgrade, authenticated CLI/HTTP and exact node use | Profile retention and generation-aware expansion |
| Workspaces and runtime | Owner-bound requests, exact source/runtime bindings, trusted actual observations, drain and terminal replay; real CLI/PostgreSQL/gVisor recovery tests; selected capture/accept API | Automatic resource release, scheduling, wider fault matrix and host replacement |
| Runtime task supervisor | Linux namespace init, privilege guards, environment clearing, guest process limit, child reaping and deadline; real gVisor CI harness | Automatic workspace cleanup and production runtime provisioning |
| Local runtime controller | Versioned immutable profiles, restricted Engine API, bounded ext4 identity, exclusive durable volume assignment and create/start/remove journal; real-runtime fault harness and managed-run adapter | Provisioning, host/engine replacement recovery and cleanup automation |
| Workspace preparation reservations | Operator-registered node storage, scoped job claims, immutable source/slot/attempt identity, renewal/reclaim, exact file-ready observation and drain retention; real PostgreSQL race/upgrade tests | Cleanup scheduling after runtime removal |
| Workspace cleanup | Exact placement/run, current node claims, durable deletion journal, descriptor-relative physical removal, atomic source-pin/slot release, owner status, node CLI and collected-check release; local crash/reuse/expiry/GC tests | Broader runtime/fault matrix, automatic scheduling, journal retention and node replacement |
| Local preparation node | Private physical root identity/OS lock, durable placement intent, exact no-replace publication, independent concurrent slots, current authorization and file-ready acknowledgment; actual process-kill recovery and import-to-files CLI tests | Scheduler/daemon, automated volume provisioning, remote workers, fenced volume migration and cleanup scheduling |
| MCP adapter | Experimental stdio adapter with twenty scoped tools, pinned official SDK, two real client protocol revisions, exact proposals/queue requests and restart/revocation tests | Full host/runtime onboarding, complete two-mode publication and packaged installation |
| Git source export | Local native-pack CLI and bounded Rust full-DAG projection to self-contained Git SHA-1/SHA-256 bundle; isolated batch Git, exact mapping, verified original Git metadata/IDs, restored fsck and no-replace publication | Backup metadata, incremental projection, GitHub delivery |
| Live source export | Scoped registered-base reads with GC holds and authority/credential fences; streaming archive v1 and atomically verified pack v3; actual CLI restart/merge/origin roundtrip | Other retained proposals, all-ref export, full metadata backup, incremental transfer, longer jobs and cleanup |
| Git history import | Pinned self-contained SHA-1/SHA-256 bundle, all-parent native graph, replayed deltas, exact original metadata, atomic full-source pack v2 and resumable HTTP/CLI upload; scoped Linux/SHA-1 App download verified on the live synthetic root | Broader live histories, incremental imports, multi-ref cutover, automated cleanup, larger-repository profiling |
| Connected | Partial: real App import, isolated agent execution, accepted changes, branch/PR publication, exact head-check capture, signed live callbacks and durable replay | Complete installed-App source-to-main workflow with managed required checks, exact group/final-tree verification and operational acceptance |
| Standalone S2 | Partial: durable native admission/publication, scoped history/candidate diff and single-ChangeSet rebase through CLI/API without Git, managed offline checks, registered-source export, plus explicitly separate transport fixtures | General history stack rewriting, full lifecycle, cutover, full server backup/restore |
| Release and benchmarks | Not released or measured | Clean installation, artifacts, SBOM/provenance, comparison and operating cost |

## Local verification

macOS arm64, Rust 1.99.1, 2026-10-21:

```text
cargo test --workspace --locked --offline
100 tests passed without external services, including independent policy/reservation vectors, candidate parent/delta verification and the real-Git merge oracle.
113 PostgreSQL integration tests passed explicitly against local PostgreSQL 18.6: transactions 7, catalog 12, intake/admission 20, protected reads 9, materializer 10, access/commands 10, profiles/upgrade 2, uploads/upgrade 7, preparation/run ownership/upgrade 17, node recovery 7, service process/transport 12.
The native admission suite covers missing/stale/ambiguous/revoked evidence, separate roles/owners, concurrent publication, cancellation, repository boundaries, populated schema9 upgrade and retained graph roots. The CLI test kills the actual server before object catalog publication and after candidate commit, then retries the same request. It independently reads persisted merged content and both-parent ancestry.
Both database URL variables are required for the explicit ignored integration suites.
cargo clippy --workspace --all-targets --locked --offline -- -D warnings
Passed.
cargo fmt --all
Applied before the checks above.
Earlier published CI evidence (Linux, macOS, PostgreSQL and gVisor; commit c086d3a):
https://github.com/ArtAndrew/TreeHarbor/actions/runs/39624242714
The node increment adds its real database suite to the required PostgreSQL CI job.
```

The separate `gVisor task supervision` CI job exercises the
[runtime supervisor](runtime-supervisor.md) on a real disposable Linux runtime.
It is not run by the ordinary local test command, and the macOS checks do not
provide sandbox evidence. The harness includes namespace teardown, a launcher
which exits before completion, guest PID limits, memory exhaustion and a dedicated
persistent disk capacity. These supervisor-only cases do not produce READY or
Validation receipts; the separate managed-run harness covers the service boundary.

The [local runtime controller](runtime-controller.md) now has a separate real-engine
fault harness in the same required job. It delays actual Engine traffic and kills
the Rust controller around create/start/remove, checks independent task volumes,
retains uncertain starts and refuses configuration drift. Local tests verify its
canonical profile/intent vectors, journal exclusion and invalid recovery records.
Linux runtime evidence comes from the required CI job, not macOS tests. The additional
managed-run harness uses actual owner CLI requests, native import and volume-backed
preparation. It covers independent same-path writers, owner-bound results, nonzero/OOM/
deadline/disk outcomes, cancellation/drain and real worker death around Engine RPCs.
Permission changes on the task directory cannot disable runtime cleanup. Managed
source sealing is described above; workspace deletion remains unfinished. Task
results cannot grant trusted Validation.

The real runtime run above passed isolation, rejected invalid bootstrap settings,
primary exit and descendant teardown, PID-1 signal/proc attacks, timeout after the
launcher exits, and external cancellation. Exhaustion at task budgets 4/16/64
observed exactly 4/16/64 live tasks including the primary, repeated after reaping;
attempts to raise the hard limit failed. Actual memory exhaustion stopped the
sandbox with an OOM observation. The 128 MiB workspace filled without changing
the separate neighboring volume. These are correctness observations for the
pinned test profile, not throughput measurements or complete node integration.

Test stores used for pure model tests are not a server implementation. Local
storage tests use actual files; they have not yet proved power-loss behavior on
every filesystem. No benchmark timing or customer adoption is claimed.

The catalog tests use real files and PostgreSQL, including simultaneous pin/GC,
duplicate hold tokens, deletion retries, a crash boundary between deletion and
database completion, and published-v1 schema upgrade. Native intake now commits a
ChangeSet version, lineage, graph pins, response and outbox atomically, and recovers
the same receipt after a worker process is killed. ChangeSet submission is now
exposed through local HTTP/CLI with explicit immutable blobs and operations.
Remote transport, automatic workspace cleanup, full application
failure injection, fenced restore and automatic maintenance remain release gates.

Protected local reads now resolve only requested object frontiers and retain opened
file descriptors across GC without holding a database transaction during content
verification. Tests include expired sessions, complete physical collection, generation
reuse, corrupt bytes, resource limits and selective reads without loading commit
parents. Authenticated workspace baseline reads now expose directory pages and verified file
bytes through the local CLI/HTTP service. Bulk node delivery and scheduling remain
absent; runtime isolation is provided by the separate managed-run boundary. See [source reads](source-reads.md) for the wire format,
read limits and the precise revocation boundary.

Native filesystem preparation now consumes these reads and returns a private tree
with a sibling baseline manifest. Sparse expansion copies a previously stopped
tree into a new candidate, preserves edits/deletions/untracked files and adds source
paths without modifying the old tree. Limits, symlink/case behavior and cancellation
are tested using actual files and PostgreSQL. See [manifest v1](workspace-manifest-v1.md).
The bounded baseline loader verifies an independently recorded exact manifest hash,
scope, snapshot and canonical profile, rejects unsafe private-file provenance and
malformed metadata, and preserves current agent files. Tests cover a new process,
the independent v1 bytes/hash and expansion of a reopened catalog-backed tree with
local edits/deletions/untracked files. Historical preparation counters are explicitly
unavailable after loading. This is a baseline recovery primitive, not a node daemon
or proof of execution readiness.
The caller must still prove runtime
quiescence, fence/persist the new generation and bind the result before READY;
none of those steps is implied by a materializer receipt.

[Preparation reservations](workspace-preparations.md) now persist the source,
node and slot identity before physical work, with scoped claims, current lease/
authorization checks and immutable completion observations. This new path records
file readiness with an empty runtime binding; it cannot start an agent through
the legacy running callback. Real PostgreSQL tests cover concurrent retry,
restart/reclaim, expiry during insertion, cross-node/tenant denial, revocation,
drain retention and populated-v6 upgrade. The [local node](local-node.md) now binds
these reservations to a private physical storage identity and exact local intent.
It publishes a synced tree with a placement-bound baseline receipt, then records
file readiness in PostgreSQL. Actual worker kills before publication, after
publication and after acknowledgment recover the same attempt/path; published
files retain their inode and local edits/deletions/untracked files survive reopen.
Concurrent workspaces, queued-IO cancellation, real lease renewal and renewal
blocked behind the completion transaction are exercised. The CLI/HTTP test runs
native import through file preparation and new-process reopening with Git absent.
That file-only test launches no runtime. The separate managed-run harness exercises
bounded volumes and actual runtime execution. Automatic scheduling, volume provisioning
and safe published-tree removal remain unimplemented.

Repository credentials and the [agent command library](agent-commands.md) now check
permission, principal kind, token expiry/revocation and current grants in the
transaction that accepts a write, including receipt replay. Tests exercise
revocation racing with a blocked write, expiry after workspace effects and before
commit, token rotation, scope/identity injection, owner-only reads, node observations,
authenticated ChangeSet acceptance and a populated v3-to-v4 upgrade. Role checks
do not prove that a sandbox ran or that a validation result is true. The local
HTTP/CLI adapter now uses this boundary and provides private credential-file
management plus trusted database administration. Real process tests cover a lost
mutation response followed by server kill/restart and exact receipt replay,
duplicate-server exclusion, stale sockets, revocation, malformed/chunked HTTP,
connection/header deadlines, schema mismatch, pool reconnection and transaction
rollback before pool reuse. See the [local guide](local-service.md).
MCP, remote TLS transport, per-tenant service admission and complete
product workflows remain absent. A Unix socket and private files do not isolate
untrusted code running as the server's OS user.

## Release gate ledger

- [x] Initial core, local store and native history contract tests.
- [ ] PostgreSQL acknowledged-change transactions and replay after process loss.
- [ ] S3 conditional immutable writes and catalog/GC race tests.
- [ ] API authorization, tenant separation, trust boundaries and resource limits.
- [ ] Workspaces, sparse expansion preserving edits, private Git state and drain.
- [ ] Two-agent overlap demo, conflict resolution, rebasing and exact validations.
- [ ] Connected path against real GitHub; required checks cannot be bypassed.
- [ ] Standalone native path with Git absent and host access disabled.
- [ ] Fenced migration between authority modes with one writer.
- [ ] Full-DAG GC, backup/restore and self-contained Git export drills.
- [ ] Contribution-rights process, third-party notices and required GitHub checks.
- [ ] Clean install/update/uninstall; checksummed release artifacts and provenance.
- [ ] Raw tuned-Git and analogue benchmark data, cold/warm and full costs.
- [ ] Pilot onboarding evidence and ten-team recruitment material.

Do not mark unchecked gates complete based on a mock, design paragraph or planned CI job.


## Native Git export increment — 2026-10-22

Added the optional Rust `git-bridge` and `treeharbor git-export --pack ...`.
The library projects complete native DAGs to self-contained SHA-1/SHA-256 bundles;
the CLI consumes completed local source packs. Seven bridge tests and one actual
CLI test cover independent roots, ordered multiple parents, exact binary/mode/link
content, raw path quoting, shared directories, a separately calculated Git object
vector, restored strict fsck, resource failures, killed process groups, concurrent
no-clobber publication and inherited hostile Git configuration. The exporter uses
one generated Git pack (formerly fast-import), without checkout or a process per file.

Local macOS arm64, Rust 1.99.1, Git 2.54.0 (Apple Git-157):
`cargo test --workspace --locked --offline` passed 112 ordinary tests;
`cargo clippy --workspace --all-targets --locked --offline -- -D warnings` passed.
Public format/limits/operations are in [git-export.md](git-export.md). This did
not include automated Git import or leased live-server export (both added later),
GitHub delivery, authority migration or full product backup. No performance
superiority, production readiness or complete mode release is claimed.


## Subtree operations increment — 2026-10-22

Canonical ChangeOperations v2 adds typed Directory before/after edges while all
leaf-only and existing object frames retain v1 bytes/IDs. The core computes a
bounded structural snapshot diff and applies file/directory transitions with all
predicates checked before writes. The candidate builder replays the new delta;
its merge profile is explicitly v2. Source graph upload and Git export accept
mixed-version graphs. Public leaf-only proposal HTTP v1 is unchanged.

Independent Python frame/hash vectors, a 49-pair exact-tree replay matrix,
stale-subtree and overlap rejection, kind/version failures, all budget boundaries,
unchanged-subtree reuse, candidate parent/delta checks and real Git mixed-history
restoration are covered. Real PostgreSQL tests retain both directory graphs during
GC, enforce upload-session membership and replay after reconnect; an old pending
merge-profile reservation must be cancelled/requeued rather than recomputed.
See [format and upgrade boundaries](subtree-operations-v2.md). Git bundle import
was added in the later increment below; agent-facing subtree uploads remain open.


Local verification for this increment (macOS arm64, Rust 1.99.1): 119 ordinary
workspace tests passed. The full PostgreSQL/transport suite passed 119 tests,
then the new old-profile upgrade test passed separately (120 total); the extended
actual CLI/HTTP crash/replay test also passed with a server-derived v2 directory
addition and durable full-graph reads. Strict workspace Clippy and fmt passed.
No runtime outcome is fabricated by the new tests; the existing transport fixture
is explicitly separate from actual task/check execution in required gVisor CI.

## Original Git metadata and exact projection

NativeCommit kind5/v2 adds an optional original Git OID and a typed reference to
the raw commit Blob. Existing v1 IDs remain unchanged; subtree kind4/v2 remains
compatible. Graph pins and read holds retain the raw bytes. Scoped uploads require
the raw child in the same session; they do not authenticate the claimed Git identity.

Export independently verifies original hash, full tree, ordered projected parents,
author/date/message and strict Git structure before publishing a bundle. It preserves
committer, signature and extension header bytes; signature validity is not checked.
New native descendants receive new IDs. A single bounded non-delta pack replaces
fast-import to preserve arbitrary headers. Linux/macOS tests compare actual Git
objects and restored histories for SHA-1/SHA-256; independent Python native vectors
and PostgreSQL retention/upload tests cover the format and reachability.

See [format and upgrade](git-origin-v2.md) and [export](git-export.md). This is the
representation/export prerequisite used by the Git bundle importer added below.
No Connected release, GitHub delivery, migration, performance result or complete
backup/restore is implied. Old readers must be upgraded before kind5/v2 writes.

Local verification (macOS arm64, Rust 1.99.1, Git 2.54.0): the full ordinary suite
passed 122 tests, followed by passing the expanded bridge and CLI suites with two
additional tests (124 total). All 122 real PostgreSQL/transport tests passed.
Strict workspace Clippy and formatting checks passed. Independent native vectors
regenerated identically. Actual runtime isolation remains covered by required
Linux gVisor CI, not by a local macOS container claim.


## Complete pinned Git history import

The isolated bridge now imports self-contained bundles without checkout or a
Git process per file. It verifies exact Git IDs, preserves every reachable
ordered parent and raw original commit, and replays each native delta against
the first parent. Import remains an optional interop boundary; it does not
publish native refs or authenticate claimed authors/reviews.

Full-source pack manifest v2 has an independent Python vector. Its whole output
is private, validated and synced before no-replace publication. Selected-directory
pack v1 bytes and fingerprints remain unchanged. Existing scoped source upload
journals accept either pack and retain their immutable retry identity.

Actual SHA-1/SHA-256 Git fixtures cover independent roots, merges, byte names,
file/directory transitions, binaries, executable bits and links; export after
input deletion restores every original commit ID and raw payload. Corrupt,
incremental, absent-tip, unsupported and budget inputs fail. Concurrent output
publication has exactly one winner. Actual CLI/PostgreSQL tests exercise partial
upload plus server loss, completed-receipt replay, source deletion and native
node preparation/reopen with no Git on PATH.

See [import format, limits and operations](git-import.md). Hard resource isolation,
GitHub delivery, network fetch, multi-ref migration and full backup/restore remain
open. Selected-object bounds do not limit Git's earlier unpacking of unrelated
bundle objects; external disk/memory/CPU quotas remain required.

Local verification (macOS arm64, Rust 1.99.1, Git 2.54.0): full ordinary suite
passed 129 tests; the final targeted suite passed with one additional branch-
selection case (130 total). All 124 real PostgreSQL/transport tests passed,
including both new full-history CLI scenarios. Workspace Clippy with warnings
as errors, formatting and unchanged v1 golden vectors passed. Actual Linux
runtime execution remains a separate required gVisor CI gate.

## Scoped live source export

The service now exports one registered native revision and its complete source
closure through authenticated, leased reads. Authorization/authority changes
reject subsequent batches and final publication. A source read hold prevents
GC retirement during construction; anonymous server scratch outlives neither its
request owner nor process. The client downloads to disk, verifies archive v1,
builds full native pack v3 and syncs a no-replace output. v1/v2 packs remain readable.
There is no Git dependency in this download path or PostgreSQL schema change.

Actual service/CLI tests kill and restart the server, remove original source
packs, export newly published two-parent commits, compare every native frame,
restore with real Git and reimport the downloaded pack. A separate imported-Git
case preserves raw original commit metadata. Database tests cover scoped membership,
wrong bound permission, epoch changes, revoked tokens, expired holds and GC
retention. Independent archive/manifest vectors and corruption, exact-closure,
no-clobber/race tests exercise the client. One deliberately fake Unix HTTP peer
tests interrupted/mismatched downloads and exact retry; it is not server-auth
evidence and is separate from actual server tests.

Local verification (macOS arm64, Rust 1.99.1): the full ordinary suite passed
133 tests, followed by four archive tests including one added typed-closure test
(134 ordinary tests total). All 127 explicitly enabled integration tests passed,
including the real PostgreSQL/server suites and the client fault-transport test.
Formatting and strict workspace Clippy passed. Required GitHub Linux/macOS,
PostgreSQL and actual gVisor checks still apply before merge.

See [format, access rules, resource bounds and cleanup](live-source-export.md).
This does not implement full metadata backup/restore, all-ref migration, export
of every retained proposal, automatic cleanup or incremental/long-running export.
No performance claim or complete mode/release claim is made.

## Scoped native history queries

Added `client history` and `/v1/history/read` for show, log, is-ancestor and
merge-bases. The history core orders complete ancestry children-before-parents,
using raw IDs to break ties; ordered parent lists remain unchanged. First-parent
display is explicit. All best bases, including criss-cross bases, are returned.
Missing data and exhausted budgets fail instead of becoming false/empty results.

The service reuses authenticated registered-base reads and graph holds, traverses
commit edges without loading source file blobs, closes all holds even when the
second root fails, and rechecks access before success. Response-size accounting
includes JSON escaping. Wire v1 has independent request/response vectors. No
native object format or schema migration changed; the service now directly uses
the existing workspace history crate.

Local verification (macOS arm64, Rust 1.99.1): 138 ordinary tests and all 128
explicit integration tests passed, as did formatting and strict workspace Clippy.
Actual PostgreSQL/CLI tests compare criss-cross bases and first-parent history
with Git, remove original inputs, restart the server and use a read-only credential.
They cover complete pagination, nonmember roots/targets/cursors, same-tenant other
repository denial, authority epoch changes, token revocation and hold cleanup.
The existing native publication scenario also reads newly landed merge history.
An independent DAG matrix checks complete membership and parent ordering for 64
generated graphs. Linux/macOS and actual gVisor remain required CI gates.

The initial query profile loads bounded complete ancestry on every page. There is
no persisted ancestry index or performance claim. General history stack rebase, complete lifecycle,
Connected GitHub, backup/restore and release requirements remain open.

## Durable single-ChangeSet rebase

Added owner-authorized start/status/cancel and `/v1/changes/rebase` preparation,
with CLI shortcuts. The core verifies the original proposal, merges exact snapshots,
then writes a new delta and single-parent commit against the pinned new base.
Schema 12 records immutable source/result lineage, retains the original AgentRun,
and atomically pins the new version and supersedes older pending candidates.
The protected branch is unchanged and old evidence cannot admit the new version.

Local verification (macOS arm64, Rust 1.99.1): 144 ordinary tests and 135 explicit
PostgreSQL/transport integration tests passed; formatting and strict workspace
Clippy passed. New tests include actual Git rebase as an independent oracle,
aggregate read/write budgets, immutable source/run retention, simultaneous version
submission versus rebase, revoked credentials/epochs, cancellation/expiry, GC
retention, populated schema-11 upgrade, and CLI recovery after server death at
object publication and after database commit. Actual CLI tests also export and
read the new revision with Git absent. Their CI outcomes are explicitly fixtures.

The required Linux gVisor harness now includes rebase during an actual running
check and fresh check/review before publishing the new version. It remains a
required CI gate, separate from local transport tests. See [wire format, limits,
retry and operational behavior](change-rebase.md). General history stack rewriting,
Connected GitHub, full lifecycle/restore and release/benchmark gates remain open.


## Scoped immutable review diffs

Added `/v1/review/diff` and `client diff` for exact accepted versions or registered
native revision pairs. The endpoint requires repository read permission, retains
both roots with atomic holds, checks current access/authority per batch and before
success, and closes all holds on ordinary failures. Results preserve original
run/base/version identity, byte paths, modes and empty directories. Text is bounded
separately from complete path enumeration; oversized escaped JSON fails explicitly
and can be recovered with smaller pages or metadata-only reads. No refs, reviews
or validation are written.

Local verification (macOS arm64, Rust 1.99.1): 147 ordinary tests and all 140
explicit PostgreSQL/materializer/node/service tests passed. Formatting and strict
Clippy passed. Independent stdlib framing/hash/unified-patch vector, real Git
changed-path comparison, text-patch application, actual server restart/removal of
original source packs, older-version reads after rebase/drain, scoped access,
revocation/epoch/lease fences, GC protection and response-budget recovery are
covered. The required Linux gVisor harness additionally reads actual immutable
agent proposals with reviewer credentials before approval. All required CI jobs
must pass on the exact head before merge. See [review diff](review-diff.md) for the
wire contract, limits and explicit distinction from integration-candidate review.

## Exact GitHub branch transport

The trusted Rust bridge now has a separate branch-publication transport. Native
conversion creates a process-local verified artifact; a bounded Linux worker
checks its bytes and updates only a generated service branch with an explicit
expected old Git commit. The App adapter requests Contents-write/Metadata-read
on one pinned repository, verifies current identity/scope/lifetime, reads before
and after one push, and preserves uncertain outcomes without blind retries.
Source access remains read-only. Failed token revocation is reported separately
from a verified branch observation, with the upstream expiry timestamp.

Tests cover scope/identity/lifetime, stale heads, failed observations and revocation.
Linux ignored tests in the required database job use actual Git smart HTTP with
synthetic credentials: concurrent writers, applied update with lost response,
redirect refusal, modified artifact, deadline and parent-SIGKILL cleanup. These
do not establish installed-App or real GitHub queue acceptance. The required five
checks must pass on the exact PR head before merge.

The CLI joins transport to durable owner authorization and persisted reports.
PR/check/queue operations and real installed-App evidence remain unfinished.
See [branch transport](github-publisher-transport.md).

## Durable publication intent and dispatch ledger

The coordinator now retains owner requests for exact ready projections, derives
branch identities per tenant/repository/ChangeSet, pins their full graphs and
accepts verified mappings only from the assigned HostPublish bridge. A fresh begin
commit issues one dispatch flag; replay returns its receipt without another flag.
Uncertain attempts cannot be reclaimed as permission to send again. Cancellation
after dispatch preserves the attempt; scoped observations can record a remote
effect after a projection or binding stops accepting new work. Main and check
authorization remain unchanged.

Schema 18 is additive. Tests cover populated upgrades, retention, foreign scope,
wrong report/mapping identity, concurrent claims, replay, revoked credentials,
version/epoch/cancellation/deadline fences and rollback after delayed receipt/outbox
writes. The actual Unix-socket CLI/service test kills the server before and after
begin commit, then verifies historical recovery without redispatch permission.
Its Git mapping comes from a real self-contained Git export; its upstream reports
are explicitly trusted fixtures, not installed-App evidence.

The trusted `github publish` CLI now connects these records to branch transport.
It binds a private journal to the exact intent, records mapping/attempt before
claim, and invokes the write adapter only for a fresh dispatch. Current eligibility
is checked before App token creation and before push. Replays use read-only branch
observation, including without Git/source packs. Reports are durable before service
submission, and terminal receipts require no App credentials. Unknown write-token
cleanup cannot be cleared by revoking a later read token.

Journal/guard tests and actual CLI/service recovery tests cover retained reports,
cancelled attempts, exact mappings and missing local/App files. The Linux transport
test additionally drops the actual CLI's begin response after commit, kills the
process, restarts the server and checks that a saved observation recovers without
another dispatch. Its host observation is explicitly a fixture. Full installed-App
publication, PR/check/queue, automatic reconciliation, cleanup and cutover remain
required. See [publication requests](github-publications.md).


## Signed GitHub event inbox

The separate `webhook-serve` command verifies HMAC-SHA256 over original raw bytes
before exposing typed identities. Owner-created event channels route a pinned
App/installation/repository to one tenant, binding and epoch. PostgreSQL retains
immutable raw content, normalized v1 facts, bounded delivery aliases and an outbox
record atomically. Duplicate delivery/content does not produce another logical
fact. Conflicting UUID reuse, missing/revoked bindings and exhausted retention
budgets fail explicitly. No event creates a check, approval or verified merge.

Additive schema23 preserves existing configuration without creating subscriptions.
The public command API creates/revokes channels and pages normalized facts; there
is no agent command for asserting that a webhook was verified. The receiver uses
one or two operator-configured secret files and a separate private Unix socket.
Its App signing key and repository write credentials are not required.

Local verification (macOS arm64, Rust1.99.1): 244 ordinary workspace tests,
247 explicit PostgreSQL/materializer/node/service tests, formatting and strict
all-target Clippy passed. This includes seven inbox tests and an actual receiver/CLI
process test that kills before/after commit and retries the original delivery.
All five required exact-head CI checks must pass before merge. No external dependency versions were added. See
[signed events](github-events.md) for configuration, wire examples and limits.

Automatic delivery reconciliation, retention/archiving, TLS installation,
installed-App callbacks and complete group/membership/check/final-tree proof remain
unfinished. Both complete product modes and all release gates remain required.
