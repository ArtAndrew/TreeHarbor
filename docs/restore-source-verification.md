# Verify preserved recovery source against Git

Development command after 0.1.11; not part of the published 0.1.11 package.
Use this after a [source-preservation decision](restore-decisions.md). It verifies
on the server that the exact native commit held by that decision projects to the
expected Git commit, then saves a durable mapping receipt.

To download and preserve missing source as one recoverable workflow, use
[source acquisition](restore-acquire-source.md). The commands below remain useful
for source already accepted through another trusted import path.

This closes the gap between retaining source and knowing which Git revision it
represents. A commit's `git_origin` field alone is insufficient: the verifier
reconstructs trees, preserves parent order, checks every reachable typed object,
and checks original Git commit bytes against their claimed hashes and native
metadata. Native commits without Git origins use the existing export projection.
The same Rust projector supports SHA-1 and SHA-256; the command starts no Git
process and makes no GitHub request. Git signatures are retained as bytes, not
cryptographically validated.

## Prepare and verify

1. Keep old external publishers fenced. Retain the successful assessment and
   inspect the exact Git OID of the source being recovered.
2. Accept the full source history through the existing
   [Git import](git-import.md) and [source upload](local-service.md) workflow.
   A trusted, complete Git bundle can be imported without activating the old
   GitHub binding. Downloading that bundle is a separate authenticated operator
   action; this command does not fetch missing post-backup source automatically.
3. Record `preserve_source` with the accepted native commit and existing source
   pin. For a registered native base, the holder is `base:<64 hex digest>`, while
   the revision is `native:sha256:<64 hex digest>`. Keep the decision key.
4. Run the verification using a fresh human `repository.write` credential:

```sh
: "${TH_DECISION_KEY:?Set the committed preserve_source decision key}"
: "${TH_EXPECTED_GIT:?Set git:sha1:<40 hex> or git:sha256:<64 hex>}"
cargo run -p treeharbor-service --locked -- client \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-owner.json \
  --timeout-ms 60000 \
  restore-verify-source --key restore-map-001 --epoch 1 \
  --decision-key "$TH_DECISION_KEY" --git-revision "$TH_EXPECTED_GIT"
```

Use the repository's actual current epoch. Keep the same key, epoch, decision key
and Git revision when retrying after a lost response. Use a new key for a different
request. An identical retry returns the previously committed receipt without
recomputing or claiming that its context is still current. A client timeout does
not establish whether recording completed.

The server acquires a read lease for the decision's exact permanent pin. It reads
and checks the complete graph, computes the Git projection in private temporary
storage, then rechecks the decision context, root generation, current repository
state and authorization before committing. An agent cannot supply a success flag,
proof, source path, executable, arbitrary root or alternate tenant locator.

## Read the result

A successful version-1 receipt includes:

- `decision_key` and `decision_sha256`, binding the result to the retained decision;
- exact `native_revision`, `git_revision`, `source_holder` and `generation`;
- verified `source_objects` and encoded `source_bytes` counts;
- `verifier: "rust-git-projection-v1"` and `git_mapping_verified: true`;
- `host_state_verified`, `signatures_verified`, `reconciliation_complete` and
  `publication_authorized`, all **false**.

The original decision receipt stays unchanged, including its original
`git_mapping_verified: false` field. Read the separate mapping receipt through a
private GitHub-command JSON file using a credential with `read`:

```json
{"action":"restore_mapping_status","key":"restore-map-001"}
```

```sh
cargo run -p treeharbor-service --locked -- client \
  --socket /srv/treeharbor-recovery/stage-001/active/service.sock \
  --credential-file /absolute/private/treeharbor/recovery-owner.json \
  github --request /absolute/private/treeharbor/restore-map-status.json
```

The HTTP endpoint is `POST /v1/github/restore/verify-source`, with the standard
repository-scoped bearer credential and this
[version-1 request fixture](../crates/service/tests/fixtures/restore-source-request-v1.json).
Unknown fields are rejected. It takes the existing decision key and expected Git
revision; the native root is selected from server-owned recovery state.

## Failure and resource limits

| Result | Meaning |
| --- | --- |
| `source_mapping_mismatch` (409) | The graph cannot produce the expected Git OID, origin metadata is inconsistent, or the selected projection is unsupported. No success receipt is written. |
| `restore_context_changed` (409) | Retained restore state changed after the decision. Reassess and record a reviewed successor decision before a new verification. |
| `repository_fenced` / `forbidden` | Current authority, state or access no longer permits verification. |
| `source_mapping_limit` / source-export limit (413) | The bounded verifier cannot process this graph. This is not a partial success. |
| `source_read_busy` (503) | The shared source-read concurrency budget is occupied. Retry the same request. |
| `source_mapping_objects` / storage failure | The complete native spool could not be verified/read/written. Preserve the source pin and investigate storage. |

The service uses the existing source-export limits: at most 100,000 native objects,
4 GiB encoded source and 2,000,000 graph edges. Git projection additionally limits
generated bytes to 8 GiB and path visits to 2,000,000, with a 60-second projection
deadline. This is not a 60-second deadline for the entire request: source reading
and spool creation precede projection. The read lease lasts 120 seconds. The two
shared read slots include other source reads/exports. Use a bounded storage volume
and external process/resource limits; archive, unpacked objects and temporary Git
pack can coexist and require more disk than source bytes alone.

Temporary spools are removed after normal success/failure. A process or machine
crash can leave named scratch directories requiring operator cleanup while the
service is stopped; no automatic crash-time cleanup is claimed. The permanent
recovery source pin remains, including after failed verification.

This receipt proves content correspondence. It does **not** prove that GitHub
currently points to this revision, validate PR markers/checks/queue membership,
find all post-backup source, or fence old external writers. Those facts must still
be reconciled before new-binding admission. No binding, old dispatch attempt,
publication receipt or validation result is rewritten by this command.
