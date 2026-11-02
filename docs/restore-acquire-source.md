# Recover source missing from a backup

Development command after 0.1.11; not included in that published package.
`github restore-acquire-source` downloads one explicitly selected Git commit,
imports its complete reachable history, preserves an independent root pin and
asks the server to verify the exact Git/native mapping. It works while the old
GitHub binding remains revoked. It does not reconnect publishers or move main.

Use this after [restore assessment](restore-github-assessment.md) identifies source
that is missing from the restored database. Keep the old deployment fenced and
retain all assessment files. The source selection is a reviewed operator decision,
not a claim that a branch or PR still points to the same revision.

## Record the selection

Use a fresh human credential with `repository.write` and `read`, separate from
agent credentials. The existing assigned bridge can produce an assessment using
its `host.read` credential. The following Linux example uses `jq` and `sha256sum`
to select exactly one branch from that saved assessment. Check the repository,
branch, exact commit and budgets before submitting the resulting request.

```sh
umask 077
: "${TH_BRANCH:?Select a branch from the saved assessment}"
: "${TH_ACQUIRE_KEY:?Choose a new stable decision key}"
assessment=/absolute/private/treeharbor/restore-assessment.json
evidence_sha256=$(sha256sum "$assessment" | cut -d ' ' -f 1)
jq -e --arg name "$TH_BRANCH" --arg key "$TH_ACQUIRE_KEY" \
  --arg evidence "$evidence_sha256" '
  . as $a | [.inventory.heads[] | select(.name == $name)] |
  if length != 1 then error("select exactly one observed branch") else .[0] end |
  {action:"restore_decision_record",key:$key,epoch:$a.context.epoch,
   specification:{version:3,binding:$a.context.binding,
     manifest_sha256:$a.context.manifest_sha256,
     context_sha256:$a.context_sha256,evidence_sha256:$evidence,
     subject:{kind:"external_branch",name:$name},
     disposition:{action:"acquire_source",git_revision:("git:sha1:" + .target.oid),
       max_objects:100000,max_bytes:4294967296},
     previous_key:null,note:"Recover the reviewed exact source revision."}}
' "$assessment" > /absolute/private/treeharbor/acquire-source-001.json

# Review the new file, then submit it. Keep its bytes/key unchanged on retry.
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" \
  github --request /absolute/private/treeharbor/acquire-source-001.json
```

Use new filenames so redirection does not replace retained evidence. If the same
subject already has a decision for this manifest/binding, review it and set
`previous_key` to its latest key. A concurrent successor cannot silently overwrite
that chain. Other supported decision subjects can also name an exact acquisition;
selecting one does not authenticate external facts. Specification v3 is required
for `acquire_source`; v1/v2 keep their previous dispositions.

## Acquire and verify

Use a development binary with the new command, the original private App
configuration and a private, capacity-bounded directory:

```sh
mkdir -m 700 /absolute/private/treeharbor/source-recovery-001
treeharbor github restore-acquire-source \
  --socket "$TH_SOCKET" --credential-file "$TH_OWNER_CREDENTIAL" \
  --config /absolute/private/treeharbor/github-app.json \
  --epoch "$TH_EPOCH" --decision-key "$TH_ACQUIRE_KEY" \
  --state /absolute/private/treeharbor/source-recovery-001 \
  --git /usr/bin/git
```

The command uses the existing GitHub read-token download path, pinned to the
original App/installation/owner/repository. It requests only the selected commit,
never a newer branch tip, and attempts read-token revocation after download.
Git runs in a private bare repository with restricted configuration; the import
does not execute repository hooks, filters or a checkout. The owner-controlled
executable is used only for download/import. The native upload and server mapping
verification start no Git processes.

The server must still recognize the acquisition as the latest decision with its
original restore context. The CLI checks this before fetching, before uploading
and before recording preservation. A successor or context change stops work at
the next checkpoint. In-flight reads/uploads are not remotely cancelled by that
decision; source already accepted by the generic owner upload remains retained.
The preservation transaction rechecks context and predecessor atomically.

A successful result is the [source mapping receipt](restore-source-verification.md):
the full source graph is retained, `git_mapping_verified` is true, and
`host_state_verified`, `reconciliation_complete` and `publication_authorized`
remain false. A separate `preserve_source` successor holds an independent permanent
pin; the original acquisition decision remains unchanged. No old host import,
publication attempt, validation or branch is rewritten.

## Interrupted execution

Retry the same decision, state directory and epoch. The private directory holds
the acquisition identity, scope/decision digest, downloaded bundle, native pack,
pack fingerprint, existing upload journal and final mapping receipt. The command
first attempts server recovery of the deterministic preservation/mapping keys.
If preservation already committed, verification can finish without local source,
App configuration or Git. If mapping committed, its historical receipt is returned
without a new download or object verification. Current service authorization is
still required. The preserved decision chain is checked against the acquisition.

A retained complete pack may be resumed without current App access. It is private
operator input, **not a fresh host observation**; the server's projector independently
verifies its actual correspondence to the chosen Git commit. A changed pack
fingerprint is refused. If only upload completed, keep the pack/journal until the
preservation decision commits. The normal upload deadline is one hour; an expired
unfinished upload does not become a successful acquisition. Keep the source and
start an explicitly reviewed new acquisition when recovery cannot finish it.

Normal errors do not clear quarantine, release accepted bases or recovery pins,
or discard evidence. Do not edit journals or retry with changed source under the
same decision. Supersede an obsolete acquisition through the ordinary decision
API. Once it has a preservation successor, its old acquisition context is no
longer current; completing/replaying the existing mapping remains valid.

## Scope, limits and storage

Acquisition budgets are 1–100,000 native objects and 22 bytes–4 GiB of source,
bounded by the supported verifier profile. Git parsing also applies its object,
ancestry, edge, path-visit and timeout limits. Download and import each use the
existing 300-second Git operation bound; this is not a whole-command deadline.
The server's [verification limits](restore-source-verification.md#failure-and-resource-limits)
apply independently. Unsupported LFS, submodule or incomplete-history inputs fail
explicitly; unavailable deleted commits require another trusted complete source
copy, not substitution of another revision.

The state directory can contain compressed download, unpacked Git data and native
frames at the same time. Use an externally bounded filesystem/runtime; source
budgets are not hard disk/RAM quotas. A killed process can leave scratch files and
a read token until expiry. Inspect stopped processes before cleaning scratch.
Completed packs and independent source pins have no automatic cleanup here.

This recovers **one selected revision**, including all its reachable parents.
It does not discover every post-backup operation, authenticate an observed PR
marker, prove host coverage or fence external writers. Those remain necessary
before verified new-binding admission.

The new `restore_acquisition_context` API takes `epoch` and `decision_key` and
requires `repository.write`. Decision requests retain the existing durable
operation kind; the [v3 fixture](../crates/metadata/tests/fixtures/restore-acquire-v3.json)
fixes the new disposition encoding. Local operation keys are `restore-acquire-preserve:`
and `restore-acquire-map:` plus the version-1 restore digest of `[1, decision_key]`,
scoped to the repository. No database migration is introduced.
