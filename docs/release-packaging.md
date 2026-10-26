# Development release packaging

There is no published installable release yet. This page describes packaging tools
for maintainers and their verification scope. Full Connected onboarding, database
restore, signed provenance and release publication remain gates.

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
and observed toolchain input hashes beside its archives. Their coverage is explicit:
they are not yet the complete native SBOM or provenance required for release.

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
