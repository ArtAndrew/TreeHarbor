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
