# Roadmap

Dates are not release guarantees. Each stage must pass its correctness gates
before performance claims or production readiness are published.

1. **First installable release — Connected Preview:** one complete GitHub workflow,
   from two parallel agent workspaces through immutable proposals, exact candidate
   checks, failure/repair and protected integration. Package a reference deployment,
   verify a clean installation, restart/retry, backup/restore, export and cleanup.
   Publish the supported policy/runtime matrix and reproducible demo. This is a
   development preview for controlled pilots, not production support.
2. **Pilot hardening:** fix observed onboarding and operational problems; improve
   automatic reconciliation, monitoring and workload performance. Publish raw
   cold/warm benchmarks and infrastructure costs before making speed claims.
3. **Broader Connected deployments:** expand supported host policies, storage and
   remote-worker configurations, with recovery and upgrade tests for each.
4. **Standalone S2 release:** finish the native history/review/check/queue lifecycle
   and fenced migration, with no Git process or GitHub credentials on the native
   path. Reuse the shared Rust core and the same ELv2 license.
5. **Production readiness:** complete the relevant operational fault campaigns,
   backup/restore and capacity evidence, supported installation/update procedures,
   and acceptance with pilot teams for each advertised deployment.

Standalone is a later release milestone and does not block the first Connected
Preview. Its existing components remain experimental. The complete two-mode
product still requires the [full release gates](docs/implementation-status.md#release-gate-ledger).
Dates and performance targets are not promises.

The experimental MCP stdio adapter is implemented; full host/onboarding acceptance
remains open. VFS, structural merge and distributed SQL require experiments and
are not prerequisites for the first pilot.

Performance targets are hypotheses: at least 2x better p95 preparation **or** 30%
lower full infrastructure cost than the best applicable tuned-Git baseline, with
time to useful work no worse. We will not claim these targets have been met
without reproducible data. Scion's internal deployment is not available as a
controlled comparison.
