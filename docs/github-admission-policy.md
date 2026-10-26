# Connected branch admission policy (development)

An owner can adopt one versioned admission policy for a GitHub binding, authority
epoch and base branch. It lists approved offline programs and their allowed
validators, plus required GitHub workflow definitions. The service uses this policy
to fence new queue admission, managed execution and required-check dispatch.

This experimental feature adds database migration29. Upgrade service and CLI
together and run migration first. Existing queues, executions, receipts and
credentials retain their meaning. A repository that has never adopted a policy
continues using its legacy, unverified path. Adoption does not approve earlier work.

## Prepare and preview

Use a Human credential with `policy.write`; reads of stored policy additionally
need `read`. Register the actual runtime profile and validator grants using the
[managed-check guide](github-managed-checks.md). A program pins its image/toolchain
and external dependencies through that immutable profile. Each execution still
requires explicit owner approval for its exact imported revision.

Save `plan.json`, replacing the example identifiers and digests with reviewed
values from your installation. `rules` is the complete `RuleObservation` from
trusted inspection of the intended effective queue rules, not a user-selected
success flag. This example requires one managed program:

```json
{
  "action": "admission_policy_plan",
  "epoch": 1,
  "binding": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "base_ref": "main",
  "expected_generation": 0,
  "definition": {
    "version": 1,
    "rules": {
      "sha256": "1111111111111111111111111111111111111111111111111111111111111111",
      "count": 5,
      "merge_queue": true
    },
    "managed": [{
      "program": {
        "version": 1,
        "runtime_profile": "sha256:2222222222222222222222222222222222222222222222222222222222222222",
        "argv": ["/check"]
      },
      "validators": ["ci-main"]
    }],
    "workflows": []
  }
}
```

```sh
treeharbor client --socket "$SOCKET" --credential-file "$POLICY_OWNER" \
  github --request plan.json
```

Preview returns the proposed digest, next generation, reviewed rules and exact
required App/name pairs with `approved:false`. It writes neither policy approval
nor GitHub configuration. `expected_generation:0` applies only before first
adoption; later changes must use the current generation. A stale preview fails.

For GitHub Actions requirements, add entries to `workflows` using the
[`CheckApproval` shape](github-workflow-policy.md#configure-an-owner-policy): exact
App/name, workflow ID/path, raw file digest and expected execution-context hashes.
The combined required set must contain every intended context with unique names.
These declarations do not establish actual Actions runtime context by themselves.

## Adopt and configure required checks

Change the preview request's `action` to `admission_policy_set`, add a unique `key`,
and retain the reviewed definition and expected generation. Submit it through the
same authenticated client. The response contains the immutable policy digest,
new generation and required pairs. Identical retries return the original receipt;
reusing a key for another mutation fails. Concurrent replacements use compare-and-
swap: only one request for a generation can succeed.

Configure GitHub's required checks explicitly using the returned pairs. The
supported verifier requires legacy branch protection enforcing admins, no
force-push/deletion/review bypass and an effective merge queue. For an adopted
policy, both preflight reads must match the reviewed rules fingerprint and the
**complete** required legacy App/name set. Missing, extra or changed contexts block
managed check dispatch. Re-preview/review the definition if host setup changes
the effective rules fingerprint. TreeHarbor does not edit host protection here.

Read current configuration with:

```json
{"action":"admission_policy","epoch":1,"binding":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa","base_ref":"main"}
```

The result distinguishes `unconfigured`, `active` and `revoked` policy references.
These are policy states, not a complete repository or merge-readiness certificate.

## Pin queue and execution approval

After adoption, a new [queue request](github-queues.md) must include:

```json
"admission": {"digest":"sha256:<returned policy digest>","generation":1}
```

Its inspected required set and rules must match that policy. Omitting the pin,
using a foreign/stale pin, or dispatching a pre-adoption queue fails. The older
optional workflow `policy` pin remains a separate historical contract.

Managed PR approvals pin the current branch policy. Group approvals inherit the
queue's exact policy generation. Program/validator mismatches fail before execution.
Prepare, start and the first collection recheck the current pin and authorization,
including after receipt writes. Published target version2 and execution evidence
version3 retain the binding, policy generation and digest. Native group/PR identities
and exact source revisions remain distinct.

Policy changes use a new expected generation. Submit `definition:null` to revoke.
Revocation leaves a tombstone and fences new queue/check actions. Restoring identical
policy bytes creates another generation; it cannot revive an older approval.
Credential expiry limits new submissions; explicit credential/grant, profile or
policy revocation invalidates pending approval eligibility. Completed historical
receipts remain readable under current access and never grant another dispatch.

A stale execution can be released through the normal drain/stop/removal protocol.
Files remain protected until runtime removal and terminal execution are confirmed.

## Required context identity and recovery

Version1 targets retain `TreeHarbor/e<epoch>/<program hex>` and omit admission
context. An explicit null context normalizes to that same canonical omission.
Version2 requires `admission:{binding,generation,policy}` and uses:

```text
TreeHarbor/e<epoch>/a<64 hex characters>
```

The digest hashes domain `treeharbor-github-admission-context\0v1\0` followed by
compact UTF-8 JSON `[epoch,binding UUID string,base_ref,generation,program digest]`.
Even maximum epoch/generation names stay below100bytes. The policy digest is in
protected evidence but excluded from this name to avoid a circular hash through
host rules. Binding/base/generation/program/epoch changes produce distinct contexts.
An older passing check on the same SHA cannot satisfy a newly rotated context.

One unresolved publication per exact host context is allowed. An unknown old
context remains fenced, while a distinct new policy context may progress. No
context's missing remote result authorizes another POST. Old late receipts recover
through the existing [publisher journal](github-check-publications.md).

## Encoding and remaining gates

Policy version1 uses domain `treeharbor-connected-admission-policy\0v1\0` and
canonical fields `version,rules,managed,workflows`. Rule fields retain
`sha256,count,merge_queue` order. Programs sort by their immutable digest,
validators sort lexically, and workflows sort by required identity. Bounds and
independent vectors are included in the contract tests. Schema29 pins policy
history without modifying migrations1–28 or expanding any permission.

Policy-bound execution evidence uses `treeharbor-github-check-execution\0v3\0`,
version3 and the immutable `admission` context. It retains `queue` for groups or
`pull` for PRs. Legacy evidence versions1/2 keep their previous bytes and domains.

The gate proves current local policy eligibility and the supported preflight
requirements. Full effective host/review-policy coverage, workflow execution-context
proof, complete cross-check/final admission, unattended reconciliation and live
acceptance of an adopted policy remain release requirements. The product's
`host_policy_verified`, `admission_verified` and `merge_verified` flags remain false.

The separate [candidate evidence reader](github-admission-evidence.md) evaluates
the complete requirement set against retained candidate/execution/check receipts.
It distinguishes historical completeness from current trust and reports unproven
workflow context explicitly. Its check-set fact does not complete final admission.
