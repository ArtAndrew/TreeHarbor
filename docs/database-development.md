# Database development

The metadata integration tests use a real **disposable PostgreSQL 18** database.
They create isolated `th_test_*` schemas and drop those schemas after successful
tests. The crash test terminates only its own test database connection. Never
point this suite at a production database. Failed tests can leave test schemas.

```sh
export TH_TEST_DATABASE_URL='host=127.0.0.1 user=treeharbor_test dbname=treeharbor_test'
cargo test -p treeharbor-metadata -p treeharbor-materializer --locked -- --ignored
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
published checksum chain; the suite tests upgrading schema v1 and a populated v2 to v3.

Native ChangeSet intake commits exact versions, execution lineage, persistent
roots and replay receipts in one transaction. A subprocess test kills the worker
after acknowledgment and verifies the same result on retry. Logical workspace
states do not prove that files or an isolated runtime exist: trusted coordinator
callbacks must supply that evidence in the later runtime integration.

The native materializer suite builds actual private files from catalog-backed
snapshots and tests sparse expansion, preserved local edits/deletes, filesystem
name differences, symlink confinement and cancellation. It does not start a runtime
or update workspace READY state. See [ADR 013](adr/013-native-filesystem-materialization.md).

Native refs/admission, API authorization, node/runtime integration and
automatic GC/reconciliation are still unimplemented. Restore must stop/fence existing
workers before replacing metadata or objects. An outbox
worker must use the repository's current authority/epoch when executing its
effect, and must reconcile external effects after a crash. A valid delivery lease
alone is not permission to modify repository state or to publish to GitHub.
