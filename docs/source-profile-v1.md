# Source profile v1

A source profile selects the full tree or explicit paths/subtrees for delivery.
It is not an access-control boundary, glob language, build dependency inference
or proof that the selected paths exist at a revision. Missing sparse paths are
detected during filesystem preparation.

Definitions are immutable and registered per tenant/repository. The engine derives
the SHA-256 ID from canonical bytes. Reordering, duplicates and redundant descendant
paths normalize to the same selection. A new selection gets a new ID. The materializer
uses the same normalization through `Selection::from_profile`; automatic node and
runtime binding remains unfinished. Registration does not mark a workspace READY.

## Commands

Within the usual authenticated command envelope, write with `workspace.write`:

```json
{
  "method": "source_profile_put",
  "key": "select-src-v1",
  "authority": "standalone",
  "epoch": 1,
  "profile": {"kind": "sparse", "paths_hex": ["737263"]}
}
```

`737263` encodes raw path bytes `src`. Hex must be lowercase and even-length; paths
retain non-UTF-8 bytes. Full selection is `{"kind":"full"}` with no paths field.
An empty sparse list is invalid and never defaults to full selection. Unknown
fields and unsupported kinds are rejected.

Put requires current authority/epoch and an idempotency key. Its response contains
`profile_digest` and normalized `profile`. Read with
`{"method":"source_profile_get","profile_digest":"sha256:…"}` and `read`
permission for that repository. Knowing a digest does not grant cross-repository
access. Revocation/expiry also apply to reads and receipt replay. The ordinary
64 KiB JSON request limit applies; hex/envelope overhead can limit the paths fitting
in one request before the canonical-format limits below.

## Workspace binding and upgrading

New workspace creation requires a registered profile in the same repository. Its
transaction verifies the canonical bytes and digest and binds the definition with
a foreign key. Unknown profiles return 404 without a workspace or preparation
event. Status includes `profile_digest` and `profile_resolved`.

Stop old server/node processes before metadata migration 005 and restart with the
updated binary. Pre-005 workspaces carried only a declared digest. Migration retains
their pins, accepted changes, raw digest and receipts, leaving `profile_resolved:
false`; it does not guess their file selection. Those workspaces can be inspected,
drained and old successful receipts replayed. New READY/run observations and new
ChangeSet acceptance are refused. Register a profile and create a new workspace to
continue. Receipt replay does not resolve an old profile or prove files/runtime
exist. Migration preserves existing credentials and creates no profiles or grants
implicitly.

## Canonical bytes

All integers are unsigned big-endian. This is a separate profile format, not a new
source-object kind or a replacement for Git objects.

| Field | Encoding |
|---|---|
| Magic | 19 bytes: ASCII `treeharbor-profile` followed by NUL |
| Version | u16, value 1 |
| Selection | u8: 0 full, 1 sparse |
| Path count | u32: 0 for full, 1–1,024 for sparse |
| Each path | u32 length followed by raw repository-relative path bytes |

Paths follow the existing repository-path contract: no absolute, empty, dot,
parent, NUL or reserved metadata components. Sort by raw bytes, remove duplicates,
then remove paths covered by a selected ancestor. Component boundaries matter:
`src` covers `src/file` but not `src-other`.

Construction bounds the supplied list to 1,024 paths and 64 KiB of framed data
before normalization. Decoding accepts only canonical order/content, no trailing
bytes and at most 64 KiB. Full framing is 26 bytes. The ID is SHA-256 of the entire
frame, printed `sha256:<64 lowercase hex>`; Rust distinguishes ProfileId from ObjectId.

Independent full-selection golden vector:

```text
frame: 74726565686172626f722d70726f66696c650000010000000000
digest: f6d053964fd79fd002aae9f952200e9c60cb51dfc2a14eca69b931d3f94d7dae
```

Sparse paths `src` and bytes `ff 2e 72 73`:

```text
frame: 74726565686172626f722d70726f66696c6500000101000000020000000373726300000004ff2e7273
digest: 18ad403f8e4063898c0c049530b1da7501992e13dba015793984f4dd893e06d7
```

Tests cover independent Python struct/hashlib vectors, malformed encodings,
repository scoping, immutable definitions, replay/revocation, real schema upgrades
and CLI/HTTP round trips across restart. No performance claim follows from them.
