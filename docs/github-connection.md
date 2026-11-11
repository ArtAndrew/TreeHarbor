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

## Verify a bundle from an archived repository

`github archive-preflight` checks that a complete local bundle contains the exact
branch tips and history of a GitHub repository archived by its owner. GitHub
makes an archived repository read-only. Keep it archived throughout a handoff.
[GitHub archive behavior](https://docs.github.com/en/repositories/archiving-a-github-repository/archiving-repositories)

Use a complete bare clone of that repository and an existing private scratch
directory. This command uses the App configuration above with Contents and
Metadata read access to the selected repository.

```sh
umask 077
git -C "$TH_BARE_CLONE" bundle create "$TH_SCRATCH/source.bundle" --branches
treeharbor github archive-preflight --config "$TH_APP_CONFIG" \
  --bundle "$TH_SCRATCH/source.bundle" --scratch "$TH_SCRATCH"
```

TreeHarbor checks the pinned repository identity and archive state, reads the
branch set twice, verifies every page and revokes the App token. It then imports
the shared history into temporary native storage and matches every advertised
branch and Git tip to the observed set. The v1 JSON report contains `archive`
with the scoped observation and its timestamps, `branches` with each full ref's
Git revision, native revision and snapshot, and the distinct source object count
and encoded bytes. `--scratch` removes the temporary store after the check.

To keep the verified native objects and branch manifest for a later handoff,
replace `--scratch` with `--output` and choose a new directory under an existing
owner-private parent. Keep the reported fingerprint with your migration records.

```sh
treeharbor github archive-preflight --config "$TH_APP_CONFIG" \
  --bundle "$TH_SCRATCH/source.bundle" --output "$TH_SCRATCH/prepared"
treeharbor git-export --archive-spool "$TH_SCRATCH/prepared" \
  --archive-fingerprint "$TH_SPOOL_FINGERPRINT" --output "$TH_SCRATCH/restored"
```

The second command opens the saved native objects locally and writes a Git
bundle. It works with the original bundle and GitHub connection unavailable.
See the [format and recovery procedure](git-import.md#durable-archive-branch-spool-v1).

The branch limit is 128. Use `--branches` for the bundle. Tags and other
advertised refs are rejected, and the usual [Git import limits](git-import.md#isolation-limits-and-recovery)
apply. Use the report as a preflight for source fidelity. Switching write
authority still requires a separate transaction and verification against the
current host state.

## Inspect tags and open pull requests before a handoff

With a development build, use the pinned App configuration for an archived
repository. The App needs Contents, Metadata and Pull requests read access.

```sh
treeharbor github archive-inventory --config "$TH_APP_CONFIG"
```

The client reads the complete branch, tag and open pull request inventories
twice. It checks repository identity and the archived state on each page and
around both reads. It returns a report after revoking its scoped read token.
The report covers up to 128 branches and up to 5,000 tags and open pull
requests each. [GitHub GraphQL repository reference](https://docs.github.com/en/graphql/reference/repos#repository)

The v1 JSON report contains `version`, the branch observation in `archive`,
`tags` and `open_pulls`. Tag entries retain the full ref identity, object kind
and OID, including annotated tag objects. Open pull requests retain the
observed fork identity, head and base revisions, number and draft state.
Use this inventory to prepare what must be preserved before the write
handoff. [GitHub ref and pull request fields](https://docs.github.com/en/graphql/reference/git#ref) · [Pull request reference](https://docs.github.com/en/graphql/reference/pulls#pullrequest)

## Stage all archived branches in TreeHarbor

In a development build, use `archive-stage` to upload the saved graph once.
The service needs object storage and an active Connected binding whose App and
upstream identity match the preflight. Use a human credential with
`repository.write` for the repository on the local Unix socket. The command
uses the retained spool and runs without a Git executable or GitHub access.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  archive-stage --spool "$TH_SCRATCH/prepared" \
  --archive-fingerprint "$TH_SPOOL_FINGERPRINT" --binding "$TH_BINDING" \
  --epoch "$TH_EPOCH" --state "$TH_SCRATCH/archive-stage.json"
```

Keep the spool and the private state file until staging is confirmed. The
client writes the upload identity to that file before sending data. Retry with
the same arguments after an interrupted upload or response. A completed retry
returns the saved receipt even after the upload expires. An expired unfinished
upload returns `upload_state_changed`. After the server confirms that result,
use a new private state filename to start another upload. If the binding or
epoch changes, verify the current connection before staging again.

The service checks that the complete uploaded graph is exactly the union of the
declared tips. It registers each distinct native tip as a durable base, closes
the upload, and saves the receipt in one PostgreSQL transaction. Branch names
that share a tip share its base. The receipt retains every branch name and
reports `state: staged`. It records retained source at the given epoch.
The GitHub binding and the current write authority stay active. Actual cutover
requires fresh host evidence and a separate fenced authority transaction.

### Archive stage protocol v1

Send the standard scoped command envelope with `method` set to `github`. The
[request fixture](../crates/metadata/tests/fixtures/github-archive-stage-v1.json)
shows its nested `request` with action `archive_stage`. The operation uses a
stable key, expected Connected epoch, active binding UUID, and the UUID of
one source upload owned by the caller. Its v1 specification includes the
spool fingerprint, App and upstream identity, default branch, and exact object
count and encoded byte total. The `branches` map accepts up to 128 full
`refs/heads/` names. After that prefix, the branch name is at most 128 ASCII
bytes. Each slash-separated segment starts with a letter or digit and can
contain letters, digits, periods, underscores and hyphens up to 64 bytes.
Segments cannot contain `..` or end in a period or `.lock`. Each tip contains
a canonical Git SHA-1 revision, native revision, and snapshot.

A successful v1 command returns the usual `version` and `result` envelope.
The result has `version`, `tenant`, `repository`, `authority` (`connected`),
`epoch`, `binding`, `upload`, `state` (`staged`) and the exact
`specification`. Changing the request under a saved key returns
`idempotency_conflict`. The caller must still be authorized to replay a
historical result. A mismatched or incomplete object set returns
`upload_membership` and leaves the upload available until its fixed deadline.
The service verifies uploaded native objects and their claimed Git origins.
Keep the independently observed preflight report with the staging receipt for
the eventual authority handoff.

The CLI's private state format v1 stores `version`, `upload`, and `intent`.
The intent binds the tenant, repository, epoch, binding, upload TTL and exact
specification. Reusing the file with different inputs fails before sending
an upload. The default TTL is 3600 seconds.

## Verify the retained Git history on the server

Run `archive-verify` with the same local operator credential. For a stage
created by the CLI, its key is `archive-` followed by the `upload` UUID
in the stage receipt and `-stage`.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  --timeout-ms 60000 archive-verify --stage-key "archive-${TH_UPLOAD}-stage" \
  --key verify-archive-001 --epoch "$TH_EPOCH"
```

The server selects the exact object generations retained by the completed
upload and its permanent branch pins. It reconstructs every Git tip in one
Rust projection and checks the complete object count and byte total.
The retained objects are sufficient even after removing the original bundle.
An absent object, incomplete graph or differing Git tip prevents a new proof.

The server rechecks the active Connected binding, epoch and repository
permission before recording the proof. Retry the same command and key after
an uncertain response. A completed retry returns the historical receipt.
Use a new key to verify the current storage again. Verification is bounded
by the server request timeout and the default Git projection limits
of 100,000 objects and 4 GiB of encoded source. Actual cutover still
requires fresh host evidence and a separate write-authority transaction.

The local v1 endpoint is `POST /v1/github/archive/verify`. It accepts the
[scoped request](../crates/service/tests/fixtures/archive-proof-request-v1.json)
with `key`, `stage_key`, and expected `epoch`. Its standard response envelope
has `version` and `result`. The result contains `version`, `tenant`,
`repository`, `authority` (`connected`), `epoch`, `binding`, `upload`,
`stage_key`, `state` (`verified`), `spool_fingerprint`, `object_count` and
`encoded_bytes`. A missing or inconsistent object returns
`source_mapping_mismatch`. The endpoint uses the local Unix socket and
requires a human `repository.write` credential.

## Record a fresh host observation for the verified history

Use a human `repository.write` credential and the key used for
`archive-verify` with the expected Connected epoch.

```sh
treeharbor client --socket "$TH_SOCKET" --credential-file "$TH_OWNER" \
  archive-observe-begin --epoch "$TH_EPOCH" --key observe-archive-001 \
  --proof-key verify-archive-001
mkdir -m 700 "$TH_SCRATCH/observe-archive-001"
treeharbor github archive-observe --socket "$TH_SOCKET" \
  --credential-file "$TH_BRIDGE" --config "$TH_APP_CONFIG" --epoch "$TH_EPOCH" \
  --request-key observe-archive-001 --state "$TH_SCRATCH/observe-archive-001"
```

The server creates a request valid for 180 seconds against the active binding
and verified archive. The assigned bridge uses its existing `host.read`
credential and the read-only App inventory operation. It saves the complete
capture as `inventory.json` before submitting the observation, then saves
`receipt.json`. Retain this directory for retries and for preserving tags and
open proposals. A completed retry with the same directory returns the
historical receipt. Use a new request key and directory for a fresh capture.

The v1 local actions are `archive_observe_begin` (owner),
`archive_observe_context` (assigned bridge, with `epoch` and `request_key`),
and [`archive_observe_record`](../crates/metadata/tests/fixtures/github-archive-observation-v1.json)
(assigned bridge). The server checks the App, upstream, epoch, observation
time, token revocation and exact branch set against the verified source.
It returns one `observed` receipt for the request. An outdated report or moved
tip returns `archive_observation_mismatch`.

The observation contains exact Git branch tips and the count and SHA-256
digest of each complete tag and open pull request set. The digest input is
`treeharbor-archive-inventory\0v1\0`, the set name (`tags` or `open-pulls`),
a NUL byte and the compact JSON of the v1 typed items. Tags are sorted by
name and pull requests by number. The receipt key is `archive-observed-`
followed by the hex SHA-256 of `treeharbor-archive-observation\0v1\0` and the
request key. The observed inventory and verified contents are inputs to
the subsequent preservation and write-authority transaction.

## Preserve the observed Git refs

Use the same private observation directory and a full local SHA-1 Git bundle
with every observed branch, tag and `refs/pull/<number>/head`. Include the head
from the base repository's pull ref for a fork PR. See
[GitHub's pull ref instructions](https://docs.github.com/en/pull-requests/how-tos/review-pull-requests/checking-out-pull-requests-locally).
Prepare the bundle from a private bare mirror containing these exact refs.
Keep the branch-only bundle for `archive-preflight`.

```sh
git -C "$TH_ARCHIVED_MIRROR" bundle create "$TH_FULL_BUNDLE" \
  --branches --tags --glob='refs/pull/*/head'
treeharbor github archive-preserve --state "$TH_SCRATCH/observe-archive-001" \
  --bundle "$TH_FULL_BUNDLE" --output "$TH_SCRATCH/preserved-archive"
```

The command matches the saved inventory to the observation receipt, checks
the exact ref names, object IDs and types, and requires each PR's recorded base
commit. It verifies a copy in an empty temporary bare repository with
`git fsck --strict`. Missing, additional or moved refs, filtered or incremental
bundles, unreachable objects, and damaged history stop publication. An optional
`HEAD` in the bundle must match the observed default branch. The default bounds
are 100,000 Git objects, 4 GiB of object contents, 8 GiB of bundle bytes and
300 seconds for the Git verification. Use a private volume with enough space
for the copy and the temporary Git object database.

The output is a new private directory containing `archive.bundle` and
`manifest.json`. The directory is synced and published without replacement.
If an error leaves an output directory, inspect its digests before retrying.
The [v1 manifest fixture](../crates/service/tests/fixtures/github-archive-preservation-v1.json)
defines `format: treeharbor-github-archive`, `version: 1`, the complete
`receipt` and `inventory`, and `bundle` with hex SHA-256, byte count,
object count and total uncompressed object bytes. The CLI returns the
manifest fingerprint, SHA-256 of `treeharbor-github-archive\0v1\0` followed
by the exact manifest bytes, formatted as `sha256:<hex>`.

Retain both files. An empty SHA-1 Git repository can verify the bundle and
fetch the branch, tag and pull refs directly. Annotated tag bytes and the
original fork PR head are preserved. The original PR can be identified by the
archived repository and PR number in the manifest. Discussion and reviews
remain at GitHub. This command runs offline against the saved observation.
Recording the preserved objects on the service and transferring authority
require separate steps.

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
