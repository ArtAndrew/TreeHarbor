# Database development

The metadata integration tests use a real **disposable PostgreSQL 18** database.
They create isolated `th_test_*` schemas and drop those schemas after successful
tests. The crash test terminates only its own test database connection. Never
point this suite at a production database. Failed tests can leave test schemas.

```sh
export TH_TEST_DATABASE_URL='host=127.0.0.1 user=treeharbor_test dbname=treeharbor_test'
cargo test -p treeharbor-metadata -p treeharbor-materializer -p treeharbor-node --locked -- --ignored
```

Use a private Unix socket or an isolated development database with disposable
credentials. This test harness uses `NoTls` for local fixtures; it is not a
production connector example. `Metadata::new` receives a preconfigured client
from trusted service code. Remote deployment must verify transport security.
`fsync` must be on; each mutation uses `synchronous_commit=on`.

Ordinary `cargo test --workspace --locked` intentionally ignores the database
tests. The separate **PostgreSQL contracts** CI job runs them explicitly and
must be required for merge. A missing database is a test failure there, not a
silent fallback to an in-memory store.

The database layer currently provides repository fencing, idempotent receipts,
leased outbox primitives, an object catalog, persistent root pins and manual
generation-safe retirement. Catalog tests include real files, a reference
reachability model and concurrent upload/collection. Migrations verify the
published checksum chain; tests upgrade historical populated schemas while
retaining pins, workspaces, receipts and credentials. Current schema 27 adds PR-head
sources to [managed Connected checks](github-managed-checks.md), retaining populated
group evidence and receipts. Schema 26 introduced group execution without
creating native refs or retroactive approvals. Schema 25 added
[owner workflow policies](github-workflow-policy.md); a populated schema24 upgrade
retains old queue receipts and grants without inventing an approval. Schema24 added
[GitHub event recovery](github-event-recovery.md), with a populated schema23
upgrade preserving the signed inbox and old permission sets. Schema 13 added
durable workspace capture requests and claims. Migration
005 adds immutable source profiles; historical unresolved profiles remain inspectable
and drainable. See [profile format and upgrade behavior](source-profile-v1.md). Migration 006 adds
[source upload sessions and backend identity](source-uploads.md).
Migration 007 adds [durable workspace preparation reservations](workspace-preparations.md)
without inventing physical bindings for historical workspaces.

Native ChangeSet intake commits exact versions, execution lineage, persistent
roots and replay receipts in one transaction. A subprocess test kills the worker
after acknowledgment and verifies the same result on retry. Logical workspace
states do not prove that files or an isolated runtime exist: trusted coordinator
callbacks must supply that evidence in the later runtime integration.

The native materializer suite builds actual private files from catalog-backed
snapshots and tests sparse expansion, preserved local edits/deletes, filesystem
name differences, symlink confinement and cancellation. It does not start a runtime
or update workspace READY state.

The [local node suite](local-node.md) separately binds those materialized files to
durable preparation ownership and acknowledges file readiness with no runtime.
It kills its own worker subprocesses at publication/acknowledgment boundaries and
tests current authorization, recovery, concurrent workspaces, cancellation and
real lease renewal. It does not execute untrusted tasks or prove sandbox readiness.

Access tests cover scoped credentials, transactional revocation/expiry, exact
receipt replay, owner checks and node/agent identity separation. Bootstrap admin
methods are trusted in-process operations, not public agent commands. The
[command library](agent-commands.md) is also used by the [local service](local-service.md).

The service process/socket tests require a **Unix** PostgreSQL connection explicitly:

```sh
export TH_SERVICE_TEST_DATABASE_URL='host=/var/run/postgresql user=treeharbor_test dbname=treeharbor_test'
cargo test -p treeharbor-service --locked -- --ignored
```

On machines running many test cases concurrently, add `--test-threads=4` after
`--ignored` to bound independent fixture setup. Each fixture runs migrations under
the database-wide migration lock; too many simultaneous initializations can exceed
the service's statement timeout. This does not reduce concurrent workers inside
the race tests or change production timeouts.

These tests launch the real binary, use private temporary Unix sockets, kill only
their own server processes and terminate only an identified connection from their
own pool. They cover bootstrap/credentials, unknown-response retry across process
loss, stale-socket recovery, revocation, HTTP bounds, request cancellation/rollback,
schema drift, pool reconnection and graceful shutdown. CI mounts the disposable
PostgreSQL service's socket directory into the runner; production TCP connections
are rejected by this development connector, not silently downgraded to plaintext.

[Native refs/admission](native-integration.md) now persist exact candidates, checks,
reviews and atomic publication. Remote transport, automatic CI/queue scheduling,
automatic workspace cleanup and GC/reconciliation remain unimplemented. Explicit
[workspace cleanup](workspace-cleanup.md) has a separate node durability protocol.
Restore must stop/fence existing
workers before replacing metadata or objects. An outbox
worker must use the repository's current authority/epoch when executing its
effect, and must reconcile external effects after a crash. A valid delivery lease
alone is not permission to modify repository state or to publish to GitHub.
