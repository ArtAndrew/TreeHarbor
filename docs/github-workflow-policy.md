# Approved workflow files (development)

TreeHarbor can retain the exact root workflow file associated with a saved
[workflow attempt](github-group-attempts.md) and compare its bytes with an explicit
owner policy. Reading a file from GitHub never approves it. A matching root file
does not prove its runner, installed toolchain, transitive actions or dependencies.
Full definition/context, host-policy and final-merge verification remain unfinished.

Upgrade CLI and service together and run the normal database migration before
using these commands. Migration25 is additive; old queue requests and their
receipts keep their original meaning and acquire no implicit policy.

For new branch adoption, [admission policy](github-admission-policy.md) combines
managed programs and workflow requirements under one generation fence. The older
workflow policy described here keeps its historical meaning. A saved root-file
match remains separate from current policy eligibility and execution-context proof.

## Configure an owner policy

Use the generic authenticated client, with a human credential that has
`policy.write`. Bridges and agents cannot grant approval. Save the following as
`policy.json`, replacing the binding, identifiers and example hashes with values
independently reviewed by the repository owner:

```json
{
  "action": "check_policy_set",
  "key": "approve-workflows-1",
  "epoch": 1,
  "binding": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "base_ref": "main",
  "expected_generation": 0,
  "definition": {
    "version": 1,
    "checks": [{
      "required": {"name": "CI / build", "app_id": 15368},
      "workflow_id": "WF_REPLACE_WITH_GITHUB_NODE_ID",
      "path": ".github/workflows/check.yml",
      "content_sha256": "sha256:1111111111111111111111111111111111111111111111111111111111111111",
      "expected_context": {
        "environment": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
        "toolchain": "sha256:3333333333333333333333333333333333333333333333333333333333333333",
        "dependencies": "sha256:4444444444444444444444444444444444444444444444444444444444444444"
      }
    }]
  }
}
```

```sh
treeharbor client --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/policy-owner.json github --request policy.json
```

`base_ref` is the GitHub branch name, such as `main`, matching the PR base field.
`content_sha256` hashes the complete raw UTF-8 file bytes, including line endings;
it is not the Git blob ID. The three context hashes identify owner requirements,
not observed execution. A runner label such as `ubuntu-latest` is insufficient
evidence of an immutable environment.

The result contains the policy digest and generation. First creation expects
generation0. Every replacement or revocation requires the current generation and
increments it once. To revoke, submit `definition: null` with a new key and the
current generation. Identical retries return the original receipt; reusing a key
for a different mutation fails. A return to an earlier policy digest creates a
new generation and cannot revive old dispatch eligibility.

The repository reader can inspect the current policy with:

```json
{"action":"check_policy","epoch":1,"binding":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","base_ref":"main"}
```

When creating a [queue request](github-queues.md), explicitly include
`"policy":{"digest":"sha256:<returned digest>","generation":1}` in its
`specification`. The exact required App/name set must match the policy. The
service verifies the active binding, authority epoch and generation when accepting
the request and immediately before granting a fresh dispatch. Policy replacement
or revocation fences that dispatch. It cannot undo a remote action already sent;
use the explicit dequeue operation when appropriate.

Legacy requests without a pin remain unverified and never inherit a later policy.
Queue status reports `workflow_policy.pin` and its current eligibility separately.
Historical receipts remain immutable.

## Collect executed file bytes

Select a retained version6 attempt-report sequence from `queue_attempts` with
`attempt_linked: true`. Use the existing private queue journal:

```sh
treeharbor github queue --config /private/treeharbor/github-app.json \
  --socket /private/treeharbor/service.sock \
  --credential-file /private/treeharbor/bridge.json \
  --queue "$QUEUE_ID" --epoch 1 --state /private/treeharbor/queue-state \
  --attempt-report 6
```

`6` is an example report sequence, not a run or attempt number. This option
requires an existing queue attempt, never enqueues and conflicts with
`--group-report` and `--check-report`. Pending local reports replay before any
App configuration is read; rerun after recovery to collect a new file.

The assigned bridge needs its existing local `host.merge` permission. The App
token is narrowed to the pinned repository with Contents, Metadata, Pull requests
and Checks read. This collector needs no additional Actions or queue-write grant.
It starts no workflow and attempts to revoke its temporary token even on failure.

The collector interprets the saved executed-file URL only as metadata. It accepts
an exact full SHA in the pinned GitHub.com repository, then constructs a fixed
GraphQL query. It does not fetch an arbitrary URL or resolve a mutable branch.
The response must match the repository, commit, path, regular file mode, byte size
and blob ID. Complete content is hashed as a Git blob using collision-detecting
SHA-1 and independently as raw SHA-256. Truncated, binary, missing and symlink
files fail collection. Same-repository `.github/workflows/<name>.yml` or `.yaml`
paths with ASCII letters, digits, dot, underscore and hyphen are supported.
Other layouts are currently unsupported, not automatically trusted.

Limits: 16KiB UTF-8 file, 48KiB encoded definition report and 128KiB provider
response. The JSON size cap may bind before the content cap. The version7 report
is exclusive of other observation payloads and references the original version6
attempt. The service revalidates the entire retained attempt → check → membership
chain and repository binding. It does not replace the latest queue snapshot.

## Read evidence

```json
{"action":"queue_definitions","epoch":1,"queue":"66666666-6666-4666-8666-666666666666","after":0}
```

Send this using `treeharbor client ... github --request definitions.json` with
repository `read` permission. Pages contain one capture; follow `next_after`.

| Field | Meaning |
|---|---|
| `workflow_file_matches_pinned_policy` | Exact App/name, workflow ID, path and file hash match the queue's immutable policy |
| `workflow_policy.current` | Its digest **and generation** remain active |
| `source_current_attempt_matches` | The saved version6 report observed the selected attempt as current then; this is not a fresh provider read |
| `execution_context_verified` | Always false at this milestone |
| `definition_verified` | Always false until complete definition/context proof exists |

`merge_verified` and `host_policy_verified` also remain false. A previously
approved file can remain a historical match after policy revocation, while
`workflow_policy.current` becomes false. A failed or superseded attempt cannot
become successful merely because its file matched. No native Validation is
created and these reports confer no merge authorization.

## Version1 policy encoding

The immutable policy contains 1–32 unique required App/name pairs, sorted by
name then App ID. Canonical JSON is compact UTF-8, with fields in the order shown
above; the nested required pair is `name,app_id`, and expected context is
`environment,toolchain,dependencies`. Strings use serde_json's JSON escaping.
The encoding is bounded to 32KiB. Decode requires both the exact canonical bytes
and digest. The digest is SHA-256 of
`treeharbor-github-check-policy\0v1\0` followed by those bytes, displayed with
the `sha256:` prefix. Required names are bounded to 256bytes; workflow paths and
opaque IDs are validated independently. This is an immutable owner policy, not
an inferred description of whatever code an agent submitted.

Independent vectors are in
[`workflow-policy-v1.json`](../crates/github-contract/tests/fixtures/workflow-policy-v1.json),
its [digest](../crates/github-contract/tests/fixtures/workflow-policy-v1.sha256),
and [`workflow-definition-v7.json`](../crates/github-contract/tests/fixtures/workflow-definition-v7.json).

A [real installed-App diagnostic](evidence/workflow-file-read-20261025.json)
retrieved the executed workflow from the public acceptance repository, independently
recomputed both hashes and revoked its token. It used a prior provider diagnostic
as source, not a retained product membership chain. No owner approval, runtime
context or final-merge proof is claimed by that live read.
