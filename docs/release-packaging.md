# Development release packaging

This page describes packaging and its verification scope. Published assets and
installed-workflow evidence are listed in [Releases](https://github.com/ArtAndrew/TreeHarbor/releases).
[Alpha.1](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.0-alpha.1) provides signed Linux x86-64 and ARM64 packages.
Installed runtime/package CI passed on both architectures; the operator-assisted
GitHub workflow passed on ARM64 with cleanup/export and access revocation.
See [the trial report](evidence/connected-pr-preview-20261028.json).
A source checkout or successful build alone does not establish this acceptance.
TreeHarbor queue certification and full Connected publication disaster recovery
remain later work; the preview requires independent backups and manual setup.

The [0.1.1-alpha.1 setup patch](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.1-alpha.1) has its own signed artifacts and
[installed ARM64 setup evidence](evidence/connected-preview-setup-20261028.json).
Its source and current setup checks are separate from the historical two-agent
GitHub trial in alpha.1. Follow the selected release's exact version/digest commands.

The [0.1.2-alpha.1 workspace patch](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.2-alpha.1)
adds durable generated [workspace requests](workspace-new.md). Its signed packages
exercise the new command in installed runtime CI on both Linux architectures;
it does not add a new live GitHub trial. Use its own signed bytes and provenance,
rather than a prior release's hashes or unsigned PR artifacts.

The [0.1.3-alpha.1 diagnostic patch](https://github.com/ArtAndrew/TreeHarbor/releases/tag/v0.1.3-alpha.1)
adds [local service diagnosis](doctor.md). Both signed Linux packages exercised
doctor around explicit database migration, followed by the managed/native runtime
workflows. Independent signature and data checks matched the original archives,
all eight subjects per architecture and the exact producer invocation. This does
not add a fresh live GitHub or LLM trial.

## Preparing signed alpha candidates

The manual `release-provenance.yml` workflow is reserved for an existing alpha tag
whose version matches Cargo.toml. It requires the exact source commit and an
independent successful main-push CI run/attempt for that commit. It refuses branch
dispatches, source/workflow mismatches and commits outside main. Ordinary PR CI
keeps read-only permissions.

For each Linux architecture, the selected main CI attempt builds and installs
the archive, runs the managed and native runtime tests, and saves and reloads
the control-plane image. CI retains the exact files and their test receipt. The
signing job selects the artifact by ID, verifies its digest and receipt, and
attests the tested archive, image and sidecars. The receipt identifies the
main CI attempt. The signature identifies the tagged signing run.
The pinned [GitHub attestation action](https://github.com/actions/attest/tree/1e69f48acb82d1966a394da916b4c1698aa569d6)
uses a short-lived GitHub signing identity; no stored signing key is required.
The workflow verifies all signed subjects with the official CLI, constraining the
repository, workflow, source commit and tag before retaining them for review.

A workflow definition does not establish signing or installed acceptance: inspect
the selected run and release record. It does not create or move tags, create
GitHub releases or publish assets. Its seven-day Actions artifacts
are candidates, not permanent distribution. Maintainers must inspect the selected
run/attempt, verify the downloaded bytes and signatures, complete the documented
Connected onboarding gates, and attach the unchanged files to a prerelease.
Each architecture has a distinct archive, `.metadata.zip` and
`.provenance.sigstore.json`. The metadata ZIP keeps the original sidecar filenames
and checksums together, so the two architectures do not collide in a release.
The producer signs the ZIP using the same tagged workflow. Attach both
architectures' package and image archives, their checksums, the metadata ZIP
and the signature bundle. Releases before 0.1.23 omit the image.
Do not rebuild or overwrite assets during publication; inspect any uncertain
upload before retrying.

For an archive and its signature bundle downloaded from the same reviewed job,
use the [official verifier](https://cli.github.com/manual/gh_attestation_verify):

```sh
gh attestation verify "$archive" --bundle "$bundle" \
  --repo ArtAndrew/TreeHarbor \
  --signer-workflow ArtAndrew/TreeHarbor/.github/workflows/release-provenance.yml \
  --signer-digest "$source_commit" --source-digest "$source_commit" \
  --source-ref "refs/tags/$tag" --deny-self-hosted-runners
```

Obtain the expected commit/tag from the reviewed release record. A successful
signature check authenticates the stated build identity and digest; it does not
prove full native component coverage, reproducible builds, production readiness
or completed Connected onboarding. Keep the runtime receipt and CI run identity
when recording acceptance evidence.

For installation, download the archive and matching provenance bundle. Verify the
archive before extracting or running its installer. The metadata ZIP is optional
for ordinary installation; to inspect its reports, first verify that ZIP using
the same command and bundle, then unpack it into a separate directory for that
architecture. Its retained original filenames allow `SBOM-SHA256SUMS` checks.

## Building development packages

The package builder needs Python 3.11+, Git and a clean committed checkout. Build
all three binaries for the same native Linux architecture with the pinned Rust toolchain:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --locked --release --target x86_64-unknown-linux-musl \
  -p treeharbor-service -p treeharbor-runtime --bins
python3 scripts/package-release.py --target x86_64-unknown-linux-musl \
  --bin-dir target/x86_64-unknown-linux-musl/release --output-dir /tmp/th-package
```

Use a new output directory. The builder refuses replacement of an archive. It
checks every executable's ELF architecture and rejects a dynamic interpreter, includes tracked
public documentation/examples and all vendored notices, and excludes the rest of
the checkout. The version comes from Cargo.toml. The manifest records the packaging
checkout revision and file hashes; this alone does not prove how binaries were
built. The archive has deterministic headers for identical inputs, which does not
claim reproducible compiler output. Never distribute a development package as an
approved release merely because these checks pass.

The builder also requires the reviewed [static-toolchain notice inventory](../third-party/toolchain/README.md).
Missing, changed, omitted or untracked notices refuse packaging, as does a compiler
pin that has not received notice review. This verifies notice inclusion and hashes;
it does not establish the compiler used for the supplied binaries or replace a
target-specific SBOM and signed build provenance.

Package CI also retains [binary-derived Rust dependency reports](binary-dependency-reports.md)
and observed toolchain/linker input hashes beside its archives. Their coverage is explicit:
they do not establish complete native component coverage or signed provenance.
The first preview requires an honestly scoped SBOM, the required notices and
signed provenance of the exact tested artifacts. Expanding native attribution
coverage is a separate task; it must not be reported as already complete.

Once a release has been approved and its archive checksum verified against the
release's trusted metadata, offline installation will use:

```sh
sha256sum --check --strict treeharbor-VERSION-TARGET.tar.gz.sha256
tar -xzf treeharbor-VERSION-TARGET.tar.gz
cd treeharbor-VERSION-TARGET
./install.sh --prefix "$HOME/.local"
"$HOME/.local/bin/treeharbor" --version
```

Replace VERSION/TARGET with the actual release name. The installer checks internal
file hashes and creates a versioned directory plus one CLI symlink. It refuses to
replace an existing destination or command. For side-by-side evaluation, choose a
new prefix. An interrupted install can leave a complete version directory or an
empty `.install-lock`; inspect it before retrying. No database migration, service
startup, credentials or runtime provisioning happens during installation.

The runtime init resides at `PREFIX/lib/treeharbor/VERSION/bin/treeharbor-runtime-init`.
Use the protected runtime setup described in [managed runs](managed-runs.md).
The optional `treeharbor-cow-driver` is in the same directory. The
[Copy-on-Write profile](experimental-cow.md#install-the-host-helper) requires a
separate protected host copy and explicit operator elevation. The ordinary
installer neither grants privileges nor modifies host mount or sudo policy.
PostgreSQL, Docker/gVisor, bounded volumes and host access are separate prerequisites.
Removing the CLI symlink and that version directory uninstalls the package; retain
repository state and backups separately.

### Upgrade from 0.1.34-alpha.1

0.1.35 adds a queue recovery option to the CLI. Both versions use schema 035
and the same queue journal format. Keep the original queue ID, state directory
and config. Stop the active bridge before handing that journal to the new CLI.
To inspect a pending request after a rollback, retain the newer CLI and its
`--recover-only` option. [Queue recovery](github-queues.md#run-the-assigned-bridge).

### Upgrade from 0.1.33-alpha.1

0.1.34 adds a client-side intent file for Git-compatible workspaces. Both
packages use schema 035, the same source objects and the same workspace
creation API. Install the new package alongside the old one and run
`doctor` with the existing configuration. Keep the original executable for
pairs that pin its hash.

File-only workspace intents still use version 1. To roll back, keep the
0.1.34 client for any outstanding version 2 Git-compatible intent. The
0.1.33 service accepts its creation request. [State format and retry](workspace-new.md#keep-private-git-history-for-a-task).

### Upgrade from 0.1.32-alpha.1

Both packages use database schema 035 and the same object format. Stop active
writers and bridges, take a [cold backup](backup-restore.md), install 0.1.33
alongside 0.1.32 and run `doctor`. Keep the old executable, its absolute path,
and the config and journal for each pair already in progress. Resume each pair
with the executable it pinned.

For a new pair, collect its three replies with the 0.1.33 tools and use the
[packaged setup command](../examples/connected-pr/README.md#configure-one-immutable-task-pair).
The explicit v1 config is still supported. To roll back, stop the writers and
switch back to the retained 0.1.32 service and tools. Keep the matching 0.1.33
binary and journals for any outstanding 0.1.33 operations.

### Upgrade from 0.1.31-alpha.1

0.1.32 adds a read-only fallback to the GitHub check publisher. Both versions
use schema 035 and the same publication records. Stop active writers and
bridges, take a [cold backup](backup-restore.md), and install 0.1.32 alongside
0.1.31. Keep the bridge's private journal. Resume an uncertain publication with
its original ID and `--recover-only`. To roll back, stop the writers and switch
back to the retained 0.1.31 package.

### Upgrade from 0.1.30-alpha.1

0.1.31 keeps database schema 035. Stop active writers and bridges, take a
[cold backup](backup-restore.md), install the new package alongside 0.1.30
and run `doctor` against the existing configuration. The default Git import
pack is compatible with both versions. A pending `--packed` import has a v4
directory that needs the 0.1.31 CLI to resume or export. Before rolling back
the bridge, complete that import or retain its bundle and 0.1.31 tools. An
accepted revision is stored independently of the bridge's local pack.

### Upgrade from 0.1.29-alpha.1

Both versions use schema 035. Drain the workers, stop the services and bridges,
and make a [cold backup](backup-restore.md). Install 0.1.30 alongside 0.1.29
and run `doctor` on the existing configuration. Finish any active agent pair
with its original binary, config and journal. For a new pair, use the new
package's binary digest and example. For rollback, stop the writers and start
the retained 0.1.29 package.

### Upgrade from 0.1.28-alpha.1

The 0.1.29 server uses the same schema 035, source packs and upload journals
as 0.1.28. Drain workers, stop the service and bridges, and take a
[cold backup](backup-restore.md). Install 0.1.29 alongside 0.1.28 and run
`doctor` with your existing configuration. Keep the 0.1.28 package for
rollback. Stop all writers before switching back to it.

### Upgrade from 0.1.27-alpha.1

Both releases use schema 035 and the same source pack, blob-batch request and
queue report formats. Drain workers, stop the service and bridges, and take a
[cold backup](backup-restore.md). Install 0.1.28 alongside 0.1.27 and run
`doctor` with the existing configuration before starting it. Keep the prior
package and original import/queue journals. To roll back, stop all writers
before switching to the retained 0.1.27 package.

### Upgrade from 0.1.26-alpha.1

Both releases use schema 035 and the same source pack and queue report formats.
Drain workers, stop the service and bridges, and take a
[cold backup](backup-restore.md). Install 0.1.27 alongside 0.1.26 and run
`doctor` with the existing configuration before starting it. Complete local
packs and import journals remain usable across both versions.

### Upgrade from 0.1.25-alpha.1

Both releases use schema 035 and the same retained object and queue report
formats. Drain workers, stop the service and bridges, and take a
[cold backup](backup-restore.md). Install 0.1.26 alongside 0.1.25 and
run `doctor` against the existing configuration before starting it.
The new GitHub flags are optional. An earlier CLI accepts the original
flags when rolling back.

### Upgrade from 0.1.24-alpha.1

Both releases use schema 035. Stop the service and reporting bridges, then
take a [cold backup](backup-restore.md) before installing 0.1.25 alongside
0.1.24. Check the existing configuration with `doctor` and upgrade the
service and reporting bridges together. Existing v8 queue publications
remain readable. The new bridge records timestamps in v11 publications.
If a rollback is needed after accepting a v11 report, retain the upgraded
backup and its journals, then restore the pre-upgrade state before
starting 0.1.24.

### Upgrade from 0.1.23-alpha.1

Both releases use schema 035. Drain workers and stop the service before
taking a [cold backup](backup-restore.md). Install the verified 0.1.24
package alongside 0.1.23, run `doctor` against the existing configuration
and start one service with that state. Keep the previous package and
configuration for rollback. Source upload journals use the same format
and the 0.1.24 client can fall back to individual object requests.

### Upgrade from 0.1.22-alpha.1

Both releases use schema 035. Use the same private configuration, database and
object directory. Stop the service and any workers, take a cold backup, and
install the verified 0.1.23 package alongside 0.1.22. Run `doctor` before
restarting the service. To deploy with Docker, verify and load the
[release image](../examples/control-plane/README.md#install-a-release-image)
and mount the existing state directory and database socket. Keep one running
service for that state. To roll back, stop the service and switch back to the
retained 0.1.22 binary or container configuration.

### Upgrade from 0.1.21-alpha.1

Capture changes you want to retain, drain workspaces and stop the service,
controllers, nodes and GitHub writers. Keep the old installation and take a
verified [cold backup](backup-restore.md) of the schema 034 database, object
store and private deployment state. Verify and install the new archive in a
separate prefix, then migrate while all writers are stopped.

```sh
"$TH_NEW_PREFIX/bin/treeharbor" admin --config "$TH_CONFIG" migrate
"$TH_NEW_PREFIX/bin/treeharbor" doctor --config "$TH_CONFIG"
```

Schema 035 records continuing branch updates when an archived GitHub repository
returns to Standalone. Existing branch updates, ChangeSets and retained history
keep their identities. After restarting, verify a retained change and the
current branch tips before reconnecting nodes with matching binaries.

To roll back, preserve changes accepted after the backup, stop and fence the
new deployment, and restore the schema 034 database and matching objects in
a separate deployment. Reconcile GitHub effects and verify the active authority
before resuming writes.

### Upgrade from 0.1.20-alpha.1

Capture changes you want to retain, drain workspaces and stop the service,
controllers, nodes and GitHub writers. Keep the old installation and take a
verified [cold backup](backup-restore.md) of the schema 033 database, object store
and private deployment state. Verify the new archive and install it in a separate
prefix before migrating.

```sh
"$TH_NEW_PREFIX/bin/treeharbor" admin --config "$TH_CONFIG" migrate
"$TH_NEW_PREFIX/bin/treeharbor" doctor --config "$TH_CONFIG"
```

Schema 034 adds archived GitHub pull provenance. Existing agent runs and accepted
change versions retain their identities. Start the new service, inspect a retained
change and reconnect nodes with matching binaries before admitting new work.
To roll back, preserve changes accepted after the backup, stop and fence the new
deployment, and restore the schema 033 database and matching objects into a
separate deployment. Reconcile external GitHub effects before reconnecting it.

### Upgrade from 0.1.19-alpha.1

Verify the new archive and install it in a separate prefix. Capture and drain
active workspaces, stop the service and its writers, and retain a verified
[cold backup](backup-restore.md). Both versions use schema 033. Run the new
CLI's [`doctor`](doctor.md) against the existing configuration, then start the
new service and reconnect nodes with the matching binaries. On a test
Standalone repository, verify a retained proposal and try the
[live Git export](git-export.md#export-all-branches-from-a-live-standalone-repository)
before admitting another task.

### Upgrade from 0.1.18-alpha.1

Capture any work you want to retain, then drain active workspaces and stop the
service, controllers, nodes and other writers. Keep the old installation and
take a [cold backup](backup-restore.md) of schema 032, the object store and the
private state your deployment needs. Verify the backup before changing the schema.

Verify and install 0.1.19-alpha.1 into a separate prefix. With all writers still
stopped, use the new binary and the existing private service configuration.

```sh
"$TH_NEW_PREFIX/bin/treeharbor" admin --config "$TH_CONFIG" migrate
"$TH_NEW_PREFIX/bin/treeharbor" doctor --config "$TH_CONFIG"
```

Schema 033 adds records for [remote tasks and attempts](remote-tasks-v1.md).
Start the new service and check a retained ChangeSet before reconnecting workers.
Use the new binaries for the service and its nodes. Verify the version and
authority before admitting a new task.

The old binary requires schema 032. To return to 0.1.18-alpha.1 after migration,
preserve changes accepted since the backup, stop and fence the new deployment,
and restore the pre-upgrade database and object state as a separate deployment.
Reconcile any external GitHub effects before reconnecting a Connected repository.

### Upgrade from 0.1.17-alpha.1

Install the verified 0.1.18-alpha.1 archive into a new prefix and keep the
previous installation. The database schema and existing single-image source
registrations are compatible with the new version. Capture and drain active
workspaces and stop the old service and workers before switching commands.
Keep a [cold backup](backup-restore.md) and run the new CLI's
[`doctor`](doctor.md) with the existing private configuration before startup.

To try [multiple source images](experimental-cow.md#multiple-revisions-on-one-source-volume),
register a fresh node and dedicated volume. Keep the old registrations and
protected host helper until their placements have been drained. To return to
0.1.17-alpha.1, capture and drain placements on the new node using 0.1.18-alpha.1
first, stop the new services and select the old prefix and single-image node.
The import pack is disposable and can be removed from the private import state
independently of the accepted source.

[Release package checks](../.github/workflows/release-package.yml) build real static
binaries on Linux x86-64 and ARM64, verify archive checksums, install into a fresh
prefix and execute CLI help/version commands. Synthetic ELF unit tests cover archive
boundaries and installer failure paths; they do not exercise a runtime or database.
Neither suite proves the complete two-agent workflow. Passing package jobs retain development archives as authenticated GitHub Actions
artifacts for seven days. These are not approved releases. The workflow creates no
tag or GitHub release, and generates no SBOM/provenance attestation.

The runtime CI also installs a newly built static package and passes its service
and runtime-init paths to the managed lifecycle tests. The Copy-on-Write drill
uses a protected host copy of the packaged third executable and verifies its
bytes against the archive. It runs two scripted agents on one node, recovers a
killed worker without duplicating containers, and captures/cleans up one task
while the other keeps writing. Those tests keep their
adversarial probe as a separate fixture while executing the installed product
binaries. Full Connected live acceptance remains a separate gate.

After the managed and native lifecycle tests succeed, runtime CI now retains
the exact package and verified image as `treeharbor-runtime-tested-RUN-ATTEMPT-ARCH-SHA`.
It compares the installed executables with the archived bytes and checks their
links to the dependency reports before upload. The `tested-runtime-artifacts.json`
receipt records the archive digest, sidecar digests, actual checkout revision,
workflow run and attempt, and completed test commands. Version 3 adds the
image digest and the matching service binary digest. Version 2 binds the
native-input sidecar and build session. The retained receipt is unsigned until
the separate tagged workflow produces the release attestations.
Earlier version-1 receipts cover their original Cargo-only sidecar set; do not
backfill them with inputs observed during a later rebuild.

The separate offline-package job still produces its own development archive.
Do not substitute it for the runtime-tested archive based on a matching source
SHA. Pull-request runs may test a synthetic merge revision; the receipt records
the checkout, PR head and merge context separately. A release must verify its
selected source, exact archive bytes, remaining acceptance and signed provenance.
