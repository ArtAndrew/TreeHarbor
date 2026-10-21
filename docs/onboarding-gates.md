# Onboarding and operating gates

These are proposed acceptance targets, not measurements. Benchmark the whole
first-use journey; an installer alone is not differentiation.

| Journey | Target and test |
|---|---|
| Standalone evaluation | With documented prerequisites present, a new operator installs a verified artifact and completes the two-agent example in 5 minutes; record cold downloads separately |
| Connected evaluation | Import an authorized test repository and open the first proposed change within 15 minutes excluding GitHub organization approval wait; report that wait separately |
| Setup footprint | Rust binaries, PostgreSQL and local objects for the first single-host pilot; no required Kubernetes, Redis, Kafka or separate identity cluster |
| Diagnosis | `doctor` identifies missing storage access, incompatible Git features, runtime configuration and host permissions without printing credentials |
| Sparse fallback | A missing build input triggers an explicit full expansion that preserves the agent's edits; no manual recreation or inferred deletion |
| Upgrade | Document and test supported schema/data upgrade, interrupted upgrade and restore-based rollback; do not call an irreversible migration reversible |
| Exit | Export all supported reachable history and proposed changes plus audit manifest; inspect the Git export without TreeHarbor; revoke host credentials |
| Recovery | Operator restores metadata and reachable objects from documented backup, validates all digests and resumes from a fenced epoch |

Record a fresh operator's actions, time to first useful action, permission prompts,
errors, documentation lookups and cleanup. Test at least three operators unfamiliar
with the implementation. A successfully replayed developer script is necessary
but not sufficient evidence of onboarding quality.
