# Run two agents in Standalone

Start from an empty repository. Two isolated agents edit the same file. The
queue lands the first change and stops on the second change's conflict. The
resolved version needs a fresh check and review before it lands. The example
also restores the result from an ordinary Git export after workspace cleanup.

Use a disposable Linux x86-64 or ARM64 VM with cgroup v2, local Docker configured
with the `runsc` gVisor runtime, Python 3.11+, static BusyBox, `e2fsprogs` and
Git for the final export check. Run as a non-root user with local Docker access.
The drill uses noninteractive `sudo` for temporary loop mounts, its Engine test
proxy and cleanup. Stage `postgres:18.6` in local Docker before the run.

Verify and extract a signed package, then install it into your chosen prefix.
Run this command from the extracted package directory. The example uses the
installed binaries and the static shell task supplied with the archive.
[Package verification and installation](../../docs/release-packaging.md)

```sh
python3 examples/standalone/run.py --treeharbor "$TH_INSTALL_PREFIX/bin/treeharbor" \
  --busybox "$(command -v busybox)"
```

The runner starts its own PostgreSQL container with a private Unix socket and
networking disabled. It creates a disposable repository and scoped local roles,
runs the protected queue workflow and removes the temporary service, database
and workspaces. The native path uses the local service. Git runs for the final
export check. The example needs no GitHub account or Rust toolchain.

## Inspect the result in Git

Starting with the 0.1.36 package, `--export-dir` keeps the Git history after
both workspaces and the service have been removed.

```sh
result=$(mktemp -d)
python3 examples/standalone/run.py --treeharbor "$TH_INSTALL_PREFIX/bin/treeharbor" \
  --busybox "$(command -v busybox)" --export-dir "$result/export"
git clone --branch main "$result/export/history.bundle" "$result/review"
git -C "$result/review" log --oneline -- same.txt
git -C "$result/review" show main:same.txt
```

The final file reads `alpha + beta`. Its first parent contains the first
agent's edit. The export directory also contains a manifest mapping native
revisions to Git commits. Each run needs a new output path under a private
directory owned by the operator.

## Keep a native backup

To inspect the same run in Git and restore it in TreeHarbor, choose two new
directories under one private parent. Keep the printed manifest SHA-256,
tenant and repository ID separately with your backup records.

```sh
result=$(mktemp -d)
python3 examples/standalone/run.py --treeharbor "$TH_INSTALL_PREFIX/bin/treeharbor" \
  --busybox "$(command -v busybox)" \
  --export-dir "$result/git" --backup-dir "$result/backup"
```

The example stops its writers and uses the matching PostgreSQL container's
`pg_dump`. The backup contains the database, native source objects, local
configuration and the completed queue journal. Store it privately. The
runner verifies its manifest before cleaning up the source. To bring up the
repository on a clean host, follow [Standalone recovery](../../docs/standalone-host-recovery.md).
