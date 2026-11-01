# Inspect GitHub after a restore

Development command after 0.1.11; not available in the published 0.1.11 package.
This collects current branch and PR identities for an operator investigating a
restored Connected installation. It does **not** reconnect the service or complete
disaster recovery. Keep old publishers fenced and retain the restored operation
journals as described in [restore configuration](restore-configure.md).

Use the private [GitHub App configuration](github-connection.md#configuration-and-command)
for the exact original repository. The installation needs Contents-read,
Pull requests-read and Metadata-read. A PR Preview installation already grants
these capabilities; this command requests only read permissions for its temporary
token, even when the App has broader permissions. No queue, Actions, Checks-write,
administration permission or TreeHarbor service credential is requested.

```sh
# Run from a source checkout until a package containing this command is released.
# The output can contain private repository metadata; keep the file private.
umask 077
cargo run -p treeharbor-service --locked -- github restore-inventory \
  --config /absolute/private/treeharbor/github-app.json \
  > /absolute/private/treeharbor/restore-inventory.json
```

Choose a new output filename. Shell redirection can leave an empty file after a
failure; file existence is not success. Check the command's exit status and JSON.
The bridge validates the pinned App, installation, owner and numeric repository
identity, enumerates heads and PRs, then attempts to revoke its token. The JSON
records `token_revoked`; failed revocation produces a nonzero CLI exit even when
observations were printed. Process cancellation can leave a token until its
provider expiry. Do not print or pass App keys to agents to diagnose failures.

The report includes:

- Repository/App IDs, the observed default branch, and start/finish timestamps.
- Every returned `refs/heads/` branch with its node ID, name and commit OID.
- PR identities, states, head/base repository IDs and commit OIDs, plus the
  merge commit when GitHub returns one. Open, closed and merged PRs are included,
  even when their source branch no longer exists or the operation is absent from
  the backup. A deleted fork remains a null head repository, not the base repository.
- `reconciliation_complete: false`, always. No database state is changed.

The bridge does not filter discovery by a known operation marker or by the
`treeharbor/` branch prefix. A human PR is still a human PR: discovery does not
claim ownership, create a ChangeSet or authorize replay of a publication.

Each connection is bounded to 100 pages of at most 50 records. The observation
phase has a 120-second deadline; existing HTTP size/request limits also apply.
Unchanged counts, unique IDs/names, advancing cursors and exact terminal counts
are required. An error, partial GraphQL response, inconsistent count or exhausted
budget returns a failure rather than a truncated successful inventory. These are
operational limits, not licensing limits.

GitHub pagination is not an atomic snapshot. Changes while traversing can make
the report inconsistent or incomplete even when counts match. An operator must
resolve discrepancies against the exact backup and reobserve selected effects
before any later reconnection. Tags, source object contents, PR bodies/operation
markers, queue memberships and check attempts are outside this command's scope.
The supported resolution/admission procedure remains unfinished. Do not delete
the quarantine journal or clear `host_recovery_required` based on this report.

API references: [repository connections](https://docs.github.com/en/graphql/reference/repos)
and [cursor pagination](https://docs.github.com/en/graphql/guides/using-pagination-in-the-graphql-api),
checked 2026-11-01.
