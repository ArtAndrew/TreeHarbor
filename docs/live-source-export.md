# Export source from a running server

Experimental Linux/macOS CLI and local HTTP endpoint. `source-export` downloads
one registered native revision and **all** of its reachable parents, snapshots,
changes and original Git metadata. The resulting local pack can be imported into
TreeHarbor again or converted to a self-contained Git bundle. No Git process is
used by the server or native download path.

This is source-history export, not a full server backup. Policies, reviews, check
receipts, credentials, audit records, unqueued proposals and unrelated refs are
not exported. Export does not change authority, move a ref or approve a merge.

## Download and restore

Build `cargo build -p treeharbor-service --locked`. Configure storage and import
a base as described in [source uploads](source-uploads.md), or publish a native
candidate through [native integration](native-integration.md). Use the returned
**native revision**, not a Git OID or an arbitrary object digest. Obtain current
authority/epoch with `repository-info`. The credential needs repository `read`
permission; no write or publication permission is needed.

```sh
mkdir -m 700 "$TH_PRIVATE/exports"
./target/debug/treeharbor client --socket "$TH_PRIVATE/service.sock" \
  --credential-file "$TH_PRIVATE/reader.json" \
  source-export --authority standalone --epoch 1 \
  --revision native:sha256:COMMIT_DIGEST \
  --output "$TH_PRIVATE/exports/pack-1"
./target/debug/treeharbor git-export \
  --pack "$TH_PRIVATE/exports/pack-1" --output "$TH_PRIVATE/exports/git-1"
git clone --branch main "$TH_PRIVATE/exports/git-1/history.bundle" restored
```

Replace `COMMIT_DIGEST` and the paths with real values. `TH_PRIVATE` must already
be an owner-private directory. Both output paths must be new. Only the optional
last two commands require Git. See [Git export](git-export.md) for projection
limits, SHA-256 Git repositories and native-to-Git identity mapping. To upload
the downloaded pack instead, use the existing `client source-import --pack …
--state … --authority … --epoch …` command with a writer credential.

Successful download prints JSON with `revision`, `snapshot`, `objects`,
`encoded_bytes` and `filesystem_entries`. The client verifies every canonical
frame and digest, typed edges, complete root closure and manifest before a synced
no-replace directory publication. Existing files, directories and symlinks are
never replaced. On an error after the final rename, a complete output may already
exist; inspect it before retrying. An interrupted download can be retried against
the same exact revision. Retry starts the transfer again; range resume is not
implemented. It never silently selects a newer ref.

## Access and retention

The request must match the repository's current authority, epoch and active state.
The root must be a registered native base in that repository, with its scoped
durable pin. Merely possessing an object ID or access to another repository in the
same tenant does not grant export access. Both imported bases and published native
integration commits qualify. Not every retained proposal is a registered base.

A 120-second read hold retains the complete source graph against GC during
archive construction. Reads use bounded, verified object batches. Authorization,
membership and authority fences are checked before each batch and again before
publishing an HTTP success. Normal completion and errors close the hold; a killed
or cancelled request leaves an expiring hold. The independent spool no longer
needs the source hold while its bytes are sent to the client. Revocation cannot
recall already authorized response bytes.

This endpoint uses the same owner-private Unix socket, peer/file checks, bearer
credential handling and Host/Origin rules as the [local service](local-service.md).
It is not a remote TLS transport. An archive proves content identity relative to
the requested root; it does not prove review, CI approval or publisher identity.

## HTTP and archive v1

`POST /v1/source/export`, `Content-Type: application/json`. Credentials belong in
the Authorization header, not this body:

```json
{
  "version": 1,
  "tenant": "example-team",
  "repository": "00000000-0000-0000-0000-000000000001",
  "authority": "standalone",
  "epoch": 1,
  "revision": "native:sha256:COMMIT_DIGEST"
}
```

The CLI fills tenant/repository from its scoped credential. Unknown fields,
noncanonical UUIDs/revisions and Git revisions are rejected. Success is HTTP 200
with `application/vnd.treeharbor.source-archive-v1`, `Cache-Control: no-store`
and a bounded binary stream. Errors before streaming use the usual JSON error
envelope. A failed response stream is incomplete, never a successful pack.

All integers below are unsigned big-endian. IDs are raw 32-byte native SHA-256
digests, without text prefixes. No compression, padding, footer or trailing bytes.

| Field | Encoding |
|---|---|
| Magic | ASCII `treeharbor-source-archive` followed by byte `00` |
| Archive version | `u16`, value `1` |
| Native revision | 32 bytes |
| Head snapshot | 32 bytes |
| Object count | `u32` |
| Total canonical frame bytes | `u64`; excludes record headers |
| Each object record | ID (32 bytes), frame length (`u64`), exact canonical frame |

Exactly `count` records follow, in unspecified order. Each object appears once.
The set must equal the full typed closure of the requested NativeCommit, whose
snapshot must match the header. Missing, unrelated, duplicate, mistyped or corrupt
objects are rejected, even when other counts and digests are valid.

The independent [archive vector](../crates/service/tests/fixtures/source-archive-v1.hex)
is hexadecimal wire bytes, generated by
[a Python reference](../scripts/generate-source-archive-vector.py) from separate
core vectors without calling Rust or Git. Its binary SHA-256 is
`dec86ed64d3568889e1df44f949047b67fe57bdc28c3ff6909ff832aec578c0e`.

## Local full-source pack v3

The downloaded directory contains a private native CAS and `manifest.json`:

| Field | Meaning |
|---|---|
| `version` | `3` |
| `selection` | `full` |
| `source_revision` | Canonical `native:sha256:…` matching `commit` |
| `commit`, `snapshot` | Canonical native `sha256:…` IDs |
| `objects` | Complete closure, once each, children before their parents |
| `encoded_bytes` | Exact sum of canonical frame lengths |
| `filesystem_entries` | Expanded entries in the head snapshot, including directories |

Unknown fields are rejected. The pack fingerprint hashes
`treeharbor-source-pack\0v3\0` followed by the exact UTF-8 manifest bytes, with
each `\0` representing one NUL byte. The independent
[manifest vector](../crates/service/tests/fixtures/source-pack-v3.json), without
its final newline, has fingerprint
`sha256:96b42016c776d0fd9eef8211534a377818324e60d1755a82ffe5fda827dfd1c2`.
Version 1 selected-file and version 2 Git-history packs keep their existing bytes
and semantics. New readers accept all three; older readers reject version 3 and
must be upgraded before consuming it. No PostgreSQL migration is needed.

## Bounds, costs and cleanup

The export profile permits at most 100,000 objects, 4 GiB of canonical frames and
2,000,000 typed edges. Client head-path validation additionally permits 1,000,000
expanded entries and 64 MiB of accumulated path bytes, subject to native path
bounds. These are operational limits, not license limits. Exceeding them fails
without publishing a complete pack.

Server reads at most 256 objects / 128 MiB of frames per batch, subject to the
existing object-size bound, and sends at most 64 KiB per response chunk. It holds
the existing source-read semaphore permit during spooling and delivery. The full
graph index and decoded batches still consume memory; bounded streaming does not
mean constant memory for the entire operation. The database connection is released
after construction, before response delivery.

Existing deadlines apply: server request default 15 seconds, configurable up to
60 seconds; connection lifetime is request timeout plus six seconds. Client
`--timeout-ms` defaults to 15,000 and can be 100–60,000. Its timeout bounds the
network exchange, not the subsequent local verification and fsync. Raising the
client timeout alone does not extend server deadlines. Large histories may exceed
these development limits; no large-repository throughput claim is made.

Budget for **server archive spool + client archive spool + client native CAS**,
graph validation and optional Git export work. Archive spools are anonymous local
temporary files, closed/unlinked on process exit, but they consume disk while open.
The server spool is derived data and is not fsynced as a backup. Client CAS and
manifest are synced before publication. Ordinary errors clean private staging;
SIGKILL during client verification can leave `.treeharbor-native-export-*` in the
output parent. Reap the process before removing abandoned staging. Automatic
cleanup and incremental export are not implemented. External CPU/RAM/disk quotas
remain necessary for untrusted input; these limits are not a runtime sandbox.
