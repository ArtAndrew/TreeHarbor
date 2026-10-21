# ADR 010: Durable ChangeSet intake and execution lineage

Date: 2026-10-21. Status: implemented internal native intake; not a released API,
workspace runtime, Git bridge or merge-admission system.

## Computation and durability are separate

`prepare_change` reads an exact Snapshot from a service-controlled verified store,
checks every operation's expected-before value, applies the operations and writes
canonical operation/result objects. Its `AppliedChange` has private fields: a
caller cannot submit a claimed result tree through this type. It is a computation
receipt, not evidence that bytes are durable or a grant of repository access.
The source store must uphold `ObjectStore`'s verified immutable-read contract.

The coordinator uploads the resulting graph through the catalog protocol, then
supplies current object holds to `accept_change`. Before acknowledging, one short
PostgreSQL transaction checks current repository authority/epoch, run ownership,
workspace generation and state, exact base, object identities/types and holds.
It creates the immutable version, base/operation/result graph pins, idempotent
response and outbox event together. Object locks are taken in digest order.
The bytes are durable before the transaction; the response follows its commit.

Retries bind the complete service-built versioned command to the authenticated
principal and repository. Physical upload tokens are not logical command identity.
An identical retry returns the original committed receipt even if a subsequent
drain or authority migration has occurred. This is the operation's historical
result, not a claim that the workspace is still running. A changed request using
the same key fails. New versions compare the previous version before incrementing;
simultaneous writers cannot replace the same version. Acceptance means retained
proposal data only; it does not mean validated, approved, admitted or merged.

## Base and execution identity

The first base registry accepts only native commits with already published object
graphs. Commit content binds its exact snapshot. It pins the commit and therefore
every reachable parent, snapshot and operation object. Base registration does not
create a branch or move main. Verified Git import bindings are later bridge work.

A workspace record pins a registered base and starts REQUESTED. A coordinator can
record READY only with the current preparation outbox lease, matching payload,
epoch and workspace generation. It must have actually observed materialization
and the runtime binding before making that call. The current library does not
materialize files, start a runtime, attest a runtime or authenticate an agent.
These callbacks belong to a trusted control-plane role, never an agent write API.

A running AgentRun records the initiating principal, task, agent identity, runtime
and workspace generation. Intake checks those bindings and the workspace's original
base. Two agents can retain different proposals that edit the same path; conflict
resolution and candidate validation belong to integration, not intake.

Drain atomically increments the workspace generation, blocks new intake, records
STOPPING for its active run and emits a drain request. It does not delete files or
release pins. TTL expiry alone never proves that writers stopped. Runtime shutdown,
unmount confirmation, quarantine/recovery and final deletion remain separate work.

## Schema and retention

Migration 003 adds immutable base bindings, workspace/run metadata and versioned
ChangeSets. Existing schema 001/002 and canonical object format v1 are unchanged.
Foreign keys prevent accidental release of roots still referenced by base,
workspace or version records. Database triggers reject in-place updates to base
bindings and accepted versions. A future explicit retention transaction may remove
eligible records and then pins; no implicit expiry or pruning is implemented here.

Service-owned logical identities cannot be reused by calling create again with a
different operation key: uniqueness fails without replacing data. Resource limits,
authorization, migration of active execution contexts and retention APIs are still
required before exposing these internal commands to remote clients.

## Evidence and remaining boundaries

Tests use actual files and PostgreSQL. They cover overlapping proposals from two
registered agent executions, stale ownership/base/epoch/generation/holds, immutable
versions, concurrent version CAS, eight identical concurrent submissions, drain
versus intake, stale/tampered preparation jobs and upgrade from a populated v2
catalog. A subprocess acknowledges an actual ChangeSet and is then killed; a new
request recovers the identical version with one outbox event. The execution records
in these fixtures are trusted test inputs, not evidence of runtime isolation.

The test upload helper walks a small fixture graph. It is not the production
batching, incremental read or materialization path and is not a performance claim.
Remaining work includes the authenticated coordinator/API/CLI, streamed object
reads and uploads, filesystem workspaces and sandbox integration, validation/review
collection, native and GitHub integration, S3, comprehensive failure injection and
backup/restore. Neither complete product mode is delivered by this ADR.
