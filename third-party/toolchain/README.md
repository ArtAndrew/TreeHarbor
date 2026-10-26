# Pinned static-toolchain notices

These upstream notices accompany development packages built with Rust 1.99.1.
They supplement the Cargo dependency notices; TreeHarbor original code remains
under Elastic License 2.0. This directory does not relicense third-party code.

`manifest.json` records the source URLs and SHA-256 hashes of seven unchanged
notice files. The package builder requires this reviewed set, verifies its hashes
and compares its compiler version with `rust-toolchain.toml`. A compiler upgrade
requires reviewing and updating the inventory and packaging guard.

- Rust `COPYRIGHT`, MIT and Apache texts come from the official Rust 1.99.1
  standard-library distribution. `COPYRIGHT-library.html` is the official
  compiler distribution's generated standard-library notice inventory, including
  its listed third-party materials. Its coverage can exceed a particular binary.
- Compiler-builtins and libm license files come from Rust source commit
  `58cf8f9c35b355199727627d07ae34a4e2846d67`, matching `rustc -Vv` for 1.99.1.
  Preserve the complete texts, including their additional terms and exceptions.
- The musl notice is from the official 1.2.5 source archive. Rust's pinned
  [musl toolchain recipe](https://github.com/rust-lang/rust/blob/58cf8f9c35b355199727627d07ae34a4e2846d67/src/ci/docker/scripts/musl-toolchain.sh)
  selects 1.2.5 and applies patches; its
  [x86-64](https://github.com/rust-lang/rust/blob/58cf8f9c35b355199727627d07ae34a4e2846d67/src/ci/docker/host-x86_64/dist-x86_64-musl/Dockerfile)
  and [AArch64](https://github.com/rust-lang/rust/blob/58cf8f9c35b355199727627d07ae34a4e2846d67/src/ci/docker/host-x86_64/dist-arm-linux-musl/Dockerfile)
  distribution recipes invoke it. This source observation is not a binary
  vulnerability assessment or a declaration that all uses of 1.2.5 are patched.

This is a notice inventory, **not a complete binary SBOM or signed provenance**.
It does not prove which object files were linked, resolve all native components
of Cargo dependencies, or authenticate the supplied executable. Target-specific
build-input inventories and artifact attestations remain release requirements.
