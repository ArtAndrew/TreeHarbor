# TreeHarbor implementation instructions

Read docs/specs/TREEHARBOR_ARCHITECTURE.md, TREEHARBOR_DELIVERY.md,
TREEHARBOR_RESEARCH.md and ADR-003, ADR-004, ADR-005 before changing contracts.
Track actual implemented behavior in docs/implementation-status.md.

The user has authorized implementation and publication on ArtAndrew's GitHub.
Copyright holder: Artishchev Andrey Borisovich, explicitly supplied by the user.
Both Connected and Standalone are required. Rust is fixed. Standard ELv2 has
no workspace quota, license meter or automatic open-source conversion.
Preserve third-party licenses; no automatic Apache exception for original code.

There is one authority per repository. Connected delegates published main to
GitHub. Standalone owns native history and must pass its core path with no Git
process or GitHub connectivity. Preserve the entire reachable published DAG.
Never treat an agent report as trusted validation, a stale check as current,
an acknowledged upload as an accepted ChangeSet, or a workspace as a sandbox.

Do not substitute an in-memory implementation, mock host or a smaller demo for
the durable PostgreSQL/object-store and real integration acceptance requirements.
Intermediate milestones are useful progress, not completion. Record unimplemented
components honestly. Do not invent performance numbers or customer adoption.

Tests must exercise failure boundaries and independent reference behavior.
Changes to storage/wire formats require a versioned spec and golden vectors.
Update implementation status with commands and actual outcomes after each stage.
Promotion outside this repository and direct messages require separate approval.
