# Binary dependency reports (development)

Release and installed-runtime CI build static Linux binaries through
**cargo-auditable 0.7.7**, then read their embedded Cargo dependency sections with
**Syft 1.55.0**. Tool archives are pinned by SHA-256 in
`scripts/install-sbom-tools.sh`; installation is into a new build directory,
without replacing system tools. These tools run only on builders. TreeHarbor’s runtime remains Rust.

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
- `native-inputs.json`: version-1 link-input observations for the two actual
  executable hashes, an observed source/session, rooted relative input inventories
  and compiler/linker hashes. The GNU linker dependency file is captured during
  the same build, before Rust removes temporary objects. This is not complete
  native component or archive-member attribution; ThinLTO can consume Rust inputs
  before the external linker. All completeness/provenance flags remain false.
- `SHA256SUMS`: integrity of the retained JSON files. In package CI artifacts this is named
  `SBOM-SHA256SUMS`, beside the archives and their checksums.

The package manifest binds the observations file by SHA-256; observations bind
the report by SHA-256. Packaging compares both binary hashes, target and checkout
revision, so replacing a binary after capture refuses the package. These hash
links establish integrity, not a signature or native component completeness.
When native inputs are present, observations also bind that file by SHA-256 and
build session. Packaging requires the referenced report and validates its exact
source/target/session/binary identities. Legacy development packages can omit
native observations; they do not become approved release artifacts.

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
th_session=$(python3 -c 'import uuid; print(uuid.uuid4())')
sh scripts/build-with-link-inputs.sh x86_64-unknown-linux-musl \
  /tmp/treeharbor-canonical-build "$th_session"
python3 scripts/capture-rust-sbom.py --target x86_64-unknown-linux-musl \
  --bin-dir /tmp/treeharbor-canonical-build/target/x86_64-unknown-linux-musl/release \
  --syft /tmp/treeharbor-sbom-tools/syft --output-dir /tmp/treeharbor-sbom-report \
  --native-inputs /tmp/treeharbor-canonical-build/native-reports/native-inputs.json \
  --build-session "$th_session"
cd /tmp/treeharbor-sbom-report
sha256sum --check --strict SHA256SUMS
```

Use `aarch64` and `aarch64-unknown-linux-musl` on an ARM64 builder. The capture
requires a clean committed checkout and refuses an existing output directory.
Cargo metadata collection may fetch locked crate manifests. No repository App
key, agent credential, signing key or extra GitHub permission is needed.

The canonical build helper requires a new external directory and uses a fresh
Cargo target directory, private capture directory and temporary directory. This
prevents cached binaries from standing in for a link whose inputs were never
captured. It supports native Linux x86-64/ARM64 with the reviewed GNU driver and
dependency-file grammar. Unknown paths, response files or unsupported flags refuse
capture. Cargo's reviewed sysroot/target library search paths are cleared only
for child GNU tools; other environment overrides remain unsupported. Raw paths,
arguments and private invocation records stay on the builder. Only normalized
reports enter the artifact, and build context is an observation, not attestation.

Sources: [cargo-auditable](https://github.com/rust-secure-code/cargo-auditable/tree/v0.7.7)
and [Syft](https://github.com/anchore/syft/tree/v1.55.0). Cargo's native
[SBOM precursor](https://doc.rust-lang.org/cargo/reference/unstable.html#sbom)
is still experimental; this path retains the pinned stable Rust compiler.
