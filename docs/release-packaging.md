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

## Preparing signed alpha candidates

The manual `release-provenance.yml` workflow is reserved for an existing alpha tag
whose version matches Cargo.toml. It requires the exact source commit and an
independent successful main-push CI run/attempt for that commit. It refuses branch
dispatches, source/workflow mismatches and commits outside main. Ordinary PR CI
keeps read-only permissions.

Each Linux architecture builds, packages and installs once, runs the existing
managed/native runtime tests with the installed binaries, records their exact
hashes, then attests that same archive and its sidecars in the producing job.
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
The producer signs the ZIP too, without changing the runtime-tested archive.
Attach both architectures' archive/checksum, metadata ZIP and signature bundle.
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
both binaries for the same native Linux architecture with the pinned Rust toolchain:

```sh
rustup target add x86_64-unknown-linux-musl
cargo build --locked --release --target x86_64-unknown-linux-musl \
  -p treeharbor-service -p treeharbor-runtime --bins
python3 scripts/package-release.py --target x86_64-unknown-linux-musl \
  --bin-dir target/x86_64-unknown-linux-musl/release --output-dir /tmp/th-package
```

Use a new output directory. The builder refuses replacement of an archive. It
checks both ELF architectures and rejects a dynamic interpreter, includes tracked
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
PostgreSQL, Docker/gVisor, bounded volumes and host access are separate prerequisites.
Removing the CLI symlink and that version directory uninstalls the package; retain
repository state and backups separately.

[Release package checks](../.github/workflows/release-package.yml) build real static
binaries on Linux x86-64 and ARM64, verify archive checksums, install into a fresh
prefix and execute CLI help/version commands. Synthetic ELF unit tests cover archive
boundaries and installer failure paths; they do not exercise a runtime or database.
Neither suite proves the complete two-agent workflow. Passing package jobs retain development archives as authenticated GitHub Actions
artifacts for seven days. These are not approved releases. The workflow creates no
tag or GitHub release, and generates no SBOM/provenance attestation.

The runtime CI also installs a newly built static package and passes its service
and runtime-init paths to the managed lifecycle tests. Those tests keep their
adversarial probe as a separate fixture while executing the installed product
binaries. Full Connected live acceptance remains a separate gate.

After the managed and native lifecycle tests succeed, runtime CI now retains
that exact archive as `treeharbor-runtime-tested-RUN-ATTEMPT-ARCH-SHA`. It compares
the installed executables with the archived bytes and checks their links to the
dependency reports before upload. No rebuild or repackaging occurs at this step.
`tested-runtime-artifacts.json` records the archive digest, sidecar digests,
actual checkout revision, workflow run/attempt and completed test commands.
The receipt is unsigned; it is not build provenance or approval for release.
Receipt version 2 additionally binds the native-input sidecar and build session.
Earlier version-1 receipts cover their original Cargo-only sidecar set; do not
backfill them with inputs observed during a later rebuild.

The separate offline-package job still produces its own development archive.
Do not substitute it for the runtime-tested archive based on a matching source
SHA. Pull-request runs may test a synthetic merge revision; the receipt records
the checkout, PR head and merge context separately. A release must verify its
selected source, exact archive bytes, remaining acceptance and signed provenance.
