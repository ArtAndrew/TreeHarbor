# HTTPS ingress for GitHub events

Development deployment recipe for the [signed webhook receiver](github-events.md).
It exposes only `POST /github/events` over HTTPS through Caddy **2.11.7**. The
general TreeHarbor command socket remains private. This is not a packaged product
installation or proof of a real installed GitHub App.

## Prerequisites and start

Run the Rust receiver with a migrated PostgreSQL schema, a subscribed repository
channel, an App webhook secret and a **10,000 ms request timeout**, as shown in the
receiver guide. The timeout is part of this proxy recipe: its upstream response
deadline is 12 seconds. A database lock timeout can return 503 earlier.

Install the official Caddy 2.11.7 binary separately. The following archive hashes
are pinned from that release's GitHub asset metadata; this is archive integrity
verification, not a signature-verification claim:

| Archive | SHA-256 |
|---|---|
| `caddy_2.11.7_linux_amd64.tar.gz` | `727b91701a392de6ebc5027509f548bf39979e5216340d0faed8fa5e69c84f8b` |
| `caddy_2.11.7_mac_arm64.tar.gz` | `cda3030e5d5b13eb9f0b6fb541037f633bddd958c070c5f561d7a5cccb581665` |

Get archives from the [official release](https://github.com/caddyserver/caddy/releases/tag/v2.11.7),
verify the matching hash before extraction/execution, and preserve the included
upstream license. Caddy is an external component, not part of the Rust binary.

Provide a DNS name reachable by GitHub and a valid certificate/full chain for that
name, plus its private key. Use a publicly trusted certificate for GitHub and keep
GitHub SSL verification enabled. Certificate issuance, DNS and renewals are managed
by the operator in this recipe; Caddy automatic issuance and HTTP redirects are
disabled. Local test certificates do not prove public callback reachability.

Set these variables in the trusted proxy's service configuration (examples):

```sh
export TH_WEBHOOK_ADDRESS='hooks.example.com'
export TH_WEBHOOK_BIND='0.0.0.0'
export TH_WEBHOOK_TLS_CERT='/private/treeharbor-tls/fullchain.pem'
export TH_WEBHOOK_TLS_KEY='/private/treeharbor-tls/private-key.pem'
export TH_WEBHOOK_SOCKET='/private/treeharbor-events/github.sock'
caddy validate --config deploy/webhook/Caddyfile --adapter caddyfile
caddy run --config deploy/webhook/Caddyfile --adapter caddyfile
```

All variables are trusted operator configuration; never derive them from a webhook,
agent or repository file. Use only a hostname with an optional port in ADDRESS,
and absolute file/socket paths. A different port must also be in the App webhook
URL and network configuration. The proxy needs OS permission to bind that port;
the local acceptance test uses an unprivileged loopback port.

The receiver's socket directory is 0700 and socket 0600. This recipe runs the proxy
as the receiver's trusted OS user, with read access to the certificate/key and
socket. An alternative container deployment must explicitly provide the same
numeric UID and the required mounts. Do not make the socket world-readable or
mount the control socket, App signing key, operator credentials, source tree or
agent volumes into the proxy. Using the same host UID is **not** a process-isolation
boundary; use the customer's runtime/service manager to enforce filesystem, network,
CPU, memory, process and disk limits. Neither component is an agent sandbox.

Configure the App webhook URL as `https://hooks.example.com/github/events` after
the proxy is running. The proxy has no App key, installation token or webhook secret;
only the Rust receiver validates HMAC and repository scope. Test a real App ping,
then a subscribed group event, and inspect the retained event through the scoped
TreeHarbor API. A successful TLS connection, a 404 health response or a ping alone
does not prove that a group can commit.

## Request and operational behavior

- Only the exact path and POST method can reach the receiver. Query suffixes are
  rejected by the receiver. Other routes, including `/health`, return 404 publicly.
- Original bytes and signature headers pass through; there is no JSON rewriting,
  decompression, automatic write retry or generic upstream URL. Environment proxy
  variables are ignored for the Unix upstream.
- The body limit is 1 MiB and the proxy header limit is 8 KiB. The receiver also
  bounds header count, body size, concurrent work and its database pool. Proxy-added
  forwarding headers count toward the receiver's bounds. Oversized requests fail.
- Header/body/write/idle deadlines are 5/15/20/30 seconds at the proxy; receiver
  deadlines can fail earlier. These are per-request protections, not a fleet-wide
  rate limiter or aggregate CPU/RAM/connection quota. Apply those externally.
- Caddy's admin endpoint, persisted config and access logs are disabled. Runtime
  JSON error logs remain enabled but omit request headers and URI. Do not enable
  debug/request-body logging or credential logging. Collect bounded, rotated logs
  and receiver/database metrics using the deployment's operational tooling.

200 for a group means the receiver committed its event, delivery identity and
outbox. A failed or missing response can leave an unknown outcome. Retrying must
preserve the original body and delivery UUID; neither a proxy failure nor a restart
authorizes changed content under that UUID. GitHub does not automatically redeliver
failed webhooks: inspect delivery failures and arrange explicit redelivery/backfill.
Stored events still do not prove group membership, checks or a final merge.

For certificate replacement, install the new files privately, validate the config,
then restart Caddy under the service manager and verify TLS plus actual delivery.
For webhook-secret rotation, follow the receiver's two-key overlap/restart process;
TLS rotation does not change HMAC keys. Keep the event database across restarts.
To disable this endpoint, turn off webhook delivery or remove the App installation,
stop the proxy and receiver, and revoke the TreeHarbor channel as appropriate.
Keep accepted evidence for recovery; no repository history is changed by stopping
ingress. The admin API is intentionally unavailable for remote configuration.

## Reproduce acceptance locally

Requirements: Caddy 2.11.7, OpenSSL, Python 3, curl with HTTP/2, `psql`, built TreeHarbor binary and a
disposable PostgreSQL database reachable by Unix socket. Do not point this test at
a production database. It creates a unique schema and removes it after testing.

```sh
cargo build -p treeharbor-service --locked
export TH_CADDY_BIN='/absolute/path/to/caddy'
export TH_SERVICE_TEST_DATABASE_URL='host=/private/postgres dbname=treeharbor_test'
# Optional TH_PSQL and TH_TREEHARBOR_BIN override the executable paths.
python3 scripts/test-webhook-proxy.py
```

The test consumes the actual deployment Caddyfile and real receiver/database. It
uses an explicitly trusted test certificate and separately verifies rejection by
the ordinary trust store. Coverage includes raw signed bytes, duplicate headers,
wrong routes/encoding, size and slow-request limits, actual group persistence,
replay, channel revocation, receiver outage/restart, killed proxy before/after DB
commit, a real database lock timeout and error-log filtering. It also checks HTTP/2
delivery, the exact body-size boundary and certificate replacement after restart. The synthetic
binding/probe fixture is not live GitHub authentication. Public certificate
lifecycle, DNS, actual App callbacks, automatic delivery reconciliation and a
complete Connected run remain release gates.
