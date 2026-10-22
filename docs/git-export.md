# Native source export to Git

Experimental local command, not a completed Connected integration or server backup.
`git-export` projects the **entire reachable native commit DAG** in a completed
local source pack into an ordinary, self-contained Git bundle. A source pack
captured with `source-pack` currently contains one initial native commit; the
bridge library also supports packs/graphs with all parents of native merges.
There is no live-server export/download command yet. Do not copy a running
server's object directory as a substitute for a consistent, pinned export.

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

## Projection contract: `native-v1`

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
- Git import and preservation of original Git commit bytes/IDs are **not yet
  implemented**. This profile must not later replace raw imported commit metadata.
  Tags, Git submodules and LFS object transfer are not implemented. Ordinary LFS
  pointer file bytes are just bytes; their targets are not fetched.
- Reviews, validation receipts, native changes/deltas, runtime evidence, credentials,
  audit and server metadata are not part of this source-history archive. A source
  bundle does not assert that its commits were approved or checked.

A bounded Rust traversal validates IDs and all typed references, including delta
objects. One generated `git fast-import` stream is processed in a new private
bare repository, with explicit parents and length-delimited data. This avoids a
Git process per file. It currently enumerates full snapshot paths per commit;
large histories with mostly unchanged trees still incur that cost.

Git runs with an empty inherited environment, isolated system/global config,
an explicitly selected private Git directory, empty templates, disabled hooks,
credential helpers, network protocols and automatic maintenance. No worktree or
source configuration is loaded. The native path continues to run without Git;
this optional command is the boundary that requires it.

Before publication, the exporter checks the generated repository with strict
`git fsck`, verifies the bundle against an **empty** repository, unbundles it
there, and checks the restored graph from its expected tips. File data and both
manifest/bundle are synced before one no-replace directory rename and parent
sync. Existing results, including symlinks, are never replaced. If an IO error
occurs after the rename, inspect the existing result; do not assume it is absent.
A retry uses a new output directory, or verifies the already completed export.

## Manifest v1

UTF-8 JSON, fixed fields, unknown fields rejected by the Rust reader:

| Field | Meaning |
|---|---|
| `version` | `1` |
| `projection` | `native-v1` |
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
frames, 8 GiB generated stream/bundle and 2,000,000 expanded path visits across
all snapshots; Git stdout/stderr are bounded. `--timeout-seconds` defaults to 300
and may be 1–3600. The deadline covers bridge traversal and subprocesses, after
local pack opening/validation. Timeout kills the Git process group, including
its pack worker. Exceeding a bound fails without publishing a completed directory.

These are application limits, **not a sandbox or hard CPU/RAM/disk quota**.
Temporary stream, two Git repositories, bundle, manifest, source pack and cleanup
all consume resources. Use an externally bounded runtime/filesystem for exports
of untrusted content. A SIGKILL can leave `.treeharbor-export-work-*` or
`.treeharbor-export-ready-*` directories and descendants; stop/reap the job before
removing its private scratch. Normal failures clean scratch automatically. No
automatic abandoned-export cleanup or crash-resume journal is implemented.

No throughput or speedup claim has been measured for this exporter. The eventual
server adapter must hold a scoped read lease/pin for the **whole** source graph
until the export finishes, and separately export review/audit metadata for a
full product backup or authority migration.
