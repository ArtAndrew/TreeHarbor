# Roadmap

Dates are not release guarantees. Each stage must pass its correctness gates
before performance claims or production readiness are published.

1. **Durable engine:** canonical objects and DAG queries; PostgreSQL catalog,
   upload reservations, pins, idempotent ChangeSets, outbox and recovery tests.
2. **Usable local pilot:** Rust CLI/API, node cache, sparse/full materialization,
   scoped credentials, one external runtime binding and two-agent conflict demo.
3. **Connected:** Git import/export, GitHub PR/checks/merge queue, exact candidates
   and real host tests. GitHub is authoritative for published main.
4. **Standalone S2:** native refs, review/validation policy, merge/rebase and queue;
   no Git process or host credentials on the native path; fenced migration.
5. **Release operations:** S3, GC race/fault campaigns, full-history backup/restore,
   export, clean installation/update/removal, artifacts, SBOM and provenance.
6. **Evidence and pilots:** tuned Git and available analogue benchmarks; preparation
   p50/p95, useful-action latency, bytes, disk, total cost, retries and merge time;
   observed onboarding with first pilot teams.

The first six-week Connected pilot is an intermediate milestone. Both modes are
required for the requested product release. See the [gate ledger](docs/implementation-status.md).

Performance targets are hypotheses: at least 2x better p95 preparation **or** 30%
lower full infrastructure cost than the best applicable tuned-Git baseline, with
time to useful work no worse. We will not claim these targets have been met
without reproducible data. Scion's internal deployment is not available as a
controlled comparison.
