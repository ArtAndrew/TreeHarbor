# Measure native snapshot import

This opt-in experiment measures the actual CLI, service, PostgreSQL and local
object-store path. It reports pack creation, first import, and replay of the
completed request separately. It does not measure full Git history, workspace
preparation, builds, merge latency, remote traffic or infrastructure cost.

Use an isolated local PostgreSQL database and a trusted small Git fixture. The
script creates a uniquely named schema and a temporary owner credential, starts
one local service, then stops it and deletes that schema and temporary files.
It never connects to GitHub or modifies the input repository. Do not point it at a
production database. Abrupt machine/process termination may require manual cleanup
of its `th_test_*` schema and `th-import-*` temporary directory.

Requirements: Unix, Python 3.9+, Git, PostgreSQL `psql` matching your local server,
and a compatible TreeHarbor binary. For performance observations use the release
profile or a verified release binary, not a debug build. The input is a UTF-8 Git
tree without submodules; relative symlinks must satisfy normal source-pack safety
rules. Its default logical-byte limit is 64 MiB; this is a small-workload probe.

Put the disposable database connection string in a private file with mode `0600`.
Use local socket authentication or `PGPASSFILE`; do not embed a password in that
connection string, which is passed to `psql` as an argument.
Then run from the product repository:

```sh
python3 scripts/benchmark-source-import.py \
  --binary /absolute/path/to/treeharbor \
  --psql /absolute/path/to/psql \
  --database-url-file /absolute/path/to/private-database-url \
  --repository /absolute/path/to/local-git-fixture \
  --revision FULL_COMMIT_ID \
  --output /absolute/path/to/new-import-result.json
```

The output records the resolved input commit, binary SHA-256, platform, pack
identities/counts, three elapsed times and whether the completed result replayed
exactly. Temporary paths, database URLs and credentials are excluded. An existing
output is never replaced. Git extraction and service setup are outside the three
timed phases; the pack and durable import remain separately visible. The CLI
supports at most 1024 explicit selection paths, so the experiment selects the
fixture's top-level files/directories, not every leaf individually.

One run produces one observation, not a median or p95. OS caches are uncontrolled;
do not label these results cold-cache or compare their import scope to a clone
of full history. Record hardware, storage, PostgreSQL settings and background
load alongside any results you publish. A fair end-to-end comparison also needs
server resource usage, cleanup, tuned Git baselines and equivalent durability.

This script is not part of the correctness CI run. It checks successful import
and exact completed-receipt replay; it does not replace export/restore validation,
crash tests or the broader benchmark and release gates.

## Initial observation

[Raw sample](benchmarks/import-macos-arm64-sample.json): 1,131 tracked files,
8,125,924 encoded pack bytes, one local macOS arm64 run. Pack creation took
10.35 seconds, import 18.44 seconds and completed replay 0.051 seconds. These
numbers expose initial setup cost; they do not demonstrate a performance advantage.

The local optimized binary was built from commit
`dacfe018b11787ab74a81937f58f0806e09188de` with the repository's release profile.
The raw report pins the input snapshot separately and records the binary hash.
This is not a signed release-artifact benchmark. Hardware/storage configuration
and server CPU were not captured, and OS cache state was uncontrolled; treat it
as a limited diagnostic observation, not a cross-machine comparison or SLO.
