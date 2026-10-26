# Third-party notices

TreeHarbor's original code is under Elastic License 2.0. Third-party dependencies
retain their own licenses. They are fetched from the versions in `Cargo.lock`;
this repository does not relicense them under ELv2.

Static Linux packages also require notices for the Rust standard library,
compiler-builtins, libm and musl. Their unchanged texts and pinned source/hash
inventory are in [static-toolchain notices](third-party/toolchain/README.md).
Cargo.lock alone does not describe these precompiled toolchain components.

The native merge increment uses these unmodified dependencies:

| Component | Upstream | Applicable license text |
|---|---|---|
| diffy 0.5.2, Brandon Williams | [diffy](https://github.com/bmwill/diffy) | MIT OR Apache-2.0; [MIT text](third-party/merge/diffy-MIT.txt) |
| hashbrown 0.17.1, Amanieu d'Antras | [hashbrown](https://github.com/rust-lang/hashbrown) | MIT OR Apache-2.0; [MIT text](third-party/merge/hashbrown-MIT.txt) |
| foldhash 0.2.0, Orson Peters | [foldhash](https://github.com/orlp/foldhash) | [Zlib text](third-party/merge/foldhash-Zlib.txt) |

The Git metadata/export increment adds the following unmodified RustCrypto and
compression dependencies (versions from Cargo.lock). Upstream license/notice files
are preserved verbatim; these files are not licensed as TreeHarbor original code.

| Component | Upstream license and notice files |
|---|---|
| sha1-checked-0.10.0 | [LICENSE-APACHE](third-party/git-bridge/sha1-checked-0.10.0-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/sha1-checked-0.10.0-LICENSE-MIT) |
| sha1-0.10.7 | [LICENSE-APACHE](third-party/git-bridge/sha1-0.10.7-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/sha1-0.10.7-LICENSE-MIT) |
| flate2-1.1.10 | [LICENSE-APACHE](third-party/git-bridge/flate2-1.1.10-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/flate2-1.1.10-LICENSE-MIT) |
| miniz_oxide-0.9.1 | [LICENSE-MIT.md](third-party/git-bridge/miniz_oxide-0.9.1-LICENSE-MIT.md), [LICENSE-ZLIB.md](third-party/git-bridge/miniz_oxide-0.9.1-LICENSE-ZLIB.md), [LICENSE](third-party/git-bridge/miniz_oxide-0.9.1-LICENSE), [LICENSE-APACHE.md](third-party/git-bridge/miniz_oxide-0.9.1-LICENSE-APACHE.md) |
| adler2-2.0.1 | [LICENSE-APACHE](third-party/git-bridge/adler2-2.0.1-LICENSE-APACHE), [LICENSE-0BSD](third-party/git-bridge/adler2-2.0.1-LICENSE-0BSD), [LICENSE-MIT](third-party/git-bridge/adler2-2.0.1-LICENSE-MIT) |
| crc32fast-1.5.3 | [LICENSE-APACHE](third-party/git-bridge/crc32fast-1.5.3-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/crc32fast-1.5.3-LICENSE-MIT) |
| simd-adler32-0.3.10 | [LICENSE.md](third-party/git-bridge/simd-adler32-0.3.10-LICENSE.md) |
| block-buffer-0.10.4 | [LICENSE-APACHE](third-party/git-bridge/block-buffer-0.10.4-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/block-buffer-0.10.4-LICENSE-MIT) |
| const-oid-0.9.6 | [LICENSE-APACHE](third-party/git-bridge/const-oid-0.9.6-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/const-oid-0.9.6-LICENSE-MIT) |
| cpufeatures-0.2.17 | [LICENSE-APACHE](third-party/git-bridge/cpufeatures-0.2.17-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/cpufeatures-0.2.17-LICENSE-MIT) |
| crypto-common-0.1.7 | [LICENSE-APACHE](third-party/git-bridge/crypto-common-0.1.7-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/crypto-common-0.1.7-LICENSE-MIT) |
| digest-0.10.7 | [LICENSE-APACHE](third-party/git-bridge/digest-0.10.7-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/digest-0.10.7-LICENSE-MIT) |
| generic-array-0.14.7 | [LICENSE](third-party/git-bridge/generic-array-0.14.7-LICENSE) |
| version_check-0.9.5 | [LICENSE-APACHE](third-party/git-bridge/version_check-0.9.5-LICENSE-APACHE), [LICENSE-MIT](third-party/git-bridge/version_check-0.9.5-LICENSE-MIT) |

This incremental inventory is not a complete distribution SBOM. The first binary
release must include a complete dependency/license inventory and corresponding
notices for every shipped component. No binary release is available yet.

The GitHub connection increment adds reqwest 0.13.5, jsonwebtoken 11.1.0,
rustls/AWS-LC and their supporting dependencies. Original upstream license and
notice texts for the newly reachable Linux/macOS dependency graph are preserved
in the [GitHub connection inventory](third-party/github/README.md). This includes
the crypto implementation's combined third-party notices; those components are
not relicensed under ELv2. Dependencies for unsupported targets in Cargo.lock are
not a claim that TreeHarbor ships or supports those targets.

The optional webhook HTTPS recipe and its CI test use unmodified Caddy 2.11.7 as
an external executable. Its binary is downloaded separately and is not vendored
or relicensed here. Preserve its archive's upstream Apache-2.0 LICENSE and the
notices for components in any future distribution that bundles it. The recipe
and test are original TreeHarbor code under ELv2; their license does not replace
[Caddy's upstream license](https://github.com/caddyserver/caddy/blob/v2.11.7/LICENSE).

The MCP adapter pins the official rmcp 3.7.0 SDK. Its unmodified Apache-2.0 license
and license/notice texts for newly resolved dependencies (including test-client
and platform-specific packages) are preserved in the
[MCP dependency inventory](third-party/mcp/README.md). TreeHarbor original code
remains under standard ELv2; there is no Apache carveout for original components.
