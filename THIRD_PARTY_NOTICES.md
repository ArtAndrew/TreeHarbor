# Third-party notices

TreeHarbor's original code is under Elastic License 2.0. Third-party dependencies
retain their own licenses. They are fetched from the versions in `Cargo.lock`;
this repository does not relicense them under ELv2.

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
