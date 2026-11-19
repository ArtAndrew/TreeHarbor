# Contributing

Please start with an issue describing a reproducible problem, expected behavior
and workload. Do not include private source code, credentials or customer data.

Changes to object encoding, durability, identity, authority or trust boundaries
need documented behavior and tests that cover the failure boundary. Benchmarks must retain raw
data, environment details, errors and the strongest applicable baseline. Never
replace an external dependency with a mock and report the release gate as passed.

## Build and test

Open or update a pull request to run the full suite on GitHub-hosted runners.
The **Checks** tab shows Rust tests and Clippy on Linux and macOS, PostgreSQL
contracts, and installation and runtime tests with gVisor on Linux x86-64 and
ARM64. Follow the same checks from the branch with `gh pr checks --watch`.

Locally, run the test closest to your change. For a change to the core object
format, for example:

```sh
cargo fmt --all -- --check
cargo test --locked -p treeharbor-core --test contracts
```

To build the CLI from source, install Git, a C compiler, CMake and
[Rust via rustup](https://rustup.rs/). The repository pins the toolchain in
`rust-toolchain.toml`. Signature tests also need OpenSSL.

```sh
cargo build -p treeharbor-service --locked
./target/debug/treeharbor --help
```

For a release, CI on `main` builds and tests the two Linux packages. The
tagged signing workflow uses those exact artifacts. For installation or a
machine-specific test, start with the [signed packages](https://github.com/ArtAndrew/TreeHarbor/releases).
See [local service setup](docs/local-service.md), [PostgreSQL tests](docs/database-development.md)
and [release packaging](docs/release-packaging.md) for details.

## Contribution rights during initial development

TreeHarbor is under ELv2. The project also intends to offer separate commercial
Provider/OEM licensing. Contributors keep their copyright; a sufficient grant
for that licensing must be established before their original code is merged.
A DCO sign-off by itself does not grant arbitrary relicensing rights.

**External code contributions are not being merged until the contribution
agreement and its verification process are published.** Issues, suggestions and
reproducible bug reports are welcome now. Do not sign an agreement by copying a
statement from this document: this is process guidance, not a CLA.

The initial contribution-rights check only accepts commits authored by the
project owner. It fails closed for unknown authors and co-authored commits.
An account match is an administrative check, not evidence of copyright ownership;
maintainers must also check provenance and preserve third-party licenses.

No external contribution should be merged using an administrator bypass. The
planned process must cover all authors, employers when applicable, versioned
terms, authority to grant rights and an auditable record before merge.
