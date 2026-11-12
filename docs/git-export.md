# Native source export to Git

Experimental local command, not a completed Connected integration or server backup.
`git-export` projects the **entire reachable native commit DAG** of each selected
branch from completed local source packs into one self-contained Git bundle.
The bridge supports [imported Git history packs](git-import.md), graphs with
all parents of native merges, and single-commit packs from `source-pack`.
Use [live source export](live-source-export.md) to download registered native
revisions from the running server, including newly published merge commits.
Do not copy a running server's object directory as a substitute for a pinned export.

## Try it

Build `cargo build -p treeharbor-service --locked`. On Linux or macOS, with Git
installed at `/usr/bin/git` (or provide an absolute `--git` path):

```sh
mkdir -m 700 export-demo
mkdir export-demo/source
printf 'hello\n' > export-demo/source/hello.txt
./target/debug/treeharbor source-pack \
  --directory "$PWD/export-demo/source" --output "$PWD/export-demo/pack" \
  --path hello.txt --author-name 'Example' --author-email 'example@example.invalid'
./target/debug/treeharbor git-export \
  --pack "$PWD/export-demo/pack" --output "$PWD/export-demo/export"
git clone --branch main "$PWD/export-demo/export/history.bundle" export-demo/restored
```

The output directory must be **new**, under an existing owner-only directory
(mode `0700`). The command prints its manifest as JSON. `--branch NAME` chooses
the exported branch; `--object-format sha256` selects Git SHA-256 instead of the
SHA-1 default. A SHA-256 bundle requires a Git version supporting that format;
this does not imply a hosting service accepts it. The installed Git executable
is trusted operator configuration and must never be selected by an agent.

The result contains exactly `history.bundle` and `manifest.json`. Git can read the
bundle without TreeHarbor. Preserve the manifest to map native revisions to Git
commits. Keep the source pack until export is verified; export does not delete it,
change server refs, move authority or grant permission to merge.

## Export all branches from a live Standalone repository

The development CLI can capture and export the complete branch set in one
command. Use the local Unix socket, a repository credential with `read`
permission and the epoch reported by `repository-info`.

```sh
mkdir -m 700 -p "$TH_PRIVATE/exports"
treeharbor client --socket "$TH_PRIVATE/service.sock" \
  --credential-file "$TH_PRIVATE/reader.json" \
  git-export --epoch 1 --output "$TH_PRIVATE/exports/repository"
git clone --bare "$TH_PRIVATE/exports/repository/history.bundle" restored.git
git -C restored.git for-each-ref --format='%(refname) %(objectname)' refs/heads
```

The command captures all tips at one instant, downloads one verified pack per
distinct tip and compares each pack's tree with the captured tree. It prints the
captured `snapshot` and the Git `export` manifest as JSON. The same two files
described above are published together under a new owner-private directory.
The service supplies native source, and the operator's local Git verifies the
bundle. The standard per-pack, aggregate input and projection limits apply.
Shared ancestry can be transferred in more than one pack.

The requested epoch must match the capture. An empty or oversized branch set,
failed pack read, mismatched tree or failed Git verification leaves the final
destination unpublished. The client applies `--timeout-ms` to individual
requests and `--timeout-seconds` across capture, pack transfers and projection.
A process killed during export can leave a private `.treeharbor-branch-packs-*`
directory under the output parent. After confirming that the export has
stopped, the operator can remove that temporary directory. For an authority
transfer, use the frozen handoff below.

## Freeze Standalone writes and export a handoff bundle

Use this development workflow to prepare a reverse transfer to GitHub. Finish
or cancel active integration attempts, drain remote tasks, and remove the
workspaces. Keep the destination fenced against writers while preparing and
verifying the handoff. A credential with `repository.write` can freeze the
native repository. Store an owner-private request file with these fields and
substitute the current epoch from `repository-info`.

```json
{"action":"handoff_freeze","key":"freeze-for-github-1","epoch":2}
```

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  native --request "$TH_PRIVATE/freeze.json"
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_READER" \
  git-export --epoch 2 --freeze-key freeze-for-github-1 \
  --output "$TH_PRIVATE/github-handoff"
```

The freeze and its receipt commit together. The receipt captures the exact
branch heads, snapshots, generations and policies. The repository stays frozen
after service restart. An exact command retry returns the same receipt. Read
the current freeze using `handoff_snapshot` with `epoch` and `freeze_key`.

The frozen export uses a scoped `read` credential. It downloads only revisions
in the active freeze and verifies the projected Git history. It checks the
freeze again before publishing `history.bundle`, `manifest.json` and
`handoff.json` in one private directory. `handoff.json` version 1 includes the
freeze receipt, the SHA-256 of the exact Git projection manifest and the bundle
SHA-256. These files provide the branch history for a later GitHub handoff.
Proposal and queue records remain in the service. Publishing to a target and
transferring its write authority require subsequent steps.

To abandon the handoff while the target remains fenced, send a new owner
request with `handoff_resume`, a new `key`, the frozen `epoch`, `freeze_key`
and `target_stays_fenced: true`. Resume advances the native epoch and preserves
branch heads, history and accepted proposals. Queued entries from the old
epoch become stale and can be submitted again. Keep GitHub fenced when using
this command. The acknowledgment records the operator's assertion about the
target, so use it only while the target is still read-only.

The [freeze receipt v1](../crates/metadata/tests/fixtures/native-handoff-freeze-v1.json)
uses SHA-256 of `treeharbor-native-handoff\0v1\0` followed by the compact JSON
encoding of its `snapshot` in field order. Refs are sorted by branch-name bytes.
The [source request v1](../crates/service/tests/fixtures/native-handoff-source-v1.json)
is `POST /v1/native/handoff/export-source` on the private socket. It uses the
ordinary complete-source export stream and binds reads to the freeze key,
scope, epoch and one exact native revision.

### Verify the frozen GitHub projection on the server

Use the owner credential to record the SHA-1 mapping from the frozen export.
The CLI reads the digest-bound `handoff.json` and `manifest.json`.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  handoff-verify --key verify-for-github-1 --epoch 2 \
  --handoff "$TH_PRIVATE/github-handoff"
```

The server reads the complete pinned native graph and reconstructs all Git
commit IDs in Rust. Every branch in the current freeze must have the exact
projected tip. The verified receipt records the freeze digest, Git refs,
object count and frame bytes. Use the same operation key for an uncertain
response. The receipt is historical after the native epoch advances. A fresh
verification must use the current freeze.

The [v1 projection request](../crates/service/tests/fixtures/native-handoff-projection-v1.json)
uses `POST /v1/native/handoff/verify-projection` over the private socket and
`repository.write`. The receipt proves the source mapping. Target publication
uses separate verification of the bundle bytes and remote branch state.

### Return write authority to GitHub

Keep Standalone frozen. Prepare a GitHub repository with the review and CI rules
you want to use after the switch. Seed the exact projected branch set with an
ordinary Git restore and a reviewed, fast-forward-only push under exclusive
operator control. Reconcile unexpected or diverged branches before continuing.
Archive the destination once its branches and default branch match. Keep it
archived through activation so GitHub branches and PRs are read-only.
[GitHub archive behavior](https://docs.github.com/en/repositories/archiving-a-github-repository/archiving-repositories)

Submit the plan and activation with `repository.write`.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  github --request "$TH_PRIVATE/handoff-plan.json"
```

The [v1 plan request](../crates/metadata/tests/fixtures/github-handoff-target-v1.json)
uses the `handoff_plan` action, a new key, the frozen epoch and `target`.
Set `target.projection_key` to the verified projection key. Assign a fresh
`target.binding` UUID and the destination's pinned App, installation, owner,
repository and bridge identities in `target.specification`. Include the GitHub
repository node ID and default branch in `target.repository_node_id` and
`target.default_branch`.

The assigned bridge needs `host.read` and its private App configuration.
With an existing private state directory, run the observer while the plan is
current. It reads two matching inventories from the archived GitHub repository,
revokes the temporary read token and records the exact Git branch set. The
bridge saves the inventory so a lost response can be retried without reading
GitHub again.

```sh
treeharbor github handoff-observe --socket "$TH_SOCKET" \
  --credential-file "$TH_BRIDGE" --epoch 2 --request-key return-to-github-1 \
  --config "$TH_APP_CONFIG" --state "$TH_PRIVATE/handoff-observation"
```

Submit this request with the owner credential while the observation is fresh
and GitHub is still archived. The 180-second plan starts after export and
destination preparation. If it expires, create a new plan and observe again.

```json
{"action":"handoff_activate","key":"activate-github-1","epoch":2,"plan_key":"return-to-github-1","keep_target_archived":true}
```

The transaction installs a fresh Connected binding, advances the epoch and
fences native writers. Native history, ChangeSets and validation audit remain
available for recovery. Old native queue entries become stale. The new binding
must go through the usual [connection and permission checks](github-connection.md)
before publication. Use the [Connected projection flow](github-projection.md)
for selected retained ChangeSet versions. GitHub evaluates their new PRs under
its own CI and review rules.

For an uncertain response, retry the exact activation and confirm the current
Connected epoch and binding before unarchiving. Treat the replayed receipt as a
record of the original operation. To abandon the migration before activation,
keep the destination fenced and use the native resume flow above.

## Export several branches

Capture the desired tips with [`ref_snapshot`](native-integration.md#capture-the-current-branch-tips)
or resolve an individual branch with `ref_resolve`. Download a
[source pack](live-source-export.md) for each exact `head`. For a
final migration capture, stop branch writers before resolving the tips and keep
the service available for reads. Authority transfer is a separate operation.
Then pass one `--ref BRANCH=PACK` per branch. Branches at the same tip can use the
same pack. The source packs and output parent are owner-private directories.

```sh
treeharbor git-export --ref "main=$TH_PRIVATE/exports/main" \
  --ref "release/stable=$TH_PRIVATE/exports/stable" \
  --output "$TH_PRIVATE/exports/repository"
git clone --bare "$TH_PRIVATE/exports/repository/history.bundle" restored.git
git -C restored.git for-each-ref --format='%(refname) %(objectname)' refs/heads
```

The development CLI accepts up to 128 distinct branch names. The manifest maps
each selected `refs/heads/...` name to its native and Git tip. Each input must
contain its own complete history. The command validates all distinct input packs
within the timeout and an aggregate budget of 1,000,000 objects and 64 GiB of
frames. The projection counts shared objects once and uses the existing
projection budgets. `--pack` and `--branch` are the single-branch form.

## Projection contracts

- Every reachable parent, in its original order, is included. Independent roots
  remain independent; there are no shallow prerequisites or object filters.
- Regular bytes, executable flags, symlink payloads and byte-valued names are
  preserved without checkout. Export does not dereference symlinks. An ordinary
  Git checkout is subject to its OS/filesystem and Git's own path protections.
- Author, timestamp/offset and UTF-8 message are preserved. Native v1 has no
  separate committer, so the Git committer is the author. No timestamp is taken
  from the exporting machine. Root commits may have an empty tree.
- Negative timestamps, NUL in messages, leading/trailing whitespace or ASCII
  controls in author names, whitespace/controls in email, and nested empty
  directories are rejected by this profile. Git verification can reject further
  incompatible inputs; nothing is silently sanitized or omitted.
- Native and Git IDs differ. Distinct native commits that differ only in their
  change-operation identity can have the same Git projection. This is a mapping,
  not a promise of a reversible native backup. A projected graph rejected by
  strict Git verification is not published.
- A commit with [Git origin metadata](git-origin-v2.md) is exported from its exact
  original bytes only after its hash, tree, ordered parents and displayed metadata
  match the native graph. Separate committer, signature and extension header bytes
  survive. Signature bytes are not authenticated by this command. Hash-format
  conversion of an origin-bearing graph is explicitly rejected.
- [Pinned Git bundle import](git-import.md) preserves origins; ordinary `source-pack`
  still creates native v1 commits. Tags, Git submodules and LFS object transfer are not implemented. Ordinary LFS
  pointer file bytes are just bytes; their targets are not fetched.
- Reviews, validation receipts, native changes/deltas, runtime evidence, credentials,
  audit and server metadata are not part of this source-history archive. A source
  bundle does not assert that its commits were approved or checked.

A bounded Rust traversal validates IDs and all typed references, including delta
objects and raw origin Blobs. Rust writes one non-delta Git pack with canonical
blob/tree/commit payloads; system Git imports it with `index-pack --strict` into
a private bare repository. Shared objects are encoded once, with collision-checked
SHA-1 or SHA-256 identities and zlib compression. This preserves arbitrary original
commit headers without a Git process per object. Full snapshot path compatibility
checks still run per commit; large histories incur that cost. Previous `native-v1`
commit IDs remain unchanged. No throughput improvement is claimed.

Git runs with an empty inherited environment, isolated system/global config,
an explicitly selected private Git directory, empty templates, disabled hooks,
credential helpers, network protocols and automatic maintenance. No worktree or
source configuration is loaded. The native path continues to run without Git;
this optional command is the boundary that requires it.

Before publication, the exporter checks the generated repository with strict
`git fsck`, verifies the bundle against an **empty** repository, unbundles it
there, and compares every restored branch name and tip with the manifest. It
checks the restored graph from those tips. File data and both
manifest/bundle are synced before one no-replace directory rename and parent
sync. Existing results, including symlinks, are never replaced. If an IO error
occurs after the rename, inspect the existing result; do not assume it is absent.
A retry uses a new output directory, or verifies the already completed export.

## Manifest v1

UTF-8 JSON, fixed fields, unknown fields rejected by the Rust reader:

| Field | Meaning |
|---|---|
| `version` | `1` |
| `projection` | `native-v1`, or `native-with-git-origin-v1` when the graph contains verified origins |
| `git_object_format` | `sha1` or `sha256` |
| `bundle_sha256` | 64 lowercase hex digits hashing the complete bundle bytes |
| `bundle_bytes` | Actual bundle size |
| `refs` | Full `refs/heads/...` names to `{native, git}` |
| `commits` | Every reachable native `sha256:...` ID to lowercase Git OID |
| `source_objects`, `source_bytes` | Distinct verified native frames, including delta references |

Maps are serialized in sorted key order. Bundle byte reproducibility across Git
versions/compression implementations is not promised; projected commit identity
is deterministic for the profile. Manifest hashes are integrity aids, not signatures
or independently trusted authorization. The golden empty-root Git SHA-1 vector
is `9ddc57252779b944f82af2992c8e9c9458922c1a` (literal input in the bridge tests,
independently computed with Python `hashlib`); restored trees and DAGs are also
compared with real Git plumbing in Linux/macOS CI.

## Bounds and operations

CLI defaults: 100,000 unique source objects, 2,000,000 typed edges, 4 GiB native
frames, 8 GiB generated object payload budget (including 32 bytes per write),
8 GiB temporary pack/bundle and 2,000,000 expanded path visits across
all snapshots; Git stdout/stderr are bounded. `--timeout-seconds` defaults to 300
and may be 1–3600. The deadline covers local pack validation, bridge traversal
and subprocesses. Timeout kills the Git process group, including
its pack worker. Exceeding a bound fails without publishing a completed directory.

These are application limits, **not a sandbox or hard CPU/RAM/disk quota**.
Temporary pack, two Git repositories, bundle, manifest, source packs and cleanup
all consume resources. Use an externally bounded runtime/filesystem for exports
of untrusted content. A SIGKILL can leave `.treeharbor-export-work-*` or
`.treeharbor-export-ready-*` directories and descendants; stop/reap the job before
removing its private scratch. Normal failures clean scratch automatically. No
automatic abandoned-export cleanup or crash-resume journal is implemented.

No throughput or speedup claim has been measured for this exporter.
[The source download adapter](live-source-export.md) holds a scoped read lease
for the whole graph while building its independent archive. Review/audit metadata,
full product backup and authority migration remain separate release work.
