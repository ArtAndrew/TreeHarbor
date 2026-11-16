# Batch source blobs over the local socket

The Rust importer can send consecutive file blobs in one request. It flushes
the batch before sending an object that depends on them. Large blobs,
directories, snapshots, deltas and commits use the existing single-object
endpoint. Only the local Unix socket exposes batch writes.

## Request v1

Use `PUT` with `Content-Type: application/octet-stream` and the same scoped
bearer credential used for single-object uploads.

| Path | Permission |
| --- | --- |
| `/v1/source/{tenant}/{repository}/uploads/{upload}/blobs` | `repository.write` for the upload owner |
| `/v1/github/{tenant}/{repository}/uploads/{upload}/blobs` | `host.read` for the assigned GitHub source bridge |

The body starts with the five bytes `54 48 53 42 01` followed by a big-endian
unsigned 16-bit frame count. Each entry is a big-endian unsigned 32-bit length
followed by that many bytes of one canonical TreeHarbor Blob frame. The server
computes each digest from the frame. Digests must be strictly increasing in
byte order. The request accepts 1–32 frames and at most 2 MiB including the
header and lengths. A truncated body, trailing bytes, bad frame or repeated
digest fails before reservation.

This request contains one empty blob. Spaces are for readability.

```text
54 48 53 42 01 00 01 00 00 00 16
74 72 65 65 68 61 72 62 6f 72 00 00 01 01
00 00 00 00 00 00 00 00
```

Success returns `{"version":1,"result":{"objects":[...],"state":"stored"}}`.
The array contains the exact `sha256:...` object IDs in request order.

## Durability and retries

The service reserves the batch against the existing upload budget in one
transaction. It writes each canonical frame to durable local storage and
then checks the current principal, repository epoch, session deadline and
object holds again. It marks all members complete in one transaction.
Ordinary source finalization still registers the complete revision separately.

After an ambiguous response, repeat the import with its saved journal.
Identical objects reuse their original reservations and deadline. A failure
after a storage write may leave reserved, incomplete objects until retry or
cleanup. They confer no membership on their own. The new CLI falls back to
single-frame requests on `404 not_found` from an older server. Other failures
follow the ordinary retry and error rules of the same import session.
