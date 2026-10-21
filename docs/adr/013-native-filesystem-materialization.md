# ADR 013: Private native trees and staged sparse expansion

Date: 2026-10-21. Status: implemented internal materializer, not a deployed node
service or runtime binding. Both repository authority modes remain required.

## Decision

Add `treeharbor-materializer`, a Rust library using the real PostgreSQL catalog
and managed local object store. It builds an exact requested snapshot from a
repository pin to either that snapshot or a native commit pointing to it. Full
selection is the default integration policy; explicit sparse selections name files
or subtrees, include their ancestors and must exist in the source snapshot. The
coordinator still has to choose profiles compatible with the agent's tools and
required root configuration files. This selection is not a confidentiality ACL.

The materializer traverses the selected current tree, deduplicates object reads
within each frontier and uses bounded prefix batches from ADR 012. It does not
load commit parents or unselected blob contents. Source-byte limits apply before
opening a read batch. Entry/path budgets bound traversal state. File and workspace
byte limits are checked before filesystem writes. Decoder buffers are bounded by
the object/read contracts, not by a claim that these settings equal peak RSS.

Initial preparation uses ordinary private copies as the correctness baseline.
There are no writable hardlinks, Git metadata, inherited hooks or invoked Git
processes. Reflink, immutable cache trees, cache-key/profile identities, shared-build
deduplication and node scheduling are still separate work. Do not advertise this
library as a completed optimized cache, runtime, CLI or MCP server.

## Filesystem publication and isolation

One materializer instance per node owns a configurable semaphore. Each blocking IO
task owns its preparation permit and temporary directory, including if the caller
cancels its await. New work uses a private staging directory under the tenant's
service-owned directory. File writes use exclusive creation, preserve executable
semantics and create separate file identities. Output directories and ordinary
files are owner-only; executable files have owner execute permission.

The result consists of a `files/` directory and a sibling versioned baseline
manifest. Agents may receive only the working directory, never service metadata or
the managed object store. File/directory fsync and final validation finish before
the caller receives a `MaterializedTree` receipt. The node registry/runtime must
not infer READY from the presence of a staging directory. This library does not
update `th_workspaces`, bind an AgentRun or grant merge/validation authority.

Normal errors/cancellation remove unpublished staging. A returned result is not
deleted on handle drop, because a lifecycle controller must drain any associated
runtime first. Hard process loss can leave an orphan staging/result directory;
restart reconciliation, lease-fenced publication and cleanup are not implemented
here. The manifest documents the source baseline, not durable acceptance of local
agent edits or proof that the current files still equal the snapshot.

## Sparse expansion

`expand_stopped` is a trusted controller primitive. All writers to the old tree
must already be stopped/frozen by the external runtime. It neither performs nor
proves that stop and must not be exposed as an agent's direct call. A live mutable
directory is not a coherent snapshot; runtime enforcement remains a release gate.

Expansion requires the same tenant, repository and source snapshot. It copies the
stopped local tree to a new private directory, unions the old/new source selections,
then adds missing source paths. Previously selected leaf paths are preserved even
when the agent deleted them. Existing local files, including untracked collisions
with newly selected files, are preserved. Baseline coverage still records those
source paths for later explicit change detection. Untracked/build output is copied
locally; this is not permission to include it in a ChangeSet or shared source cache.

An obstructing file/symlink ancestor or incompatible filesystem name causes an
error; the old tree is untouched. Expansion can recreate an absent directory to
add newly selected children while keeping previously deleted tracked files absent.
The new candidate directory is not switched into a live runtime. The controller
must fence/increment the generation, rebind the stopped runtime and invalidate
observations for the old generation before continuing work. Seal/submit remains
separate and is not implemented by this library.

## Names and symlinks

The source model retains raw path bytes. Materialization uses those bytes exactly
where the target filesystem supports them; it fails instead of renaming or silently
normalizing incompatible names. Local macOS testing exercised rejection of invalid
UTF-8 names; Linux testing must exercise preservation. Case/Unicode aliases that
would overwrite another materialized entry are rejected.

Symlinks must be relative and confined to the workspace. Absolute, escaping,
cyclic/excessively indirect targets and special files are rejected. Link validation
expands links before processing `..` and consults the actual target filesystem,
including case aliases. Dangling internal targets are allowed. The builder never
follows a symlink while creating descendants. These rules protect materialization;
an agent can later modify its own files, so process/filesystem confinement still
requires a real external sandbox. Same-UID hostile writers to service directories
and a false runtime quiescence assertion are outside this primitive's guarantees.

## Evidence and remaining gates

Tests use actual PostgreSQL, managed objects and private output directories. They
cover concurrent independent trees from a native commit; bytes/executable bits and
symlinks; sparse reads with unavailable unselected data; preserving edits/deletes;
failed expansion without changing the old tree; limits and exact snapshot binding;
case aliases and unsupported raw filenames; symlink ancestors; and cancellation
while blocked on metadata. Unit tests cover the manifest's golden encoding and a
link-expansion escape/cycle corpus. The catalog has a separate byte-budget prefix
test. The CI database job includes this crate.

The API is internal; authentication, a runtime-proven stop, persistent node binding,
recovery/reaper, cache/reflink, restart manifest loading, full validation/sealing and
Git-compatible workspaces remain. No performance improvement or complete
Connected/Standalone path is claimed.

Implementation references: pinned Tokio 1.54.0 `task/blocking.rs` documents that
started blocking tasks cannot be aborted; pinned tempfile 3.27.0 `dir/mod.rs`
documents `TempDir::keep`. Their distributed source documentation was inspected
because version-specific documentation pages were unavailable through retrieval.
