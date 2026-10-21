# Security

TreeHarbor is in development and has no supported production release yet. Do not
expose the current libraries as an untrusted multi-tenant service or place real
secrets in test workspaces. A workspace does not provide process isolation.

Report vulnerabilities through this repository's GitHub **Security → Report a
vulnerability** when private reporting is enabled. If that option is unavailable,
open an issue asking for a private reporting channel without posting vulnerability
details, secrets or customer data. No response-time SLA is currently promised.

Security work must cover tenant authorization, object reachability, path/symlink
safety, credentials, untrusted hooks, stale worker fencing and validator identity.
Please include the tested revision and a minimal reproduction suitable for
sharing privately.
