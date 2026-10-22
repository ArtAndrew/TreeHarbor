# Native snapshot merge

`treeharbor-merge` computes a three-way result from the immutable base snapshot,
current target snapshot and proposed snapshot. It is a Rust library development
increment. The [native integration service](native-integration.md) now constructs
durable candidates and exposes admission through CLI/API.

The engine uses native directories and blobs, with pinned `diffy` 0.5.2 for byte
text merging. It does not create a Git mirror, invoke Git, execute hooks or apply
repository-defined merge drivers. Its profile identity is
`treeharbor-native-merge-v2/diffy-0.5.2/exact-sibling-renames/subtree-delta`.
This profile uses [subtree operations v2](subtree-operations-v2.md) when deriving
candidate deltas for directory entry replacements. A clean one-sided file/directory
transition can now be represented and replayed as one exact operation. Conservative
three-way rename/type conflict handling is otherwise unchanged.
Changing the dependency version or behavior requires a new profile. Durable
candidate construction must bind that identity before checks are dispatched.

## Computation contract

`merge_snapshots(store, base, current, proposed, limits)` returns either:

- `Clean(MergedSnapshot)`: exact input and result snapshot identities, plus profile.
- `Conflicted(Vec<Conflict>)`: byte-preserving paths, related paths, conflict kinds
  and the original leaf identities. No partially merged snapshot is returned.
- An error: unsupported case, exhausted budget, corrupt/missing object or IO error.

`merge_revisions` first computes **all** best common ancestors of the native tips.
Unrelated histories and multiple bases are explicit errors. It does not arbitrarily
choose one ancestor or infer ancestry from timestamps. The explicit snapshot API
also provides the tree computation needed to rebase an immutable proposal from
its original base onto a current target. `prepare_candidate` creates a verified
integration delta plus proposal/integration commits; a general history-rebase API
is still unfinished.

Inputs must be service-controlled, verified source graphs protected from GC.
Fetched objects are checked against their identities. Unchanged blobs are reused
without reading their content. Whole-tree equality reuses an existing snapshot.
A nontrivial merge currently scans directory metadata for all three trees and
rebuilds directory objects, whose unchanged digests remain shared. It is **not**
an incremental directory traversal or a measured performance improvement.

Conflict detection and budget checks complete before any store writes. Clean
output objects are written children before parents. An IO failure during writing
can leave unreachable objects; it cannot update a ref. The caller still needs
upload reservations, durable publication, pins and one final metadata transaction.
This computation result grants neither validation nor merge authorization.

## Supported profile and conservative boundaries

| Case | Behavior |
|---|---|
| One unchanged side, identical sides | Reuse the exact chosen tree |
| Independent paths, additions and deletions | Three-way merge of exact entries |
| Modify/delete, add/add content conflict | Structured conflict |
| Two regular-file edits | Byte text merge; conflict markers are never published |
| Executable-bit change plus opposite text change | Merge mode and content separately |
| Symlink changes, file/symlink disagreement | Select unchanged-side result, otherwise conflict; never follow a link |
| Binary data with NUL | Select unchanged-side result, otherwise binary conflict |
| Exact unique rename within one directory | Follow opposite edit, retain file mode |
| Rename/delete, different rename destinations, rename/add collision | Structured conflict |
| File/directory collision | Structured conflict, including non-adjacent sorted prefixes |
| Non-UTF-8 paths and text, CRLF, no final newline | Preserve bytes |
| Ambiguous equal-content renames | Explicit unsupported error |
| Cross-directory rename | Explicit unsupported error; directory-move propagation is unfinished |
| Unmatched deletion and addition on the same side | Explicit unsupported error; may be an edited rename |
| `.gitattributes` in any nontrivial input tree | Explicit unsupported error; drivers/filters are not silently ignored |
| Explicit empty child directory | Explicit unsupported error; do not silently discard native entries |
| Criss-cross history with multiple best bases | Explicit unsupported error until virtual-base synthesis is implemented |

The conservative rename rules can reject an unrelated delete/add that Git merges
cleanly. Symlink conflicts and destination collisions can also be more conservative
than Git. The fixture matrix is not a claim of complete Git merge equivalence.
The library is a textual/tree merge engine, not an AST or semantic-conflict detector.

## Resource boundaries

Default per-operation bounds: 10,000 history commits; 20,000 directory reads;
200,000 visited entries; 32 MiB cumulative path bytes; 128 MiB encoded reads and
128 MiB staged writes. History reads share the byte budget with tree reads.
Each nontrivial text merge permits at most 1 MiB combined inputs and 12,000 combined
lines. The sum of squared combined line counts is capped at 32 million across the
operation to bound pathological diff work. These are operational bounds, not
license restrictions or benchmark results.

Bounds are caller-selected. Production integration still requires a bounded
worker pool, cancellation/deadline handling and publication reservations. A large
blob can be fetched before its content is classified; the existing object-format
maximum bounds that individual allocation. Do not run expensive merges on an
unbounded async request executor.

## Verification

```sh
cargo test -p treeharbor-merge --locked
```

The test suite uses Git as an independent reference **only in tests**. It creates
temporary repositories without user configuration/hooks, invokes `merge-tree
--write-tree`, then compares full paths, modes and blob bytes, not native and Git
object IDs. It covers 20 tree scenarios in both directions and 16 text scenarios
including CRLF and missing final newlines. A separate native test removes Git
from `PATH`, writes to the real local object store, reopens it and verifies bytes.
Other tests exercise resource exhaustion, corrupt/missing inputs, rename ambiguity,
all-parent history, conflict staging and unchanged-blob reuse.

Upstream interfaces: [diffy byte merge](https://docs.rs/diffy/0.5.2/diffy/fn.merge_bytes.html)
and [Git merge-tree](https://git-scm.com/docs/git-merge-tree). Checked on 2026-10-21.
The dependency licenses are recorded in [third-party notices](../THIRD_PARTY_NOTICES.md).

Still required for Standalone: native candidate commits/refs, durable queue, trusted
check collection, exact review policy, atomic fenced admission, broader merge
coverage and full-path recovery. This library does not complete Standalone S2.
