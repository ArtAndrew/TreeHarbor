# Binary dependency reports (development)

Release and installed-runtime CI build static Linux binaries through
**cargo-auditable 0.7.7**, then read their embedded Cargo dependency sections with
**Syft 1.55.0**. Tool archives are pinned by SHA-256 in
`scripts/install-sbom-tools.sh`; installation is into a new build directory,
without replacing system tools. These tools are build dependencies, not a new
TreeHarbor runtime or programming language.

The reports are **Rust dependency coverage, not a complete native SBOM or signed
build provenance**. Precompiled standard-library internals and native components
bundled by crates still need the full release inventory. No passing report closes
those gates or approves a release.

## What is retained

- `rust-dependencies.cdx.json`: CycloneDX 1.7, recovered Cargo component graph and
  exact binary hashes. Package-declared licenses are joined from Cargo metadata;
  crates.io source archive references use exact Cargo.lock checksums. Legacy slash
  declarations remain named declarations, without an invented SPDX interpretation.
- `input-observations.json`: target, packaging checkout revision, binary and lock
  hashes, observed compiler identity and hashes of available target sysroot inputs.
  Availability is not proof that every archive member was linked. The file keeps
  `native_link_closure_verified` and `build_provenance_verified` false.
- `SHA256SUMS`: integrity of both JSON files. In package CI artifacts this is named
  `SBOM-SHA256SUMS`, beside the archives and their checksums.

File hashes must match the supplied binaries. Unknown or ambiguous package
declarations, unsupported package sources, missing source checksums, foreign
locations and dangling dependency references fail capture. An ordinary binary
without an embedded Cargo graph cannot gain a dependency list merely because a
source lockfile exists. Absolute build directories are removed only from verified
file identities; remaining occurrences cause failure.

The scanner runs on copies of the two binaries with update checks and disk caching
disabled. Raw scanner output and full Cargo metadata stay temporary. The installed
application is not executed to discover its dependencies. Its ordinary runtime
acceptance remains a separate test.

## Reproduce on a disposable Linux builder

Install the pinned Rust toolchain, native build prerequisites and a target standard
library as described in [packaging](release-packaging.md). Choose new absolute
directories outside the source checkout for tools and reports:

```sh
sh scripts/install-sbom-tools.sh x86_64 /tmp/treeharbor-sbom-tools
export PATH="/tmp/treeharbor-sbom-tools:$PATH"
cargo auditable build --locked --release --target x86_64-unknown-linux-musl \
  -p treeharbor-service -p treeharbor-runtime --bins
python3 scripts/capture-rust-sbom.py --target x86_64-unknown-linux-musl \
  --bin-dir target/x86_64-unknown-linux-musl/release \
  --syft /tmp/treeharbor-sbom-tools/syft --output-dir /tmp/treeharbor-sbom-report
cd /tmp/treeharbor-sbom-report
sha256sum --check --strict SHA256SUMS
```

Use `aarch64` and `aarch64-unknown-linux-musl` on an ARM64 builder. The capture
requires a clean committed checkout and refuses an existing output directory.
Cargo metadata collection may fetch locked crate manifests. No repository App
key, agent credential, signing key or extra GitHub permission is needed.

Sources: [cargo-auditable](https://github.com/rust-secure-code/cargo-auditable/tree/v0.7.7)
and [Syft](https://github.com/anchore/syft/tree/v1.55.0). Cargo's native
[SBOM precursor](https://doc.rust-lang.org/cargo/reference/unstable.html#sbom)
is still experimental; this path retains the pinned stable Rust compiler.
