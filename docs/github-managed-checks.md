# Managed checks for Connected PRs and candidates

Experimental service/CLI support. GitHub continues to own the published branch;
TreeHarbor does not create a native authoritative ref for the imported candidate.
This path runs an explicitly approved offline program on a published PR head or
a retained queue candidate.
It is separate from collecting existing GitHub Actions results.

## Scope and trust

The Human policy owner selects a published PR or a stable, complete membership
capture, the completed GitHub import of that exact revision, a Validator principal
and a check program.
The service fixes those identities in an immutable approval. The assigned validator
prepares a fresh full workspace and starts the approved command. The existing
trusted node verifies materialized files before starting the gVisor runtime.
The immutable image supplies the toolchain and external dependencies; repository
files supply the remaining build inputs. Network, host environment and CI secrets
are unavailable to the task. See [runtime prerequisites](managed-checks.md).

The collector accepts no caller-provided outcome, command or tree. It derives a
receipt from the removed runtime's exact source, placement, image/profile, command,
exit status and ownership. Unknown process outcome remains an error. The node,
operator and assigned Git importer remain trusted infrastructure; this is not
hardware attestation. Import provenance alone does not rehash GitHub history.

These local execution operations do not publish a check or require additional App
permissions. The separate [managed publisher](github-check-publications.md) can
release the collected result as a required App check with explicit authorization.
Complete host-policy verification, fresh remote membership reconciliation and full
live acceptance remain release gates. Existing workflow-policy approvals
do not automatically select a managed program; this first path needs explicit
owner approval for each check. Arbitrary Actions workflows gain no context proof.

## API and CLI

Upgrade service and CLI together and apply additive database migration 27. Existing
queues, permissions, collected group evidence and receipts remain intact; no existing
check gains approval. The preceding schema 26 introduced group execution.
Use `treeharbor client --socket SOCKET --credential-file FILE github --request REQUEST`.
Keep request and credential files in the operator's private directory.

Owner request for a queue candidate (`policy.write`):

```json
{
  "action": "managed_check_approve",
  "key": "approve-candidate-1",
  "epoch": 1,
  "check_run": "11111111-1111-4111-8111-111111111111",
  "specification": {
    "queue": "22222222-2222-4222-8222-222222222222",
    "membership_sequence": 4,
    "import": "33333333-3333-4333-8333-333333333333",
    "validator": "ci",
    "program": {
      "version": 1,
      "runtime_profile": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
      "argv": ["/opt/check", "--offline"]
    }
  }
}
```

Replace the example IDs and profile digest with actual registered values. The
runtime profile must be registered by the operator in this repository; the check
program is stored immutably as part of approval. The completed import must belong
to the same binding/epoch and match the selected full Git candidate revision.

### Before queue admission

[GitHub requires branch checks](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
to pass before a PR can enter its native merge queue, and checks the resulting
group revision again. A group-only required check cannot
bootstrap this sequence. For the PR head, submit:

```json
{
  "action": "managed_check_approve_pull",
  "key": "approve-pr-head-1",
  "epoch": 1,
  "check_run": "44444444-4444-4444-8444-444444444444",
  "specification": {
    "pull": "55555555-5555-4555-8555-555555555555",
    "import": "66666666-6666-4666-8666-666666666666",
    "validator": "ci",
    "program": {
      "version": 1,
      "runtime_profile": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
      "argv": ["/opt/check", "--offline"]
    }
  }
}
```

`pull` is TreeHarbor's completed PR delivery ID, not the GitHub PR number. The import
must contain its exact published Git head and belong to the same repository binding
and authority epoch. Neither an inspection nor a queue entry is required. A draft
PR can be tested; that does not waive GitHub's admission rules. Completed source
publication records do not expire with their former delivery leases.

PR approval, status and execution receipts use version 2 and contain `pull` instead
of `queue`; the retained `subject` contains repository/PR identity, head/base branch
names and the exact head revision. Group responses retain version 1. Both use the
same prepare/start/collect/revoke/release actions below. A PR success cannot satisfy
a group execution, even when both commits have identical files. Use a new approval,
import and execution for the group. Then request publication separately; the
complete live host gate remains pending.

### Shared lifecycle

| Action | Permission | Additional fields |
|---|---|---|
| `managed_check_approve` | Human `policy.write` | `key`, `epoch`, `check_run`, `specification` above |
| `managed_check_approve_pull` | Human `policy.write` | `key`, `epoch`, `check_run`, PR specification above |
| `managed_check_prepare` | Assigned Validator `validation.report` | `key`, `epoch`, `check_run` |
| `managed_check_start` | Assigned Validator `validation.report` | `key`, `epoch`, `check_run` |
| `managed_check_collect` | Assigned Validator `validation.report` | `key`, `epoch`, `check_run` |
| `managed_check_status` | `read` | `epoch`, `check_run` |
| `managed_check_revoke` | Human `policy.write` | `key`, `epoch`, `check_run` |
| `managed_check_release` | Assigned Validator `validation.report` | `key`, `epoch`, `check_run` |

The preparation response contains server-generated workspace and execution-run IDs.
Run the existing node `prepare-once` command for that workspace, submit `start`,
and use the node `run-once` lifecycle. Collect only after the runtime is removed.
The lifecycle is `approved` → `preparing` → `requested` → `collected`.
A check workspace cannot be sealed into an agent ChangeSet.

Use a unique idempotency key per operation across the repository. Repeat the same
key and request after a lost response. Same-key replies are **historical receipts**;
read `managed_check_status` for current eligibility. It recomputes stored evidence
and current trust instead of treating a previous successful response as approval.

`outcome` retains history. `execution_context_verified` is true only for a matching
successful execution with current local approval/trust. `current_locally` means
that the retained source, approval, credentials, binding and policy still match;
it does not mean GitHub has just been queried. `admission_verified`,
`host_policy_verified` and `merge_verified` remain false in this development path.
A change of candidate, authority epoch, queue cancellation, newer proposal,
revoked approval/profile, or explicitly revoked owner/validator credential prevents
new starts and asks the runtime controller to stop. Revoking the node's trust also
invalidates current evidence. Expiry prevents new authenticated calls; explicit
credential revocation additionally fences work already accepted.

Release a collected workspace through the normal drain/cleanup protocol. A revoked
or stale approval that never requested a runtime can also release its prepared
files. A running task must first be stopped and collected. Evidence remains after
workspace cleanup; deleting files never substitutes for stopping processes.

## Evidence encoding

Evidence is SHA-256 over a domain followed by compact UTF-8 JSON with lexicographically
ordered object keys, no trailing newline, and array order preserved. Group evidence
keeps the `treeharbor-github-check-execution\0v1\0` domain and version 1 unchanged.
PR evidence uses `treeharbor-github-check-execution\0v2\0`, version 2, and a `pull`
UUID instead of `queue`. `\0` denotes a NUL byte in these domains.

Both bind tenant/repository/check/epoch, source subject and import, native revision
and snapshot, program/profile/image/argv, owner and validator credentials, workspace,
run, terminal runtime facts and outcome. Runtime facts include attempt, preparation,
placement, node, file manifest, runtime binding, container, timestamps, exit and
worker-generation identities. Encoding vectors are in
[`github-managed-evidence.json`](../crates/metadata/tests/fixtures/github-managed-evidence.json).
These synthetic vectors test encoding, not source authenticity or runtime isolation.

## Verification limits

PostgreSQL tests exercise PR execution without an inspection/queue, exact source and
authority binding, replay, loss, revocation, cleanup and preservation of populated
group evidence through migration. Terminal-success rows in collector tests are explicitly
labelled trusted fixtures, not sandbox evidence. The shared runtime and native
collector have separate real Linux gVisor tests. A complete Connected run on a
live GitHub group, with required App-check publication, is still required.
