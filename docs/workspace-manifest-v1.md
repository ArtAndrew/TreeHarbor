# Native workspace baseline manifest v1

Status: internal on-disk format written by `treeharbor-materializer`. No restart
loader or externally accepted manifest-upload API exists yet. This file is not an
authorization token or proof of the current working directory's content.

A result directory has `files/` plus `manifest.json`, outside the agent's writable
mount. JSON is UTF-8. Top-level fields are `header` and `entries`:

```json
{
  "header": {
    "version": 1,
    "materializer": "copy-unix-v1",
    "tenant": "tenant",
    "repository": "00000000-0000-0000-0000-000000000001",
    "snapshot": "0303030303030303030303030303030303030303030303030303030303030303",
    "selection": null,
    "preserved_local_state": false
  },
  "entries": [["61", 33188, "0101010101010101010101010101010101010101010101010101010101010101"]]
}
```

The example identities are illustrative. `snapshot` is the exact native snapshot
digest, separate from any commit identity. `selection: null` means full; otherwise
it is a sorted array of lowercase hexadecimal repository-relative path byte strings.
Selections include subtrees and required ancestors; globs are not encoded. Current
limits allow at most 1024 selection paths, normalized to remove duplicates and
descendants of an already selected subtree.

Each entry is `[path_hex, mode, object_id_hex]`, in bytewise path order with no
duplicates. Paths use the core RepoPath validation. Modes are decimal encodings of
040000, 100644, 100755 or 120000 octal. Directory entries refer to original source
directory objects even when only a subset of their children is selected. The root
directory is implicit. Blob IDs refer to source contents/symlink targets, not edits.

`preserved_local_state: true` means this result was expanded from a stopped local
tree. Its local bytes can differ from the baseline; tracked deletions remain entries
even though the path is absent. Untracked local paths can exist without a manifest
entry. Consequently this file must not be used as a successful seal, current-tree
checksum, validation receipt or shared immutable-cache manifest.

The writer streams entries, writes a final newline and fsyncs before returning a
result. The golden test `manifest_v1_has_stable_explicit_byte_names_and_baseline_identity`
asserts a fixed serialization independently of the traversal/fixture uploader.
JSON field ordering/whitespace do not change the source snapshot or profile identity.
A [preparation observation](workspace-preparations.md) hashes the exact durable
manifest bytes, so re-encoding the file changes that observation's digest.
A future loader must reject
unknown versions/materializer semantics, malformed/duplicate paths or fields,
oversized metadata, invalid IDs/modes and mismatched tenant/repository bindings;
it must authenticate service-owned file provenance and reestablish database/runtime
state before reuse. Such loading is not currently implemented.
