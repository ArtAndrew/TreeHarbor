# GitHub App connection and repository bindings

Development commands for an operator preparing a Connected installation. The
standalone `github probe` verifies the App installation, selected repository access
and stable numeric identities. A server binding can retain that configuration and
observations from its assigned bridge. Neither path imports source, publishes a
branch/PR, inspects branch rules or enqueues a change. The released
[Connected PR Preview](connected-pr-preview.md) combines these commands with exact
source import and branch/PR delivery; the full Connected mode remains incomplete.

## Prepare an App

For alpha.1 PR delivery, use the [PR Preview permission table](connected-pr-preview.md#4-use-pr-permissions-not-queue-permissions):
Contents-write, Pull requests-write and Metadata-read on the selected repository.
No webhook or queue setup is needed. The read-only probe recipe below intentionally
has fewer permissions; it cannot publish branches/PRs.

For later experimental queue/managed-check acceptance, the
[queue acceptance kit](../examples/github-queue/README.md) provides a synthetic
repository, exact conflicting edits, a `merge_group` workflow and a reviewable
App permission template. The following minimal read-only setup is for the probe
only; it cannot publish branches/PRs or operate a queue.

Register a GitHub App on GitHub.com, grant repository **Contents: read-only**
(Metadata read is implicit), and install it on the selected repository. Webhooks
are not consumed by this increment and may remain disabled for this probe. Do not
grant administration, organization permissions or write permissions for this step.
Generate a private RSA key in the App settings. Keep it outside the repository,
source workspaces, agents and CI jobs that execute untrusted code.

Follow GitHub's official [App registration](https://docs.github.com/en/apps/creating-github-apps/registering-a-github-app/registering-a-github-app)
and [installation authentication](https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/authenticating-as-a-github-app-installation)
instructions. Use the **App ID**, **Client ID**, and **installation ID** from their
respective settings pages; they are distinct identifiers. Pin the repository ID
and owner account ID from the repository API. Repository names alone are not
sufficient identity. If using GitHub CLI to inspect those public identifiers:

```sh
gh api repos/OWNER/REPOSITORY --jq '{repository_id:.id,owner_id:.owner.id,owner:.owner.login,name:.name}'
```

TreeHarbor itself never reads the GitHub CLI configuration, a PAT, GH_TOKEN,
GITHUB_TOKEN, or Git credential helpers for this command.

## Configuration and command

Put the JSON configuration and downloaded RSA key in a private operator-owned
directory (0700); each file must be 0600, regular, single-linked and owned by the
current user. Symlinked files and group/world-readable files are rejected. The key
path must be absolute. These are examples, not usable credentials or IDs:

```json
{
  "version": 1,
  "app_id": 123456,
  "client_id": "Iv1_example",
  "private_key_file": "/absolute/private/treeharbor/github-app.pem",
  "repository": {
    "installation_id": 234567,
    "repository_id": 345678,
    "owner_id": 456789,
    "owner": "example-org",
    "name": "example-repository"
  }
}
```

```sh
cargo run -p treeharbor-service --locked -- github probe \
  --config /absolute/private/treeharbor/github-app.json
```

Success returns JSON with the observed repository node ID, default branch name,
privacy/archive/disabled flags, `read_connection_verified: true` and
`probe_token_revoked: true`. This verifies repository metadata/access, not a source
checkout. `publication_ready` and `queue_configuration_verified` remain **false**.
The complete [version 1 output fixture](../crates/integrations-github/tests/fixtures/probe-v1.json)
defines the current field names/types with synthetic IDs and time.
An archived or disabled repository may still be readable; that is not a grant to
publish. Errors produce a nonzero exit without exposing keys, tokens or API bodies.

Queue eligibility is explicitly distinct from queue configuration:

- `personal_repository_unsupported`: personal repository; GitHub's merge queue is
  unavailable under its documented ownership requirements.
- `public_organization_eligible`: the ownership/visibility requirement is met;
  branch rules, CI and queue configuration still need verification.
- `private_organization_plan_required`: an organization repository; the required
  Enterprise Cloud plan is not verified by this command.

GitHub documents queue availability for public organization repositories and
private organization repositories on Enterprise Cloud. Required workflows must
handle `merge_group` and check the group's candidate. This probe does not test
those requirements. [GitHub merge queue documentation](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
(checked 2026-10-23).

## Credential and transport behavior

The Rust bridge signs a short-lived RS256 JWT, checks the installation's App/account
identity and suspension, then requests one read-only installation token with an
explicit numeric `repository_ids` selection. It checks returned permissions and
expiry, independently enumerates the token's accessible repositories, and compares
the observed repository and owner IDs with the operator's pins. Renames/transfers
are not silently adopted. Both names and numeric pins require operator review.

After probing, the token is revoked, including a best-effort revocation after a
scope/identity failure. A failed revocation does not produce success. A lost token
creation response, malformed response, process death or cancellation may leave an
unobserved token until GitHub expires it (normally one hour). No token is saved to
disk or printed for manual reuse. Repeating the probe may mint another token; it
does not retry a failed network operation automatically. Key revocation/removal of
the App installation remains the operator's emergency control.

Only `https://api.github.com` is supported, using certificate/hostname validation
and the platform trust store. API version **2026-03-10** is pinned. Redirects,
environment proxies, implicit retries, cookies and decompression are disabled.
Each response is bounded to 128 KiB; a request has connection/read/total deadlines,
and the full probe has a 45-second deadline including waiting for its four-request
concurrency limit. HTTP errors retain only stage/status and numeric Retry-After,
not upstream bodies. Configuring GHES, a custom API URL, a proxy or skipping TLS
validation is not supported. The bridge needs direct network access to GitHub.

Keys and owned token buffers are redacted and wiped on drop where supported;
cryptographic/TLS libraries and the OS may retain transient copies. This is not a
claim of protection against a compromised operator process, memory dump or host.
Agents never receive the App key or upstream token.

## Retain a connection in a running TreeHarbor server

First migrate the [local service](local-service.md) to the current schema and create a
repository with `--authority connected`. Provision a Human principal with
`repository.write` and `read` for that repository. Separately provision a Bridge
principal with **`host.read`** and issue its scoped credential. Existing tokens and
grants do not acquire this permission on migration. `host.publish` is a separate
permission and is unnecessary for connection verification. Agents cannot receive
either bridge permission. Keep both TreeHarbor credentials private as well.

Write a private request file using your actual IDs and a fresh non-nil UUID:

```json
{
  "action": "bind",
  "key": "connect-repository-1",
  "epoch": 1,
  "binding": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
  "specification": {
    "version": 1,
    "app_id": 123456,
    "client_id": "Iv1_example",
    "bridge": "github-reader",
    "upstream": {
      "installation_id": 234567,
      "repository_id": 345678,
      "owner_id": 456789,
      "owner": "example-org",
      "name": "example-repository"
    }
  }
}
```

Using the compiled development binary:

```sh
treeharbor client --socket /absolute/private/service.sock \
  --credential-file /absolute/private/owner.json \
  github --request /absolute/private/bind.json

treeharbor github verify --config /absolute/private/treeharbor/github-app.json \
  --socket /absolute/private/service.sock \
  --credential-file /absolute/private/bridge.json \
  --binding aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa \
  --probe bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb --epoch 1
```

The assigned bridge compares its local App configuration with the server's pins,
performs the read-only probe and records its immutable observation. Neither the App
key nor the temporary GitHub token enters PostgreSQL. The server authenticates the
bridge's report; it does not independently attest the remote HTTP exchange. Do not
run the bridge with agent-supplied configuration or within the agent sandbox.

An attempt has a fixed 120-second server deadline, which a retry cannot extend.
Reuse its `--probe` ID after interruption: a pending attempt may repeat the read-only
GitHub probe; an observed attempt returns its retained result without accessing the
App key or network. At most four unexpired pending attempts per binding are allowed
as a resource bound. After expiry use a new probe UUID; retained evidence is not
erased. A lost token response can still leave a temporary GitHub token as described
above. There is no distributed transaction between GitHub and PostgreSQL.

Other requests use the same `client ... github --request FILE` command:

```json
{"action":"status","epoch":1,"binding":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}
```

```json
{"action":"revoke","key":"disconnect-repository-1","epoch":1,"binding":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"}
```

Status requires `read`; revocation requires the Human's `repository.write`.
Revocation is irreversible. Reconfiguration requires a new binding UUID after
revoking the previous active binding. Only one active binding is allowed per
TreeHarbor repository. Connection observations remain audit evidence. A committed
mutation replay may return its historical receipt after revocation; that does not
reactivate the binding. Use `status` for current binding state. `github verify`
checks current assignment, authorization, authority epoch and active state even
when retrieving an observed attempt.

Revocation prevents acceptance of new reports, but cannot cancel a GitHub request
already in flight. Revoke the App installation/key as necessary for emergency
upstream access removal. A connection observation never grants CI approval, source
import or permission to publish. `publication_ready` stays false.

The command API also exposes assigned-Bridge actions `probe_begin`, `probe_context`
and `probe_finish` for custom trusted adapters. All take `epoch`, `binding` and
`probe`; mutations additionally take a unique `key`, and finish takes `observation`
in the exact v1 probe format. Unknown fields are rejected. A successful command
returns the standard `version`/`result` envelope. Wrong scope/assignment is refused;
inactive, expired or completed attempts reject new effects with
`host_binding_state` (HTTP 409). A reused key with different input produces an
idempotency conflict. Retry transport failures with the same key and input, then
inspect current state; do not interpret an old receipt as current readiness.

## Verification and remaining gates

```sh
cargo test -p treeharbor-github -p treeharbor-service --lib --locked
# Optional public HTTPS compatibility check; no credentials or writes:
cargo test -p treeharbor-github --locked live_public_github_https_api -- --ignored
```

The tests exercise the actual HTTP client against local protocol fixtures, including
identity replacement, broader tokens, suspension, expiry, redirects, rate limits,
lost token responses, response bounds and revocation errors. OpenSSL independently
verifies a test JWT signature. The public fixture key is never an App credential.
The optional public HTTPS check proves only TLS/API-version compatibility.

Database/CLI tests additionally exercise durable bindings, immutable observations,
permission separation, concurrent reports, deadline expiry during commit,
revocation, populated schema upgrades and real server restart. Their reported
observations use trusted test fixtures, not an installed GitHub App.

On 2026-10-24 the actual Rust client passed a live installed-App read probe against
the public synthetic [acceptance repository](https://github.com/TreeHarbor/TreeHarbor-acceptance).
The [raw probe receipt](evidence/github-app-probe-20261024.json) records the exact
identities, successful connection and revocation of its temporary token. The App
is installed on that repository only. Its broader branch/PR/queue permissions are
for later acceptance; the probe itself requests a narrowed read-only token.

A separate live `github verify` saved its observation through the local service
and PostgreSQL. After killing and restarting that service, repeating the same
probe with an intentionally absent App configuration returned the identical
observation and binding/probe/epoch identities. The replay envelope is current
context (`state: observed`), not the original finish receipt (`outcome: observed`).
It remains historical evidence, with `publication_ready: false`.

This establishes read access and durable observation only. Installed-App source
import, branch/PR publication, checks, signed callbacks and complete merge-queue
membership/final-tree evidence remain separate acceptance gates. The
[exact source import](github-import.md) and GitHub branch transport currently need
a Linux bridge host; the read probe and local control service also run on macOS.
No branch protection is changed or bypassed by verification.
