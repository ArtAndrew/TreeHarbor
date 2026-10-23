# GitHub App connection probe

Development command for an operator preparing a Connected installation. It verifies
the App installation, selected repository access and stable numeric identities.
It does **not** create a TreeHarbor repository binding, import source, publish a
branch/PR, inspect branch rules or enqueue a change. Connected is not released.

## Prepare an App

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

A real installed-App probe and PR/check/merge-queue scenario remain separate
acceptance gates. Durable binding, network Git import, projection/outbox,
webhook validation, PR/check reconciliation and merge confirmation are not
implemented by this increment. No branch protection is changed or bypassed.
