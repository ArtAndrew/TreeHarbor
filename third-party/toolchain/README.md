# Pinned static-toolchain notices

These upstream notices accompany development packages built with Rust 1.99.1.
They supplement the Cargo dependency notices; TreeHarbor original code remains
under Elastic License 2.0. This directory does not relicense third-party code.

`manifest.json` records the source URLs and SHA-256 hashes of ten unchanged
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
- Compiler-rt CRT and LLVM libunwind notices come from Rust's pinned LLVM
  submodule, commit `1b9c0d5ff9bbe7634aead059efe6b11a7eeba145`. The pinned
  [bootstrap builders](https://github.com/rust-lang/rust/blob/58cf8f9c35b355199727627d07ae34a4e2846d67/src/bootstrap/src/core/build_steps/llvm.rs)
  build `crtbegin.c`/`crtend.c` from compiler-rt and libunwind from that submodule.
  Preserve the complete [compiler-rt license](compiler-rt/LICENSE.TXT),
  [credits](compiler-rt/CREDITS.TXT) and [libunwind license](llvm-libunwind/LICENSE.TXT),
  including their exceptions and legacy license sections. These source recipes
  identify why the notices are included; they do not authenticate the precompiled
  archives or establish which archive members entered a particular executable.

This is a notice inventory, **not a complete binary SBOM or signed provenance**.
It does not prove which object files were linked, resolve all native components
of Cargo dependencies, or authenticate the supplied executable. Target-specific
build-input inventories and artifact attestations remain release requirements.
