# Native workspace baseline manifest v1

Status: internal on-disk format written and loaded by `treeharbor-materializer`.
There is no externally accepted manifest-upload API. This file is not an
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

## Loading an observed baseline

`MaterializedTree::load_baseline` takes an operator-controlled result directory,
an expected tenant/repository, snapshot, immutable source profile and exact manifest
SHA-256 from trusted durable state. Computing a new expected hash from the same
unverified input would defeat this binding. The API reads only the baseline and
checks the type of the `files/` mount root; it does not traverse its contents.

The loader requires an owner-private directory and regular manifest owned by the
node's effective OS user, with one hard link. It opens the directory and manifest
without following their final symlinks, opens the manifest relative to the directory
descriptor and rejects a symlink substituted for `files/`. Ancestor directories
must be controlled by the operator. It detects changes to the observed manifest
metadata or directory identity during reading. These checks do not replace a
controller's lifecycle lock or protect against a hostile process with the same OS
identity. The baseline must remain outside the agent's writable mount.

The parser streams entries into the baseline map, with no full JSON tree or second
entry array. It rejects unknown/duplicate/missing fields, unsupported versions,
invalid or unordered byte paths, absent/non-directory ancestors, invalid IDs/modes,
noncanonical selections, unselected entries and absent explicitly selected paths.
It accepts equivalent JSON field order/whitespace only when their exact bytes match
the expected observation. Source identity itself is unchanged by JSON formatting.
This v1 baseline contains no workspace, generation, node, slot or preparation-attempt
identity. Two workspaces from the same source can have identical manifests. The
controller must verify their separate durable placement binding before adopting a
directory; a matching baseline alone cannot identify the correct workspace.

Read budgets cap input at 256 MiB, entries at the lower of the caller's limit and
1,000,000, and decoded path bytes at the lower of its limit and 64 MiB. Source-profile
limits remain 1024 paths and 64 KiB in the canonical profile encoding. Lexical guards
bound individual JSON strings to 64 KiB and non-string scalar tokens to 128 bytes
before deserialization can allocate large values; the normal JSON recursion limit
also applies. Exceeding a bound is an error, never truncated successful input.

Loaded trees have `statistics() == None`: historical preparation counters are not
persisted in v1, and zero would be a fabricated measurement. Freshly prepared trees
return `Some(Statistics)`. Neither value reports current disk usage.

Recovery never restores original file bytes, removes untracked files or rejects
tracked deletions. This remains true when `preserved_local_state` is false: an agent
may have edited an originally fresh tree. A loaded baseline can feed the existing
`expand_stopped` operation only after the controller independently proves that all
writers are stopped. Expansion creates a new candidate and preserves the old tree.

Tests cover the independent fixed v1 bytes/digest, malformed and oversized inputs,
scope/profile/hash mismatch, private-file/link checks, loading in a new process and
reopening an actual catalog-backed sparse materialization before expanding it with
edits/deletions/untracked files intact. These prove the loader boundary, not full
node crash recovery. Durable node/slot identity, local attempt publication/recovery,
current database fencing, runtime reconciliation and safe cleanup remain required
before a recovered tree is admitted for execution or a source pin is released.
