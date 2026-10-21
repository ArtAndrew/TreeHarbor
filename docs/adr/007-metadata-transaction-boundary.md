# ADR 007: Durable PostgreSQL mutation boundary

Date: 2026-10-21. Status: accepted implementation refinement.

The first metadata layer serializes short authoritative mutations on the
repository row. It binds the idempotency digest to tenant, repository, principal,
method, key, authority, epoch and the complete canonical command. A repeated
committed request returns its original result, even after an authority change;
it does not execute an old operation in the new epoch. A different request with
the same key fails. New writes require matching active authority and epoch.

Caller SQL effects, the response and outbox rows commit in one transaction with
`synchronous_commit=on`; the service rejects a database with `fsync=off`.
Dropping the mutation rolls it back. An ambiguous connection failure at commit
is resolved by replaying the same key and command, never by generating a new key.
The service's authenticated application layer must provide the principal; this
library is not an authorization layer and is not exposed directly to agents.

Object upload, GitHub calls, CI and sandbox operations must stay outside this
short transaction. Their durable references and exact admission predicates
remain separate mandatory work. No completed operation here alone proves a
ChangeSet's bytes durable or its validation admissible.

Outbox claims use `FOR UPDATE SKIP LOCKED` only for work distribution. Reclaiming
an expired job increments its generation. Acknowledge/renew require matching
tenant, repository, worker, generation and a nonexpired lease. A crash after an
external side effect but before acknowledgement still requires idempotency or
reconciliation in that destination; the outbox does not promise exactly-once
external execution. Lease expiry does not prove a sandbox process has stopped.

The initial repository lock is intentionally conservative. Object upload is not
serialized by it. Benchmark its contention; narrow the lock scope only after
equivalent fencing and policy/read consistency tests pass. This is one
PostgreSQL schema, not a new distributed database. Tenant predicates and foreign
keys help prevent mistakes but are not a complete multi-tenant security boundary.
