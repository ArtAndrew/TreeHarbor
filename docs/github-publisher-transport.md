# GitHub branch transport

Development library for the trusted Connected bridge. It converts a retained
native commit into a verified Git artifact and sends it to one generated service
branch. The trusted [`github publish` command](github-publications.md#publisher-command)
connects it to durable owner intent and one-shot dispatch. PRs, checks, merge queues
and installed-App acceptance remain pending. Do not treat an exported pack as
authorization to publish.

## Exact branch updates

`treeharbor_git_bridge::publish::prepare` creates a process-local `Prepared`
artifact from a native object graph, trusted Git executable, private staging
directory, non-nil UUID and resource limits. It derives the Git commit ID and a
SHA-256 bundle digest. The caller must hold source retention during preparation.
The type cannot be deserialized from a caller-supplied manifest.

The only destination is `refs/heads/treeharbor/<32 lowercase UUID hex digits>`.
`Connection::publish_branch` takes a pinned repository binding, the dedicated
worker executable, that artifact and an explicit expected old Git commit. `None`
requires the branch to be absent. Git uses one refspec and an explicit
`--force-with-lease=<ref>:<expected>`; it never infers expected state from remote
tracking refs. This permits an authorized non-fast-forward replacement of a
service branch while rejecting a stale writer. It never writes main, tags or an
arbitrary caller-selected ref and does not bypass host branch protections.

The service driver uses `publish_branch_guarded`, rechecking its current attempt
before minting access and again immediately before push. This does not make a
local cancellation atomic with the remote operation or issue a second dispatch.

Before network IO, the worker copies and hashes the private bundle, loads it into
a fresh bare repository and checks it with Git. Changed bytes fail before a push.
The adapter verifies installation, repository and owner IDs, current token scope
and lifetime, and refuses archived or disabled repositories. It requests exactly
one repository with `contents:write` and `metadata:read`. Source downloads and
`observe_branch` continue to request read-only access. Workflow changes needing
additional GitHub permission can be rejected; no permission is silently widened.

## Observations and uncertain outcomes

The adapter reads the exact service ref before sending. If the desired commit is
already there, it returns `already_present` without pushing. An unexpected old
commit produces `RefChanged`. Otherwise it attempts one push and independently
reads the ref again, including after worker failure:

- `pushed`: the worker succeeded and the exact target commit was observed;
- `observed_after_uncertain_push`: the worker did not report success, but the
  exact target commit was observed;
- `PublicationUnknown`: the final ref could not be verified at the target.

These are observations, not merge approval or proof that this particular request
caused the host update. A timed-out push can already have taken effect. Absence
does not prove that an in-flight request cannot still finish. No error authorizes
an automatic resend. `observe_branch` performs read-only recovery; durable
orchestration uses the [publication ledger](github-publications.md) to retain the
intended branch, old/new commits and authorization before external effects, and
record/reconcile the observed result separately. Its CLI journal retains mappings,
attempt identity and unacknowledged reports; restarting never blindly resends.

The temporary App token is revoked after an observed attempt. A successful ref
observation is preserved with `token_revoked: false` if revocation fails: credential
cleanup failure does not undo a branch update. The bridge must report that condition
and account for the remaining token lifetime using `token_expires_at` (RFC 3339).
Cancellation, process death or a
lost token-creation response can leave a token alive until GitHub expires it.
No token is returned in branch results or written to the artifact.

## Execution boundary and current limits

The network worker currently runs on Linux and supports GitHub.com SHA-1
repositories. Preparation can run on the supported Unix development platforms.
App keys and write tokens belong only to the trusted bridge, never an agent.
Tokens travel to Git through private process environment, not argv, a URL,
repository configuration or metadata. A trusted bridge host is required: this
does not protect secrets from its administrator or other unrestricted processes
under the same identity.

Git receives a cleared environment, disabled hooks, credentials helpers, automatic
maintenance and submodule recursion. HTTPS, certificate verification and a fixed
GitHub host are required; redirects and proxies are disabled. The supervisor bounds
time, CPU, address space, file size and descriptors, and stops its process group
on parent loss. Aggregate disk is monitored with possible overshoot. Hard volume
quotas, host isolation and deployment security belong to the external runtime.

Publication limits are operational: the adapter accepts at most a five-minute
worker budget and holds a concurrency permit until the blocking worker finishes.
The current path exports and unbundles complete history; there is no incremental
upload cache or performance claim. Crashes can leave staging directories for later
cleanup. No agent code, workspace hooks or checks execute in this transport.

Local HTTP fixtures test scoped tokens, identity checks, stale heads and uncertain
observations. Linux service tests exercise real Git smart HTTP, competing writers,
an applied push whose response is dropped, redirect refusal, changed artifacts,
deadlines and parent-loss cleanup. These fixtures use synthetic credentials and
do not establish real installed-App or GitHub merge-queue readiness.
