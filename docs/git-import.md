# Import a pinned Git history

Experimental local interop on Linux/macOS. `git-import` converts one exact Git
commit and **all its reachable parents and trees** from a self-contained bundle
into a durable native source pack. It does not fetch a remote, move repository
authority, create a native branch or grant review/validation. Neither complete
product mode is released.

## Capture, upload, use

Build `cargo build -p treeharbor-service --locked`. Create a bundle with ordinary
Git from your existing repository. Select the exact commit before bundling; keep
that commit reachable until the bundle has been created. For a stable example:

```sh
mkdir -m 700 "$PWD/import-demo"
git -C "$SOURCE_REPO" bundle create "$PWD/import-demo/source.bundle" --all
git -C "$SOURCE_REPO" rev-parse --verify 'refs/heads/main^{commit}'
# Copy the full commit ID from that repository; it must exist in the bundle.
treeharbor git-import --bundle "$PWD/import-demo/source.bundle" \
  --commit "git:sha1:$FULL_COMMIT_ID" --output "$PWD/import-demo/pack"
```

If the branch advanced beyond the bundle, use a captured commit that exists in it
or create a new bundle. The importer never silently substitutes another revision.
`--all` can include other refs; **only the selected commit's reachable history**
enters the native manifest. Tags and unrelated branch tips are not migrated.
Use `git:sha256:...` for a SHA-256 repository and a Git supporting that format.
GitHub support is a separate integration; local SHA-256 interop makes no hosting
compatibility claim. `--git` accepts an absolute operator-selected executable
(default `/usr/bin/git`), never an executable chosen by an untrusted agent.

The output must be new and its existing parent directory owner-only (`0700`).
The command prints JSON with native `revision`, `snapshot`, object count,
encoded bytes and head-tree filesystem entry count. A completed pack contains
the entire selected graph, so the original bundle/repository is no longer needed
for upload, native work or re-export.

Provision the service and repository-scoped ingestion credential using the
[local service](local-service.md) and [source upload](source-uploads.md) guides:

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_CREDENTIAL" \
  source-import --pack "$PWD/import-demo/pack" \
  --state "$PWD/import-demo/upload.json" --authority standalone --epoch 1
```

Use the actual authority/epoch and a credential with `repository.write`. Completion
registers a pinned native base, not a main ref. Use its exact native revision with
`workspace-create`, a registered full/sparse profile and [node preparation](local-node.md).
Upload, native file preparation and their recovery work without Git. The source
pack's full selection is independent of the later workspace's sparse profile.

To take this local pack back to Git:

```sh
treeharbor git-export --pack "$PWD/import-demo/pack" \
  --output "$PWD/import-demo/export"
git clone --branch main "$PWD/import-demo/export/history.bundle" import-demo/restored
```

For SHA-256 input, pass `--object-format sha256` to export. Original commits keep
their Git IDs; new native descendants have deterministic new projections.
Exporting live server state still needs a future pinned export/download adapter.

## Fidelity and unsupported inputs

The bridge preserves ordered parents (including merge parents and independent
roots), exact file bytes/modes/names, symlink payloads, empty root trees and original
commit bytes. Each native delta is computed against its first parent's snapshot
(an empty tree for roots) and replayed to verify the exact result. All other
parents are retained. Raw origin bytes include separate committer, signatures
and extension headers; signature validity is not checked. Commit metadata does
not authenticate an author, approval or permission.

Only complete ancestry is accepted: incremental bundles requiring preexisting
objects fail verification against an empty repository. Missing tips, damaged
objects and unsupported formats fail explicitly. Gitlinks/submodules, recognized
LFS pointers, nested empty directories, non-UTF-8 author/message fields and
unsupported timestamp/encoding/path forms are rejected, rather than normalized.
Native core bounds on names, paths, entries, parents and object sizes also apply.
LFS hydration, submodule resolution, custom filters and checkout hooks are not run.
Symlinks are bytes during import; unsafe links or OS-specific names can still be
rejected later by the materializer. Successful import is not a promise that every
tree can be checked out on every filesystem.

## Isolation, limits and recovery

The regular input bundle is opened without following its final symlink and copied
to private staging. Git sees a new private bare repository and empty configuration,
with hooks, credentials, replacement objects, network protocols and automatic
maintenance disabled. It verifies the bundle, unbundles, checks the object graph,
enumerates reachable IDs and reads one `cat-file --batch` stream into a bounded
temporary file. It never checks out files or starts Git for every source file.
Source repository configuration is not loaded. The input ancestors, executable,
private directory and operator identity are trusted.

Defaults are 100,000 objects, 4 GiB selected expanded Git payload and separately
4 GiB native frame writes, 8 GiB compressed input, 2,000,000 edges and 2,000,000
expanded path visits across snapshots. `--max-objects` and `--max-bytes` configure
the object/native/selected-payload budgets; each object payload is at most 64 MiB.
Intermediate native objects count toward the budget even if absent from the final
manifest. `--timeout-seconds` defaults to 300 (range 1–3600). Git output and stderr
are bounded and timeout kills the private process group. Full path validation and
delta verification repeat for each commit; large histories incur this cost.

These checks **are not hard CPU/RAM/disk isolation**. Git must unpack the bundle
before selected-graph limits can be checked, including objects outside the chosen
history. Run hostile or large imports in an externally bounded runtime/filesystem.
Budget the copied bundle, unpacked Git repository, batch tempfile, native frames,
validation and cleanup, not just the resulting pack. No performance claim is made.

Staging frames and manifest are synced and validated before a no-replace directory
rename and parent sync. Concurrent writers have at most one winner; an existing
output or symlink is never replaced. A normal failure cleans staging. Process
death can leave private `.treeharbor-git-pack-*` scratch; stop/reap its processes
before cleanup. An IO failure after rename can mean a completed result exists:
inspect it rather than assuming nothing was published. Restart local capture into
a new path if necessary; no capture-resume journal exists.

Server upload uses the existing [immutable retry journal](native-import.md).
Repeat the exact pack/state invocation after connection loss; completed receipts
survive service restart and session expiry, subject to current authorization.
No partial local capture grants a server base or updates a ref. There is no
automatic cleanup of completed local packs or abandoned imports yet.

## Full-source pack manifest v2

The native object formats are unchanged. The private JSON manifest has exactly:

| Field | Meaning |
|---|---|
| `version` | Integer `2` |
| `selection` | Literal `full`: the selected commit and its complete history |
| `source_revision` | Canonical `git:sha1:...` or `git:sha256:...` matching the root's origin claim |
| `commit`, `snapshot` | Canonical native `sha256:...` IDs |
| `objects` | Unique complete reachable closure, child before parent, commit last |
| `encoded_bytes` | Sum of canonical frame lengths of those objects |
| `filesystem_entries` | Selected head's entries, including directories, excluding root |

The reader rejects unknown fields/versions, non-full selection, invalid revisions,
duplicate/missing/unrelated objects, wrong edge types and inconsistent totals.
It verifies frame identities and the root's origin **claim**, not its Git semantic
association or signature; actual import/export performs that verification. Neither
the manifest nor its fingerprint is an authorization or CI attestation.

Fingerprint = SHA-256 of `treeharbor-source-pack\0v2\0` followed by exact manifest
bytes. Keep those bytes unchanged once an upload starts. The upload journal stays
v1, binding this fingerprint; v1 selected-file pack IDs and fingerprints stay
unchanged. Older pack readers must be upgraded before v2 use.

The [independent vector](../crates/service/tests/fixtures/source-pack-v2.json),
generated by [Python framing/hash calculations](../scripts/generate-git-import-vector.py)
from the [core vectors](../crates/core/tests/fixtures/git-origin-v2.txt), has an empty
root and one origin-bearing commit. Excluding the fixture's terminal newline,
its fingerprint is `sha256:f4b6ed668e538f38a2e1b854eba7223495fbe0884f8e201670d77236b0c7cb14`.
